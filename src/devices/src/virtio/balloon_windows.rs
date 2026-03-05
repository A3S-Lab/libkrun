use std::io;

use super::{ActivateResult, DeviceState, InterruptTransport, Queue, VirtioDevice};
use polly::event_manager::{EventManager, Subscriber};
use utils::epoll::{EpollEvent, EventSet};
use utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::{ByteValued, Bytes, GuestAddress, GuestMemory, GuestMemoryMmap};
use windows::Win32::System::Memory::{DiscardVirtualMemory, VirtualAlloc, MEM_RESET, PAGE_READWRITE};

const IFQ_INDEX: usize = 0; // Inflate queue
const DFQ_INDEX: usize = 1; // Deflate queue
const STQ_INDEX: usize = 2; // Stats queue
const PHQ_INDEX: usize = 3; // Page-hinting queue
const FRQ_INDEX: usize = 4; // Free page reporting queue

const AVAIL_FEATURES: u64 = (1 << 32) | (1 << 1) | (1 << 5) | (1 << 6);

#[derive(Copy, Clone, Debug, Default)]
#[repr(C, packed)]
pub struct VirtioBalloonConfig {
    num_pages: u32,
    actual: u32,
    free_page_report_cmd_id: u32,
    poison_val: u32,
}

unsafe impl ByteValued for VirtioBalloonConfig {}

pub struct Balloon {
    queues: Vec<Queue>,
    queue_events: Vec<EventFd>,
    activate_evt: EventFd,
    state: DeviceState,
    acked_features: u64,
    config: VirtioBalloonConfig,
}

impl Balloon {
    pub fn new() -> io::Result<Self> {
        let queues = vec![Queue::new(256); 5];
        let mut queue_events = Vec::with_capacity(5);
        for _ in 0..5 {
            queue_events.push(EventFd::new(EFD_NONBLOCK)?);
        }

        Ok(Self {
            queues,
            queue_events,
            activate_evt: EventFd::new(EFD_NONBLOCK)?,
            state: DeviceState::Inactive,
            acked_features: 0,
            config: VirtioBalloonConfig::default(),
        })
    }

    pub fn id(&self) -> &str {
        "virtio_balloon"
    }

    fn process_frq(&mut self) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        let mut have_used = false;

        while let Some(head) = self.queues[FRQ_INDEX].pop(mem) {
            let index = head.index;

            for desc in head.into_iter() {
                if let Ok(host_addr) = mem.get_host_address(desc.addr) {
                    // Use DiscardVirtualMemory (Windows 8.1+) to release pages back to host.
                    // This API tells the OS that the memory contents are no longer needed,
                    // allowing the OS to reclaim the physical pages. The virtual address
                    // range remains valid but will be zero-filled on next access.
                    //
                    // Fallback: If DiscardVirtualMemory fails (e.g., on Windows 7 or older),
                    // use VirtualAlloc with MEM_RESET. This is less efficient as it only
                    // marks pages as "can be discarded" rather than immediately releasing them,
                    // but provides compatible behavior on older Windows versions.
                    unsafe {
                        let slice = std::slice::from_raw_parts_mut(host_addr, desc.len as usize);
                        let result = DiscardVirtualMemory(slice);

                        if result == 0 {
                            // Fallback to VirtualAlloc with MEM_RESET for Windows 7 compatibility
                            let _ = VirtualAlloc(
                                Some(host_addr as *const _),
                                desc.len as usize,
                                MEM_RESET,
                                PAGE_READWRITE,
                            );
                        }
                    }
                }
            }

            have_used = true;
            if let Err(e) = self.queues[FRQ_INDEX].add_used(mem, index, 0) {
                error!("balloon(windows): failed to add used (FRQ): {e:?}");
            }
        }

        have_used
    }

    /// Process page-hinting queue: guest hints that pages can be reclaimed.
    /// Unlike inflate, this is a soft hint - pages remain accessible but can be
    /// reclaimed by the OS if needed. Uses MEM_RESET for lazy reclamation.
    fn process_phq(&mut self) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        let mut have_used = false;

        while let Some(head) = self.queues[PHQ_INDEX].pop(mem) {
            let index = head.index;

            for desc in head.into_iter() {
                // Each PFN is 4 bytes (u32)
                let pfn_count = (desc.len as usize) / 4;
                let mut pfn_bytes = vec![0u8; pfn_count * 4];

                if mem.read_slice(&mut pfn_bytes, desc.addr).is_ok() {
                    // Convert bytes to u32 PFNs (little-endian)
                    for chunk in pfn_bytes.chunks_exact(4) {
                        let pfn = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        let gpa = GuestAddress((pfn as u64) << 12); // PFN to GPA (4KB pages)
                        if let Ok(host_addr) = mem.get_host_address(gpa) {
                            // Use MEM_RESET for soft hinting - pages remain valid but can be reclaimed
                            unsafe {
                                let _ = VirtualAlloc(
                                    Some(host_addr as *const _),
                                    4096,
                                    MEM_RESET,
                                    PAGE_READWRITE,
                                );
                            }
                        }
                    }
                }
            }

            have_used = true;
            if let Err(e) = self.queues[PHQ_INDEX].add_used(mem, index, 0) {
                error!("balloon(windows): failed to add used (PHQ): {e:?}");
            }
        }

        have_used
    }

    /// Process inflate queue: guest is giving memory back to the host.
    /// Each descriptor contains an array of u32 page frame numbers (PFNs).
    fn process_ifq(&mut self) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        let mut have_used = false;

        while let Some(head) = self.queues[IFQ_INDEX].pop(mem) {
            let index = head.index;

            for desc in head.into_iter() {
                // Each PFN is 4 bytes (u32)
                let pfn_count = (desc.len as usize) / 4;
                let mut pfn_bytes = vec![0u8; pfn_count * 4];

                if mem.read_slice(&mut pfn_bytes, desc.addr).is_ok() {
                    // Convert bytes to u32 PFNs (little-endian)
                    for chunk in pfn_bytes.chunks_exact(4) {
                        let pfn = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        let gpa = GuestAddress((pfn as u64) << 12); // PFN to GPA (4KB pages)
                        if let Ok(host_addr) = mem.get_host_address(gpa) {
                            // Same DiscardVirtualMemory + MEM_RESET fallback as deflate queue
                            unsafe {
                                let slice = std::slice::from_raw_parts_mut(host_addr, 4096);
                                let result = DiscardVirtualMemory(slice);

                                if result == 0 {
                                    // Fallback to VirtualAlloc with MEM_RESET for Windows 7 compatibility
                                    let _ = VirtualAlloc(
                                        Some(host_addr as *const _),
                                        4096,
                                        MEM_RESET,
                                        PAGE_READWRITE,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            have_used = true;
            if let Err(e) = self.queues[IFQ_INDEX].add_used(mem, index, 0) {
                error!("balloon(windows): failed to add used (IFQ): {e:?}");
            }
        }

        have_used
    }

    /// Process deflate queue: guest is reclaiming memory from the host.
    /// On Windows, we don't need to do anything special - the guest will
    /// simply start using the pages again, which will cause them to be
    /// faulted back in.
    fn process_dfq(&mut self) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        let mut have_used = false;

        while let Some(head) = self.queues[DFQ_INDEX].pop(mem) {
            let index = head.index;

            // Just acknowledge the deflate request - no action needed on Windows
            // The pages will be faulted back in when the guest accesses them

            have_used = true;
            if let Err(e) = self.queues[DFQ_INDEX].add_used(mem, index, 0) {
                error!("balloon(windows): failed to add used (DFQ): {e:?}");
            }
        }

        have_used
    }

    fn register_runtime_events(&self, event_manager: &mut EventManager) {
        let Ok(self_subscriber) = event_manager.subscriber(self.activate_evt.as_raw_fd()) else {
            return;
        };

        for evt in &self.queue_events {
            let fd = evt.as_raw_fd();
            let event = EpollEvent::new(EventSet::IN, fd as u64);
            if let Err(e) = event_manager.register(fd, event, self_subscriber.clone()) {
                error!("balloon(windows): failed to register queue event {fd}: {e:?}");
            }
        }

        let _ = event_manager.unregister(self.activate_evt.as_raw_fd());
    }
}

impl VirtioDevice for Balloon {
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
        5 // VIRTIO_ID_BALLOON
    }

    fn device_name(&self) -> &str {
        "virtio_balloon_windows"
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

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config_slice = self.config.as_slice();
        let config_len = config_slice.len() as u64;
        if offset >= config_len {
            return;
        }
        if let Some(end) = offset.checked_add(data.len() as u64) {
            let end = std::cmp::min(end, config_len) as usize;
            let src = &config_slice[offset as usize..end];
            data[..src.len()].copy_from_slice(src);
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "balloon(windows): guest attempted to write config (offset={:x}, len={:x})",
            offset,
            data.len()
        );
    }

    fn activate(&mut self, mem: GuestMemoryMmap, interrupt: InterruptTransport) -> ActivateResult {
        self.state = DeviceState::Activated(mem, interrupt);
        self.activate_evt
            .write(1)
            .map_err(|_| super::ActivateError::BadActivate)?;

        let num_pages = self.config.num_pages;
        let actual = self.config.actual;
        debug!(
            "balloon(windows): device activated, num_pages={}, actual={}",
            num_pages, actual
        );
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.state.is_activated()
    }
}

impl Subscriber for Balloon {
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

        let mut raise_irq = false;

        let mut triggered_queue: Option<usize> = None;
        for (queue_index, evt) in self.queue_events.iter().enumerate() {
            if evt.as_raw_fd() != source {
                continue;
            }
            let _ = evt.read();
            triggered_queue = Some(queue_index);
            break;
        }

        if let Some(queue_index) = triggered_queue {
            match queue_index {
                IFQ_INDEX => {
                    debug!("balloon(windows): inflate queue event");
                    raise_irq |= self.process_ifq();
                }
                DFQ_INDEX => {
                    debug!("balloon(windows): deflate queue event");
                    raise_irq |= self.process_dfq();
                }
                STQ_INDEX => {
                    debug!("balloon(windows): stats queue event (ignored)");
                }
                PHQ_INDEX => {
                    debug!("balloon(windows): page-hinting queue event");
                    raise_irq |= self.process_phq();
                }
                FRQ_INDEX => {
                    debug!("balloon(windows): free-page reporting queue event");
                    raise_irq |= self.process_frq();
                }
                _ => {}
            }
        }

        if raise_irq {
            self.state.signal_used_queue();
        }
    }

    fn interest_list(&self) -> Vec<EpollEvent> {
        vec![EpollEvent::new(
            EventSet::IN,
            self.activate_evt.as_raw_fd() as u64,
        )]
    }
}
