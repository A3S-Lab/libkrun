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
}
