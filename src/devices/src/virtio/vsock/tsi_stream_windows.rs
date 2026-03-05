// TSI Stream Proxy for Windows - integrates with vsock muxer
// Implements the Proxy trait for TCP/Named Pipe connections

use std::collections::HashMap;
use std::num::Wrapping;
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::sync::{Arc, Mutex};

use super::super::Queue as VirtQueue;
use super::defs;
use super::defs::uapi;
use super::muxer::{push_packet, MuxerRx};
use super::muxer_rxq::MuxerRxQ;
use super::packet::{
    TsiAcceptReq, TsiConnectReq, TsiGetnameRsp, TsiListenReq, TsiSendtoAddr, VsockPacket,
};
use super::proxy::{
    NewProxyType, Proxy, ProxyError, ProxyRemoval, ProxyStatus, ProxyUpdate, RecvPkt,
};
use super::tsi_windows::{TsiStreamProxyWindows, TsiPipeProxyWindows};
use utils::epoll::EventSet;
use vm_memory::GuestMemoryMmap;

/// Windows TSI Stream Proxy wrapper
pub struct TsiStreamProxyWindowsWrapper {
    id: u64,
    cid: u64,
    family: u16,
    local_port: u32,
    peer_port: u32,
    control_port: u32,
    stream_proxy: Option<TsiStreamProxyWindows>,
    pipe_proxy: Option<TsiPipeProxyWindows>,
    pub status: ProxyStatus,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    rxq: Arc<Mutex<MuxerRxQ>>,
    rx_cnt: Wrapping<u32>,
    tx_cnt: Wrapping<u32>,
    last_tx_cnt_sent: Wrapping<u32>,
    peer_buf_alloc: u32,
    peer_fwd_cnt: Wrapping<u32>,
    push_cnt: Wrapping<u32>,
}

impl TsiStreamProxyWindowsWrapper {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        cid: u64,
        family: u16,
        local_port: u32,
        peer_port: u32,
        control_port: u32,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
        rxq: Arc<Mutex<MuxerRxQ>>,
    ) -> Result<Self, ProxyError> {
        // Determine if this is a TCP or Named Pipe connection
        let (stream_proxy, pipe_proxy) = match family {
            defs::LINUX_AF_INET | defs::LINUX_AF_INET6 => {
                (Some(TsiStreamProxyWindows::new()), None)
            }
            // For now, treat AF_UNIX as Named Pipes on Windows
            defs::LINUX_AF_UNIX => {
                (None, Some(TsiPipeProxyWindows::new()))
            }
            _ => return Err(ProxyError::InvalidFamily),
        };

        Ok(Self {
            id,
            cid,
            family,
            local_port,
            peer_port,
            control_port,
            stream_proxy,
            pipe_proxy,
            status: ProxyStatus::Idle,
            mem,
            queue,
            rxq,
            rx_cnt: Wrapping(0),
            tx_cnt: Wrapping(0),
            last_tx_cnt_sent: Wrapping(0),
            peer_buf_alloc: 0,
            peer_fwd_cnt: Wrapping(0),
            push_cnt: Wrapping(0),
        })
    }

    fn push_packet(&mut self, pkt: VsockPacket) {
        push_packet(
            &self.mem,
            &self.queue,
            &self.rxq,
            pkt,
            self.cid,
            self.local_port,
            self.peer_port,
        );
    }

    fn send_rst(&mut self) {
        let pkt = VsockPacket::new_rst_pkt(self.local_port, self.peer_port);
        self.push_packet(pkt);
    }

    fn send_response(&mut self, op: u16, result: i32) {
        let mut pkt = VsockPacket::new_op_response_pkt(self.local_port, self.control_port, op);
        pkt.set_op_result(result);
        self.push_packet(pkt);
    }

    fn parse_address(addr_str: &str, family: u16) -> Result<std::net::SocketAddr, ProxyError> {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

        // Parse "ip:port" format
        let parts: Vec<&str> = addr_str.rsplitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(ProxyError::InvalidFamily);
        }

        let port: u16 = parts[0].parse().map_err(|_| ProxyError::InvalidFamily)?;
        let ip_str = parts[1];

        let addr = match family {
            defs::LINUX_AF_INET => {
                let ip: Ipv4Addr = ip_str.parse().map_err(|_| ProxyError::InvalidFamily)?;
                SocketAddr::new(IpAddr::V4(ip), port)
            }
            defs::LINUX_AF_INET6 => {
                let ip: Ipv6Addr = ip_str.parse().map_err(|_| ProxyError::InvalidFamily)?;
                SocketAddr::new(IpAddr::V6(ip), port)
            }
            _ => return Err(ProxyError::InvalidFamily),
        };

        Ok(addr)
    }
}

// Windows doesn't have AsRawFd, so we implement AsRawHandle
impl AsRawHandle for TsiStreamProxyWindowsWrapper {
    fn as_raw_handle(&self) -> RawHandle {
        // Return a dummy handle - Windows event handling is different
        // The actual I/O is handled through the proxy objects
        std::ptr::null_mut()
    }
}

impl Proxy for TsiStreamProxyWindowsWrapper {
    fn id(&self) -> u64 {
        self.id
    }

    fn status(&self) -> ProxyStatus {
        self.status
    }

    fn connect(&mut self, pkt: &VsockPacket, req: TsiConnectReq) -> ProxyUpdate {
        let mut update = ProxyUpdate::default();

        // Parse address from request
        let addr_result = if let Some(ref mut proxy) = self.stream_proxy {
            // TCP connection
            let addr_str = String::from_utf8_lossy(&req.addr);
            match Self::parse_address(&addr_str, self.family) {
                Ok(addr) => proxy.process_connect(&super::tsi_windows::stream_proxy::TsiConnectReq {
                    addr: addr_str.to_string(),
                }),
                Err(e) => Err(super::tsi_windows::stream_proxy::ProxyError::InvalidState),
            }
        } else if let Some(ref mut proxy) = self.pipe_proxy {
            // Named Pipe connection
            let pipe_name = String::from_utf8_lossy(&req.addr);
            proxy.connect(&pipe_name)
                .map_err(|_| super::tsi_windows::stream_proxy::ProxyError::InvalidState)
        } else {
            Err(super::tsi_windows::stream_proxy::ProxyError::InvalidState)
        };

        match addr_result {
            Ok(_) => {
                self.status = ProxyStatus::Connecting;
                self.peer_buf_alloc = pkt.buf_alloc();
                self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt());
                update.signal_queue = true;
            }
            Err(_) => {
                self.send_rst();
                self.status = ProxyStatus::Closed;
                update.remove_proxy = ProxyRemoval::Immediate;
            }
        }

        update
    }

    fn confirm_connect(&mut self, pkt: &VsockPacket) -> Option<ProxyUpdate> {
        if self.status != ProxyStatus::Connecting {
            return None;
        }

        // Check if connection is established
        let connected = if let Some(ref mut proxy) = self.stream_proxy {
            proxy.check_connected().unwrap_or(false)
        } else if let Some(ref proxy) = self.pipe_proxy {
            proxy.status() == super::tsi_windows::pipe_proxy::PipeStatus::Connected
        } else {
            false
        };

        if connected {
            self.status = ProxyStatus::Connected;
            let mut response_pkt = VsockPacket::new_connect_response_pkt(
                self.local_port,
                self.peer_port,
            );
            response_pkt.set_buf_alloc(defs::CONN_TX_BUF_SIZE);
            self.push_packet(response_pkt);

            let mut update = ProxyUpdate::default();
            update.signal_queue = true;
            Some(update)
        } else {
            None
        }
    }

    fn getpeername(&mut self, pkt: &VsockPacket) {
        // For Windows, we don't have direct peername support
        // Send a dummy response
        let mut rsp = TsiGetnameRsp::default();
        rsp.result = -1; // EPERM
        let mut rsp_pkt = VsockPacket::new_op_response_pkt(
            self.local_port,
            self.control_port,
            uapi::VSOCK_OP_GETPEERNAME,
        );
        rsp_pkt.set_op_payload(&rsp);
        self.push_packet(rsp_pkt);
    }

    fn sendmsg(&mut self, pkt: &VsockPacket) -> ProxyUpdate {
        let mut update = ProxyUpdate::default();

        if self.status != ProxyStatus::Connected {
            return update;
        }

        // Extract payload from packet
        let payload = pkt.data();
        if payload.is_empty() {
            return update;
        }

        // Send data through proxy
        let result = if let Some(ref mut proxy) = self.stream_proxy {
            proxy.send_data(payload)
        } else if let Some(ref mut proxy) = self.pipe_proxy {
            proxy.send_data(payload)
        } else {
            return update;
        };

        match result {
            Ok(bytes_sent) => {
                self.tx_cnt += Wrapping(bytes_sent as u32);
                // Update credit if needed
                if self.tx_cnt - self.last_tx_cnt_sent >= Wrapping(defs::CONN_CREDIT_UPDATE_THRESHOLD) {
                    let mut credit_pkt = VsockPacket::new_credit_update_pkt(
                        self.local_port,
                        self.peer_port,
                    );
                    credit_pkt.set_buf_alloc(defs::CONN_TX_BUF_SIZE);
                    credit_pkt.set_fwd_cnt(self.tx_cnt.0);
                    self.push_packet(credit_pkt);
                    self.last_tx_cnt_sent = self.tx_cnt;
                    update.signal_queue = true;
                }
            }
            Err(_) => {
                self.send_rst();
                self.status = ProxyStatus::Closed;
                update.remove_proxy = ProxyRemoval::Immediate;
            }
        }

        update
    }

    fn sendto_addr(&mut self, _req: TsiSendtoAddr) -> ProxyUpdate {
        // Not applicable for stream sockets
        ProxyUpdate::default()
    }

    fn sendto_data(&mut self, _pkt: &VsockPacket) {
        // Not applicable for stream sockets
    }

    fn listen(
        &mut self,
        pkt: &VsockPacket,
        req: TsiListenReq,
        _host_port_map: &Option<HashMap<u16, u16>>,
    ) -> ProxyUpdate {
        let mut update = ProxyUpdate::default();

        let result = if let Some(ref mut proxy) = self.stream_proxy {
            // TCP listen
            let addr_str = String::from_utf8_lossy(&req.addr);
            match Self::parse_address(&addr_str, self.family) {
                Ok(addr) => proxy.process_listen(&super::tsi_windows::stream_proxy::TsiListenReq {
                    addr: addr_str.to_string(),
                    backlog: req.backlog,
                }),
                Err(_) => Err(super::tsi_windows::stream_proxy::ProxyError::InvalidState),
            }
        } else if let Some(ref mut proxy) = self.pipe_proxy {
            // Named Pipe listen
            let pipe_name = String::from_utf8_lossy(&req.addr);
            proxy.listen(&pipe_name)
                .map_err(|_| super::tsi_windows::stream_proxy::ProxyError::InvalidState)
        } else {
            Err(super::tsi_windows::stream_proxy::ProxyError::InvalidState)
        };

        match result {
            Ok(_) => {
                self.status = ProxyStatus::Listening;
                self.send_response(uapi::VSOCK_OP_LISTEN, 0);
                update.signal_queue = true;
            }
            Err(_) => {
                self.send_response(uapi::VSOCK_OP_LISTEN, -1);
                self.status = ProxyStatus::Closed;
                update.remove_proxy = ProxyRemoval::Immediate;
            }
        }

        update
    }

    fn accept(&mut self, _req: TsiAcceptReq) -> ProxyUpdate {
        let mut update = ProxyUpdate::default();

        if self.status != ProxyStatus::Listening {
            return update;
        }

        // Try to accept connection
        let result = if let Some(ref mut proxy) = self.stream_proxy {
            proxy.process_accept()
        } else if let Some(ref mut proxy) = self.pipe_proxy {
            proxy.accept().map(|_| None)
        } else {
            return update;
        };

        match result {
            Ok(Some(_)) | Ok(None) => {
                // Connection accepted or would block
                // For now, just signal success
                self.send_response(uapi::VSOCK_OP_ACCEPT, 0);
                update.signal_queue = true;
            }
            Err(_) => {
                self.send_response(uapi::VSOCK_OP_ACCEPT, -1);
            }
        }

        update
    }

    fn update_peer_credit(&mut self, pkt: &VsockPacket) -> ProxyUpdate {
        self.peer_buf_alloc = pkt.buf_alloc();
        self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt());
        ProxyUpdate::default()
    }

    fn push_op_request(&self) {
        // Not implemented for Windows
    }

    fn process_op_response(&mut self, _pkt: &VsockPacket) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn enqueue_accept(&mut self) {
        // Not implemented for Windows
    }

    fn push_accept_rsp(&self, _result: i32) {
        // Not implemented for Windows
    }

    fn shutdown(&mut self, _pkt: &VsockPacket) {
        self.status = ProxyStatus::Closed;
    }

    fn release(&mut self) -> ProxyUpdate {
        self.status = ProxyStatus::Closed;
        let mut update = ProxyUpdate::default();
        update.remove_proxy = ProxyRemoval::Immediate;
        update
    }

    fn process_event(&mut self, evset: EventSet) -> ProxyUpdate {
        let mut update = ProxyUpdate::default();

        // Handle read events
        if evset.contains(EventSet::IN) {
            if self.status == ProxyStatus::Connected {
                // Try to receive data
                let mut buf = vec![0u8; defs::CONN_TX_BUF_SIZE as usize];
                let result = if let Some(ref mut proxy) = self.stream_proxy {
                    proxy.recv_data(&mut buf)
                } else if let Some(ref mut proxy) = self.pipe_proxy {
                    proxy.recv_data(&mut buf)
                } else {
                    return update;
                };

                match result {
                    Ok(bytes_read) if bytes_read > 0 => {
                        self.rx_cnt += Wrapping(bytes_read as u32);
                        // Create data packet
                        let mut data_pkt = VsockPacket::new_data_pkt(
                            self.local_port,
                            self.peer_port,
                            &buf[..bytes_read],
                        );
                        data_pkt.set_buf_alloc(defs::CONN_TX_BUF_SIZE);
                        data_pkt.set_fwd_cnt(self.rx_cnt.0);
                        self.push_packet(data_pkt);
                        update.signal_queue = true;
                    }
                    Ok(0) => {
                        // Connection closed
                        self.send_rst();
                        self.status = ProxyStatus::Closed;
                        update.remove_proxy = ProxyRemoval::Immediate;
                    }
                    Err(_) => {
                        // Error or would block
                    }
                }
            } else if self.status == ProxyStatus::Listening {
                // Try to accept connection
                update = self.accept(TsiAcceptReq::default());
            }
        }

        // Handle write events
        if evset.contains(EventSet::OUT) && self.status == ProxyStatus::Connecting {
            // Connection established
            update = self.confirm_connect(&VsockPacket::default()).unwrap_or_default();
        }

        update
    }
}

// Implement AsRawFd for compatibility (returns dummy value)
#[cfg(target_os = "windows")]
impl std::os::unix::io::AsRawFd for TsiStreamProxyWindowsWrapper {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        -1 // Dummy value for Windows
    }
}
