use std::io;

use polly::event_manager::{EventManager, Subscriber};
use utils::epoll::{EpollEvent, EventSet};
use utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::{Bytes, GuestMemoryMmap};
use windows::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_RNG_ALGORITHM, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

use super::{ActivateError, ActivateResult, DeviceState, InterruptTransport, Queue, VirtioDevice};

const REQ_INDEX: usize = 0;
const NUM_QUEUES: usize = 1;
const QUEUE_SIZE: u16 = 256;
const VIRTIO_F_VERSION_1: u32 = 32;
const VIRTIO_ID_RNG: u32 = 4;

const AVAIL_FEATURES: u64 = 1 << VIRTIO_F_VERSION_1 as u64;

pub struct Rng {
    queues: Vec<Queue>,
    queue_events: Vec<EventFd>,
    activate_evt: EventFd,
    state: DeviceState,
    acked_features: u64,
}

impl Rng {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            queues: vec![Queue::new(QUEUE_SIZE)],
            queue_events: vec![EventFd::new(EFD_NONBLOCK)?],
            activate_evt: EventFd::new(EFD_NONBLOCK)?,
            state: DeviceState::Inactive,
            acked_features: 0,
        })
    }

    pub fn id(&self) -> &str {
        "rng"
    }

    fn register_runtime_events(&self, event_manager: &mut EventManager) {
        let Ok(self_subscriber) = event_manager.subscriber(self.activate_evt.as_raw_fd()) else {
            return;
        };

        let fd = self.queue_events[REQ_INDEX].as_raw_fd();
        let event = EpollEvent::new(EventSet::IN, fd as u64);
        if let Err(e) = event_manager.register(fd, event, self_subscriber.clone()) {
            error!("rng(windows): failed to register queue event {fd}: {e:?}");
        }

        let _ = event_manager.unregister(self.activate_evt.as_raw_fd());
    }

    fn process_req(&mut self) -> bool {
        let mem = match self.state {
            DeviceState::Activated(ref mem, _) => mem,
            DeviceState::Inactive => return false,
        };

        let mut have_used = false;

        while let Some(head) = self.queues[REQ_INDEX].pop(mem) {
            let index = head.index;
            let mut written = 0;

            for desc in head.into_iter() {
                let mut rand_bytes = vec![0u8; desc.len as usize];

                // Use Windows BCryptGenRandom for cryptographically secure random data
                let result = unsafe {
                    BCryptGenRandom(
                        BCRYPT_RNG_ALGORITHM,
                        rand_bytes.as_mut_ptr(),
                        rand_bytes.len() as u32,
                        BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                    )
                };

                if result.is_err() {
                    error!("rng(windows): BCryptGenRandom failed: {:?}", result);
                    self.queues[REQ_INDEX].go_to_previous_position();
                    break;
                }

                if let Err(e) = mem.write_slice(&rand_bytes, desc.addr) {
                    error!("rng(windows): failed to write slice: {e:?}");
                    self.queues[REQ_INDEX].go_to_previous_position();
                    break;
                }

                written += desc.len;
            }

            have_used = true;
            if let Err(e) = self.queues[REQ_INDEX].add_used(mem, index, written) {
                error!("rng(windows): failed to add used elements: {e:?}");
            }
        }

        have_used
    }
}

impl VirtioDevice for Rng {
    fn avail_features(&self) -> u64 {
        AVAIL_FEATURES
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features;
    }

    fn device_type(&self) -> u32 {
        VIRTIO_ID_RNG
    }

    fn device_name(&self) -> &str {
        "rng_windows"
    }

    fn queues(&self) -> &[Queue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [Queue] {
        &mut self.queues
    }

    fn queue_events(&self) -> &[EventFd] {
        &self.queue_events
    }

    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        data.fill(0);
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "rng(windows): guest attempted to write config (offset={:x}, len={:x})",
            offset,
            data.len()
        );
    }

    fn activate(&mut self, mem: GuestMemoryMmap, interrupt: InterruptTransport) -> ActivateResult {
        if self.queues.len() != NUM_QUEUES {
            error!(
                "rng(windows): expected {} queue(s), got {}",
                NUM_QUEUES,
                self.queues.len()
            );
            return Err(ActivateError::BadActivate);
        }

        self.state = DeviceState::Activated(mem, interrupt);
        self.activate_evt
            .write(1)
            .map_err(|_| ActivateError::BadActivate)?;
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.state.is_activated()
    }
}

impl Subscriber for Rng {
    fn process(&mut self, event: &EpollEvent, event_manager: &mut EventManager) {
        let source = event.fd();

        if source == self.activate_evt.as_raw_fd() {
            let _ = self.activate_evt.read();
            self.register_runtime_events(event_manager);
            return;
        }

        if !self.is_activated() {
            return;
        }

        if source == self.queue_events[REQ_INDEX].as_raw_fd() {
            let _ = self.queue_events[REQ_INDEX].read();
            if self.process_req() {
                self.state.signal_used_queue();
            }
        }
    }

    fn interest_list(&self) -> Vec<EpollEvent> {
        vec![EpollEvent::new(
            EventSet::IN,
            self.activate_evt.as_raw_fd() as u64,
        )]
    }
}
