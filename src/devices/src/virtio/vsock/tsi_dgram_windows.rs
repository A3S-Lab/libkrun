// TSI DGRAM Proxy for Windows - integrates with vsock muxer
// Implements the Proxy trait for UDP connections

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
use super::tsi_windows::TsiDgramProxyWindows;
use utils::epoll::EventSet;
use vm_memory::GuestMemoryMmap;

/// Windows TSI DGRAM Proxy wrapper
pub struct TsiDgramProxyWindowsWrapper {
    id: u64,
    cid: u64,
    family: u16,
    local_port: u32,
    peer_port: u32,
    control_port: u32,
    dgram_proxy: TsiDgramProxyWindows,
    pub status: ProxyStatus,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    rxq: Arc<Mutex<MuxerRxQ>>,
    pending_sendto: Option<std::net::SocketAddr>,
}

impl TsiDgramProxyWindowsWrapper {
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
        if family != defs::LINUX_AF_INET && family != defs::LINUX_AF_INET6 {
            return Err(ProxyError::InvalidFamily);
        }

        Ok(Self {
            id,
            cid,
            family,
            local_port,
            peer_port,
            control_port,
            dgram_proxy: TsiDgramProxyWindows::new(),
            status: ProxyStatus::Idle,
            mem,
            queue,
            rxq,
            pending_sendto: None,
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

    fn send_response(&mut self, op: u16, result: i32) {
        let mut pkt = VsockPacket::new_op_response_pkt(self.local_port, self.control_port, op);
        pkt.set_op_result(result);
        self.push_packet(pkt);
    }

    fn parse_address(addr_str: &str, family: u16) -> Result<std::net::SocketAddr, ProxyError> {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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

impl AsRawHandle for TsiDgramProxyWindowsWrapper {
    fn as_raw_handle(&self) -> RawHandle {
        std::ptr::null_mut()
    }
}

impl Proxy for TsiDgramProxyWindowsWrapper {
    fn id(&self) -> u64 {
        self.id
    }

    fn status(&self) -> ProxyStatus {
        self.status
    }

    fn connect(&mut self, pkt: &VsockPacket, req: TsiConnectReq) -> ProxyUpdate {
        // DGRAM sockets don't connect, just bind
        let mut update = ProxyUpdate::default();
        let addr_str = String::from_utf8_lossy(&req.addr);

        match Self::parse_address(&addr_str, self.family) {
            Ok(addr) => match self.dgram_proxy.bind(&addr) {
                Ok(_) => {
                    self.status = ProxyStatus::Connected;
                    update.signal_queue = true;
                }
                Err(_) => {
                    self.status = ProxyStatus::Closed;
                    update.remove_proxy = ProxyRemoval::Immediate;
                }
            },
            Err(_) => {
                self.status = ProxyStatus::Closed;
                update.remove_proxy = ProxyRemoval::Immediate;
            }
        }

        update
    }

    fn confirm_connect(&mut self, _pkt: &VsockPacket) -> Option<ProxyUpdate> {
        None
    }

    fn getpeername(&mut self, _pkt: &VsockPacket) {
        let mut rsp = TsiGetnameRsp::default();
        rsp.result = -1;
        let mut rsp_pkt = VsockPacket::new_op_response_pkt(
            self.local_port,
            self.control_port,
            uapi::VSOCK_OP_GETPEERNAME,
        );
        rsp_pkt.set_op_payload(&rsp);
        self.push_packet(rsp_pkt);
    }

    fn sendmsg(&mut self, _pkt: &VsockPacket) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn sendto_addr(&mut self, req: TsiSendtoAddr) -> ProxyUpdate {
        let addr_str = String::from_utf8_lossy(&req.addr);
        if let Ok(addr) = Self::parse_address(&addr_str, self.family) {
            self.pending_sendto = Some(addr);
        }
        ProxyUpdate::default()
    }

    fn sendto_data(&mut self, pkt: &VsockPacket) {
        if let Some(addr) = self.pending_sendto.take() {
            let payload = pkt.data();
            let _ = self.dgram_proxy.sendto(payload, &addr);
        }
    }

    fn listen(
        &mut self,
        _pkt: &VsockPacket,
        _req: TsiListenReq,
        _host_port_map: &Option<HashMap<u16, u16>>,
    ) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn accept(&mut self, _req: TsiAcceptReq) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn update_peer_credit(&mut self, _pkt: &VsockPacket) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn push_op_request(&self) {}

    fn process_op_response(&mut self, _pkt: &VsockPacket) -> ProxyUpdate {
        ProxyUpdate::default()
    }

    fn enqueue_accept(&mut self) {}

    fn push_accept_rsp(&self, _result: i32) {}

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

        if evset.contains(EventSet::IN) && self.status == ProxyStatus::Connected {
            let mut buf = vec![0u8; 65536];
            match self.dgram_proxy.recvfrom(&mut buf) {
                Ok((bytes_read, Some(from_addr))) if bytes_read > 0 => {
                    let mut data_pkt = VsockPacket::new_data_pkt(
                        self.local_port,
                        self.peer_port,
                        &buf[..bytes_read],
                    );
                    self.push_packet(data_pkt);
                    update.signal_queue = true;
                }
                _ => {}
            }
        }

        update
    }
}

#[cfg(target_os = "windows")]
impl std::os::unix::io::AsRawFd for TsiDgramProxyWindowsWrapper {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        -1
    }
}
