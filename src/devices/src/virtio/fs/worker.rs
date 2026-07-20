#[cfg(target_os = "macos")]
use crossbeam_channel::Sender;
#[cfg(target_os = "macos")]
use utils::worker_message::WorkerMessage;

#[cfg(not(target_os = "windows"))]
use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicI32;
use std::sync::Arc;
use std::thread;
#[cfg(target_os = "windows")]
use std::{fs::OpenOptions, io::Write};

use utils::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use utils::eventfd::EventFd;
use vm_memory::GuestMemoryMmap;

use super::super::{FsError, Queue};
use super::defs::{HPQ_INDEX, REQ_INDEX};
use super::descriptor_utils::{Reader, Writer};
use super::passthrough::{self, PassthroughFs};
use super::server::Server;
use crate::virtio::{InterruptTransport, VirtioShmRegion};

#[cfg(target_os = "windows")]
fn fs_worker_debug_log(message: impl AsRef<str>) {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WINDOWS_VERBOSE_DEBUG")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }) {
        return;
    }
    let message = message.as_ref();
    eprintln!("[VIRTIOFS-WORKER] {message}");
    for path in [
        r"C:\Users\18770\.a3s\libkrun-virtiofs-device.log",
        r"D:\code\libkrun\tmp_virtiofs_device.log",
    ] {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{message}");
        }
    }
}

pub struct FsWorker {
    queues: Vec<Queue>,
    queue_evts: Vec<EventFd>,
    interrupt: InterruptTransport,
    mem: GuestMemoryMmap,
    shm_region: Option<VirtioShmRegion>,
    server: Server<PassthroughFs>,
    stop_fd: EventFd,
    exit_code: Arc<AtomicI32>,
    #[cfg(target_os = "macos")]
    map_sender: Option<Sender<WorkerMessage>>,
}

impl FsWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        queues: Vec<Queue>,
        queue_evts: Vec<EventFd>,
        interrupt: InterruptTransport,
        mem: GuestMemoryMmap,
        shm_region: Option<VirtioShmRegion>,
        passthrough_fs: Arc<PassthroughFs>,
        stop_fd: EventFd,
        exit_code: Arc<AtomicI32>,
        #[cfg(target_os = "macos")] map_sender: Option<Sender<WorkerMessage>>,
    ) -> Self {
        Self {
            queues,
            queue_evts,
            interrupt,
            mem,
            shm_region,
            server: Server::new(passthrough_fs),
            stop_fd,
            exit_code,
            #[cfg(target_os = "macos")]
            map_sender,
        }
    }

    pub fn run(self) -> thread::JoinHandle<()> {
        thread::Builder::new()
            .name("fs worker".into())
            .spawn(|| self.work())
            .unwrap()
    }

    fn work(mut self) {
        let virtq_hpq_ev_fd = self.queue_evts[HPQ_INDEX].as_raw_fd();
        let virtq_req_ev_fd = self.queue_evts[REQ_INDEX].as_raw_fd();
        let stop_ev_fd = self.stop_fd.as_raw_fd();

        #[cfg(target_os = "windows")]
        fs_worker_debug_log(format!(
            "FsWorker::work hpq_fd={} req_fd={} stop_fd={}",
            virtq_hpq_ev_fd, virtq_req_ev_fd, stop_ev_fd
        ));

        let epoll = Epoll::new().unwrap();

        let _ = epoll.ctl(
            ControlOperation::Add,
            virtq_hpq_ev_fd,
            &EpollEvent::new(EventSet::IN, virtq_hpq_ev_fd as u64),
        );
        let _ = epoll.ctl(
            ControlOperation::Add,
            virtq_req_ev_fd,
            &EpollEvent::new(EventSet::IN, virtq_req_ev_fd as u64),
        );
        let _ = epoll.ctl(
            ControlOperation::Add,
            stop_ev_fd,
            &EpollEvent::new(EventSet::IN, stop_ev_fd as u64),
        );

        loop {
            let mut epoll_events = vec![EpollEvent::new(EventSet::empty(), 0); 32];
            match epoll.wait(epoll_events.len(), -1, epoll_events.as_mut_slice()) {
                Ok(ev_cnt) => {
                    #[cfg(target_os = "windows")]
                    if ev_cnt > 0 {
                        fs_worker_debug_log(format!("FsWorker::epoll_wake count={}", ev_cnt));
                    }
                    for event in &epoll_events[0..ev_cnt] {
                        let source = event.fd();
                        let event_set = event.event_set();
                        #[cfg(target_os = "windows")]
                        fs_worker_debug_log(format!(
                            "FsWorker::epoll_event source={} events=0x{:x}",
                            source,
                            event.events()
                        ));
                        match event_set {
                            EventSet::IN if source == virtq_hpq_ev_fd => {
                                self.handle_event(HPQ_INDEX);
                            }
                            EventSet::IN if source == virtq_req_ev_fd => {
                                self.handle_event(REQ_INDEX);
                            }
                            EventSet::IN if source == stop_ev_fd => {
                                debug!("stopping worker thread");
                                let _ = self.stop_fd.read();
                                return;
                            }
                            _ => {
                                log::warn!(
                                    "Received unknown event: {event_set:?} from fd: {source:?}"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("failed to consume muxer epoll event: {e}");
                }
            }
        }
    }

    fn handle_event(&mut self, queue_index: usize) {
        debug!("Fs: queue event: {queue_index}");
        #[cfg(target_os = "windows")]
        fs_worker_debug_log(format!("FsWorker::handle_event queue={}", queue_index));
        if let Err(e) = self.queue_evts[queue_index].read() {
            error!("Failed to get queue event: {e:?}");
            #[cfg(target_os = "windows")]
            fs_worker_debug_log(format!(
                "FsWorker::handle_event_read_failed queue={} err={:?}",
                queue_index, e
            ));
        }

        loop {
            self.queues[queue_index]
                .disable_notification(&self.mem)
                .unwrap();

            self.process_queue(queue_index);

            if !self.queues[queue_index]
                .enable_notification(&self.mem)
                .unwrap()
            {
                break;
            }
        }
    }

    fn process_queue(&mut self, queue_index: usize) {
        let queue = &mut self.queues[queue_index];
        while let Some(head) = queue.pop(&self.mem) {
            #[cfg(target_os = "windows")]
            fs_worker_debug_log(format!(
                "FsWorker::process_queue queue={} head_index={}",
                queue_index, head.index
            ));
            let reader = Reader::new(&self.mem, head.clone())
                .map_err(FsError::QueueReader)
                .unwrap();
            let writer = Writer::new(&self.mem, head.clone())
                .map_err(FsError::QueueWriter)
                .unwrap();

            if let Err(e) = self.server.handle_message(
                reader,
                writer,
                &self.shm_region,
                &self.exit_code,
                #[cfg(target_os = "macos")]
                &self.map_sender,
            ) {
                error!("error handling message: {e:?}");
                #[cfg(target_os = "windows")]
                fs_worker_debug_log(format!(
                    "FsWorker::handle_message_failed queue={} head_index={} err={:?}",
                    queue_index, head.index, e
                ));
            }

            if let Err(e) = queue.add_used(&self.mem, head.index, 0) {
                error!("failed to add used elements to the queue: {e:?}");
                #[cfg(target_os = "windows")]
                fs_worker_debug_log(format!(
                    "FsWorker::add_used_failed queue={} head_index={} err={:?}",
                    queue_index, head.index, e
                ));
            }

            if queue.needs_notification(&self.mem).unwrap() {
                #[cfg(target_os = "windows")]
                fs_worker_debug_log(format!(
                    "FsWorker::needs_notification queue={} head_index={}",
                    queue_index, head.index
                ));
                self.interrupt.signal_used_queue();
            }
        }
    }
}
