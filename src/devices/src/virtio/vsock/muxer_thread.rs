use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use super::super::Queue as VirtQueue;
use super::muxer::{push_packet, MuxerRx, ProxyMap};
use super::muxer_rxq::MuxerRxQ;
use super::proxy::{NewProxyType, Proxy, ProxyRemoval, ProxyUpdate};
use super::tsi_stream::TsiStreamProxy;

use crate::virtio::vsock::defs;
use crate::virtio::vsock::unix::{UnixAcceptorProxy, UnixProxy};
use crate::virtio::InterruptTransport;
use crossbeam_channel::Sender;
use rand::{rng, rngs::ThreadRng, Rng};
use utils::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use utils::eventfd::EventFd;
use vm_memory::GuestMemoryMmap;

pub struct MuxerThread {
    cid: u64,
    pub epoll: Epoll,
    rxq: Arc<Mutex<MuxerRxQ>>,
    proxy_map: ProxyMap,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    interrupt: InterruptTransport,
    reaper_sender: Sender<u64>,
    unix_ipc_port_map: HashMap<u32, (PathBuf, bool)>,
    /// Written by the device thread when the guest replenishes the virtio RX
    /// queue.  MuxerThread registers this fd in its epoll set and uses it to
    /// resume proxies that were paused due to backpressure.
    rx_ready_fd: Arc<EventFd>,
}

impl MuxerThread {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cid: u64,
        epoll: Epoll,
        rxq: Arc<Mutex<MuxerRxQ>>,
        proxy_map: ProxyMap,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
        interrupt: InterruptTransport,
        reaper_sender: Sender<u64>,
        unix_ipc_port_map: HashMap<u32, (PathBuf, bool)>,
        rx_ready_fd: Arc<EventFd>,
    ) -> Self {
        MuxerThread {
            cid,
            epoll,
            rxq,
            proxy_map,
            mem,
            queue,
            interrupt,
            reaper_sender,
            unix_ipc_port_map,
            rx_ready_fd,
        }
    }

    pub fn run(self) {
        thread::Builder::new()
            .name("vsock muxer".into())
            .spawn(|| self.work())
            .unwrap();
    }

    fn send_credit_request(&self, credit_rx: MuxerRx) {
        debug!("send_credit_request");
        // signal_queue is false on the WaitForCredit path, so the caller's
        // should_signal won't fire.  Signal the interrupt here to ensure the
        // guest sees the CreditRequest and responds with a CreditUpdate.
        if push_packet(self.cid, credit_rx, &self.rxq, &self.queue, &self.mem) {
            self.interrupt.signal_used_queue();
        }
    }

    pub fn update_polling(&self, id: u64, fd: RawFd, evset: EventSet) {
        debug!("update_polling id={id} fd={fd:?} evset={evset:?}");
        let _ = self
            .epoll
            .ctl(ControlOperation::Delete, fd, &EpollEvent::default());
        if !evset.is_empty() {
            let _ = self
                .epoll
                .ctl(ControlOperation::Add, fd, &EpollEvent::new(evset, id));
        }
    }

    fn process_proxy_update(&self, id: u64, update: ProxyUpdate, thread_rng: &mut ThreadRng) {
        if let Some(polling) = update.polling {
            self.update_polling(polling.0, polling.1, polling.2);
        }

        if let Some(credit_rx) = update.push_credit_req {
            debug!("send_credit_request");
            self.send_credit_request(credit_rx);
        }

        match update.remove_proxy {
            ProxyRemoval::Keep => {}
            ProxyRemoval::Immediate => {
                warn!("immediately removing proxy: {id}");
                self.proxy_map.write().unwrap().remove(&id);
            }
            ProxyRemoval::Deferred => {
                warn!("deferring proxy removal: {id}");
                if self.reaper_sender.send(id).is_err() {
                    self.proxy_map.write().unwrap().remove(&id);
                }
            }
        }

        let mut should_signal = update.signal_queue;

        if let Some((peer_port, accept_fd, family, proxy_type)) = update.new_proxy {
            let local_port: u32 = thread_rng.random_range(1024..u32::MAX);
            let new_id: u64 = ((peer_port as u64) << 32) | (local_port as u64);
            let new_proxy: Box<dyn Proxy> = match proxy_type {
                NewProxyType::Tcp => Box::new(TsiStreamProxy::new_reverse(
                    new_id,
                    self.cid,
                    id,
                    family,
                    local_port,
                    peer_port,
                    accept_fd,
                    self.mem.clone(),
                    self.queue.clone(),
                    self.rxq.clone(),
                )),
                NewProxyType::Unix => Box::new(UnixProxy::new_reverse(
                    new_id,
                    self.cid,
                    local_port,
                    peer_port,
                    accept_fd,
                    self.mem.clone(),
                    self.queue.clone(),
                    self.rxq.clone(),
                )),
            };
            self.proxy_map
                .write()
                .unwrap()
                .insert(new_id, Mutex::new(new_proxy));
            if let Some(proxy) = self.proxy_map.read().unwrap().get(&new_id) {
                proxy.lock().unwrap().push_op_request();
            };
            should_signal = true;
        }

        if should_signal {
            debug!("signal IRQ");
            self.interrupt.signal_used_queue();
        }
    }

    fn create_lisening_ipc_sockets(&self) {
        for (port, (path, do_listen)) in &self.unix_ipc_port_map {
            if !do_listen {
                continue;
            }
            let id = ((*port as u64) << 32) | (defs::TSI_PROXY_PORT as u64);
            let proxy = match UnixAcceptorProxy::new(id, path, *port) {
                Ok(proxy) => proxy,
                Err(e) => {
                    warn!("Failed to create listening proxy at {path:?}: {e:?}");
                    continue;
                }
            };
            self.proxy_map
                .write()
                .unwrap()
                .insert(id, Mutex::new(Box::new(proxy)));
            if let Some(proxy) = self.proxy_map.read().unwrap().get(&id) {
                self.update_polling(id, proxy.lock().unwrap().as_raw_fd(), EventSet::IN);
            };
        }
    }

    fn work(self) {
        use std::os::unix::io::AsRawFd;

        let mut thread_rng = rng();
        // Proxies removed from epoll because the virtio RX queue had no space.
        // Stored as (proxy_id, raw_fd) so we can re-register them on wake.
        let mut paused_proxies: Vec<(u64, RawFd)> = Vec::new();

        // Register rx_ready_fd in our epoll set.  We use the raw fd value as
        // the event data so we can identify this event in the loop below.
        let rx_ready_raw = self.rx_ready_fd.as_raw_fd();
        let rx_ready_id = rx_ready_raw as u64;
        let _ = self.epoll.ctl(
            ControlOperation::Add,
            rx_ready_raw,
            &EpollEvent::new(EventSet::IN, rx_ready_id),
        );

        self.create_lisening_ipc_sockets();
        loop {
            let mut epoll_events = vec![EpollEvent::new(EventSet::empty(), 0); 32];
            match self
                .epoll
                .wait(epoll_events.len(), -1, epoll_events.as_mut_slice())
            {
                Ok(ev_cnt) => {
                    for ev in &epoll_events[0..ev_cnt] {
                        let id = ev.data();
                        let evset = EventSet::from_bits(ev.events).unwrap();

                        // Wake event: guest replenished the virtio RX queue.
                        if id == rx_ready_id {
                            let _ = self.rx_ready_fd.read();
                            for (pid, pfd) in paused_proxies.drain(..) {
                                if self.proxy_map.read().unwrap().contains_key(&pid) {
                                    self.update_polling(pid, pfd, EventSet::IN);
                                }
                            }
                            continue;
                        }

                        debug!("Event: ev.data={} ev.fd={}", id, ev.fd());

                        let update = self.proxy_map.read().unwrap().get(&id).map(|proxy_lock| {
                            let mut proxy = proxy_lock.lock().unwrap();
                            proxy.process_event(evset)
                        });

                        if let Some(update) = update {
                            let needs_rx_space = update.needs_rx_space;
                            self.process_proxy_update(id, update, &mut thread_rng);

                            if needs_rx_space {
                                // process_proxy_update already removed this proxy
                                // from epoll via update.polling = empty.  Save it
                                // so we can re-register it on the next wake event.
                                if let Some(proxy) = self.proxy_map.read().unwrap().get(&id) {
                                    let fd = proxy.lock().unwrap().as_raw_fd();
                                    paused_proxies.push((id, fd));
                                }
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
}
