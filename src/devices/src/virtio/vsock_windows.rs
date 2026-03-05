use std::collections::HashMap;
use std::collections::VecDeque;
use std::io;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use bitflags::bitflags;
use polly::event_manager::{EventManager, Subscriber};
use utils::byte_order;
use utils::epoll::{EpollEvent, EventSet};
use utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::{Bytes, GuestMemoryMmap};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileA, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::WaitNamedPipeA;

use super::{ActivateError, ActivateResult, DeviceState, InterruptTransport, Queue, VirtioDevice};

pub const TYPE_VSOCK: u32 = 19;

const RXQ_INDEX: usize = 0;
const TXQ_INDEX: usize = 1;
const EVQ_INDEX: usize = 2;
const NUM_QUEUES: usize = 3;
const QUEUE_SIZE: u16 = 256;

const VIRTIO_F_VERSION_1: u32 = 32;
const VIRTIO_F_IN_ORDER: usize = 35;
const VIRTIO_VSOCK_F_DGRAM: u32 = 3;
const VSOCK_HOST_CID: u64 = 2;

const VSOCK_OP_REQUEST: u16 = 1;
const VSOCK_OP_RESPONSE: u16 = 2;
const VSOCK_OP_RST: u16 = 3;
const VSOCK_OP_SHUTDOWN: u16 = 4;
const VSOCK_OP_RW: u16 = 5;
const VSOCK_OP_CREDIT_UPDATE: u16 = 6;
const VSOCK_OP_CREDIT_REQUEST: u16 = 7;
const VSOCK_FLAGS_SHUTDOWN_RCV: u32 = 1;
const VSOCK_FLAGS_SHUTDOWN_SEND: u32 = 2;
const VSOCK_TYPE_STREAM: u16 = 1;
const VSOCK_TYPE_DGRAM: u16 = 3;

const DEFAULT_BUF_ALLOC: u32 = 256 * 1024;
const MAX_PENDING_RX: usize = 4096; // Increased from 1024
const MAX_PENDING_PER_PORT: usize = 512; // Increased from 128
const MAX_STREAMS: usize = 4096; // Increased from 1024
const CONNECT_TIMEOUT_MS: u64 = 100;
const MAX_RW_PAYLOAD: usize = 64 * 1024;
const MAX_READ_BURST_PER_STREAM: usize = 8;

const AVAIL_FEATURES: u64 = (1 << VIRTIO_F_VERSION_1 as u64)
    | (1 << VIRTIO_F_IN_ORDER as u64)
    | (1 << VIRTIO_VSOCK_F_DGRAM as u64);

bitflags! {
    pub struct TsiFlags: u32 {
        const HIJACK_INET = 1 << 0;
        const HIJACK_UNIX = 1 << 1;
    }
}

impl TsiFlags {
    pub fn tsi_enabled(&self) -> bool {
        !self.is_empty()
    }
}

impl Default for TsiFlags {
    fn default() -> Self {
        TsiFlags::empty()
    }
}

#[derive(Debug)]
pub enum VsockError {
    EventFd(io::Error),
}

pub struct Vsock {
    id: String,
    cid: u64,
    queues: Vec<Queue>,
    queue_events: Vec<EventFd>,
    activate_evt: EventFd,
    state: DeviceState,
    acked_features: u64,
    host_port_map: Option<HashMap<u16, u16>>,
    pipe_port_map: Option<HashMap<u32, String>>, // guest_port -> pipe_name
    streams: HashMap<u32, StreamState>,
    pending_rx: VecDeque<PendingRx>,
    pending_by_guest_port: HashMap<u32, usize>,
}

// Trait to abstract TCP streams and Named Pipes
trait VsockStream: Read + Write + Send {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
}

impl VsockStream for TcpStream {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        TcpStream::set_nonblocking(self, nonblocking)
    }
}

struct NamedPipeStream {
    handle: HANDLE,
}

impl NamedPipeStream {
    fn connect(pipe_name: &str, timeout_ms: u32) -> io::Result<Self> {
        let pipe_path = format!("\\\\.\\pipe\\{}", pipe_name);
        let c_path = std::ffi::CString::new(pipe_path.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid pipe name"))?;

        // Wait for pipe to be available
        unsafe {
            if WaitNamedPipeA(
                windows::core::PCSTR(c_path.as_ptr() as *const u8),
                timeout_ms,
            )
            .is_err()
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Named pipe not available",
                ));
            }
        }

        // Open the pipe
        let handle = unsafe {
            CreateFileA(
                windows::core::PCSTR(c_path.as_ptr() as *const u8),
                0x80000000u32 | 0x40000000u32, // GENERIC_READ | GENERIC_WRITE
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                None,
            )
        };

        match handle {
            Ok(h) if h != INVALID_HANDLE_VALUE => Ok(Self { handle: h }),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for NamedPipeStream {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

// SAFETY: Named pipe handles are Win32 kernel objects. They can be used from
// different threads as long as access is synchronized externally, which is
// guaranteed by the &mut self / &self borrows on Read/Write/VsockStream.
unsafe impl Send for NamedPipeStream {}

impl Read for NamedPipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read = 0u32;
        unsafe {
            ReadFile(self.handle, Some(buf), Some(&mut bytes_read), None)
                .map_err(|e| io::Error::other(format!("ReadFile failed: {}", e)))?;
        }
        Ok(bytes_read as usize)
    }
}

impl Write for NamedPipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written = 0u32;
        unsafe {
            WriteFile(self.handle, Some(buf), Some(&mut bytes_written), None)
                .map_err(|e| io::Error::other(format!("WriteFile failed: {}", e)))?;
        }
        Ok(bytes_written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl VsockStream for NamedPipeStream {
    fn set_nonblocking(&self, _nonblocking: bool) -> io::Result<()> {
        // Named pipes opened with FILE_FLAG_OVERLAPPED are already non-blocking
        Ok(())
    }
}

enum StreamType {
    Tcp(TcpStream),
    NamedPipe(NamedPipeStream),
}

impl Read for StreamType {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            StreamType::Tcp(s) => s.read(buf),
            StreamType::NamedPipe(s) => s.read(buf),
        }
    }
}

impl Write for StreamType {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            StreamType::Tcp(s) => s.write(buf),
            StreamType::NamedPipe(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            StreamType::Tcp(s) => s.flush(),
            StreamType::NamedPipe(s) => s.flush(),
        }
    }
}

struct StreamState {
    stream: StreamType,
    request_hdr: [u8; 44],
    fwd_cnt: u32,
    guest_dst_port: u32,
}

#[derive(Debug, Clone)]
struct PendingRx {
    hdr: [u8; 44],
    payload: Vec<u8>,
}

impl Vsock {
    pub fn new(
        cid: u64,
        host_port_map: Option<HashMap<u16, u16>>,
        unix_ipc_port_map: Option<HashMap<u32, (PathBuf, bool)>>,
        _tsi_flags: TsiFlags,
    ) -> Result<Self, VsockError> {
        let queues = vec![Queue::new(QUEUE_SIZE); NUM_QUEUES];
        let mut queue_events = Vec::with_capacity(NUM_QUEUES);
        for _ in 0..NUM_QUEUES {
            queue_events.push(EventFd::new(EFD_NONBLOCK).map_err(VsockError::EventFd)?);
        }

        // Convert Unix socket paths to Named Pipe names
        let pipe_port_map = unix_ipc_port_map.map(|map| {
            map.into_iter()
                .map(|(port, (path, _))| {
                    // Extract pipe name from path (e.g., /tmp/foo.sock -> foo)
                    let pipe_name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("default")
                        .to_string();
                    (port, pipe_name)
                })
                .collect()
        });

        Ok(Self {
            id: "vsock".to_string(),
            cid,
            queues,
            queue_events,
            activate_evt: EventFd::new(EFD_NONBLOCK).map_err(VsockError::EventFd)?,
            state: DeviceState::Inactive,
            acked_features: 0,
            host_port_map,
            pipe_port_map,
            streams: HashMap::new(),
            pending_rx: VecDeque::new(),
            pending_by_guest_port: HashMap::new(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn cid(&self) -> u64 {
        self.cid
    }

    fn register_runtime_events(&self, event_manager: &mut EventManager) {
        let Ok(self_subscriber) = event_manager.subscriber(self.activate_evt.as_raw_fd()) else {
            return;
        };

        for eventfd in &self.queue_events {
            let fd = eventfd.as_raw_fd();
            let event = EpollEvent::new(EventSet::IN, fd as u64);
            if let Err(e) = event_manager.register(fd, event, self_subscriber.clone()) {
                error!("vsock(windows): failed to register queue event {fd}: {e:?}");
            }
        }

        let _ = event_manager.unregister(self.activate_evt.as_raw_fd());
    }

    fn read_hdr(mem: &GuestMemoryMmap, addr: vm_memory::GuestAddress) -> Option<[u8; 44]> {
        let mut hdr = [0_u8; 44];
        mem.read_slice(&mut hdr, addr).ok()?;
        Some(hdr)
    }

    fn write_hdr(mem: &GuestMemoryMmap, addr: vm_memory::GuestAddress, hdr: &[u8; 44]) -> bool {
        mem.write_slice(hdr, addr).is_ok()
    }

    fn hdr_u16(hdr: &[u8; 44], off: usize) -> u16 {
        byte_order::read_le_u16(&hdr[off..off + 2])
    }

    fn hdr_u32(hdr: &[u8; 44], off: usize) -> u32 {
        byte_order::read_le_u32(&hdr[off..off + 4])
    }

    fn hdr_u64(hdr: &[u8; 44], off: usize) -> u64 {
        byte_order::read_le_u64(&hdr[off..off + 8])
    }

    fn set_u16(hdr: &mut [u8; 44], off: usize, value: u16) {
        byte_order::write_le_u16(&mut hdr[off..off + 2], value)
    }

    fn set_u32(hdr: &mut [u8; 44], off: usize, value: u32) {
        byte_order::write_le_u32(&mut hdr[off..off + 4], value)
    }

    fn set_u64(hdr: &mut [u8; 44], off: usize, value: u64) {
        byte_order::write_le_u64(&mut hdr[off..off + 8], value)
    }

    fn make_response_hdr(
        &self,
        incoming_hdr: &[u8; 44],
        op: u16,
        len: u32,
        buf_alloc: u32,
        fwd_cnt: u32,
    ) -> [u8; 44] {
        let mut hdr = [0_u8; 44];

        let src_cid = Self::hdr_u64(incoming_hdr, 0);
        let src_port = Self::hdr_u32(incoming_hdr, 16);
        let dst_port = Self::hdr_u32(incoming_hdr, 20);
        let ty = Self::hdr_u16(incoming_hdr, 28);

        Self::set_u64(&mut hdr, 0, VSOCK_HOST_CID);
        Self::set_u64(&mut hdr, 8, src_cid);
        Self::set_u32(&mut hdr, 16, dst_port);
        Self::set_u32(&mut hdr, 20, src_port);
        Self::set_u32(&mut hdr, 24, len);
        Self::set_u16(&mut hdr, 28, ty);
        Self::set_u16(&mut hdr, 30, op);
        Self::set_u32(&mut hdr, 32, 0);
        Self::set_u32(&mut hdr, 36, buf_alloc);
        Self::set_u32(&mut hdr, 40, fwd_cnt);
        hdr
    }

    fn make_rst_response(&self, incoming_hdr: &[u8; 44]) -> [u8; 44] {
        self.make_response_hdr(incoming_hdr, VSOCK_OP_RST, 0, 0, 0)
    }

    fn credit_for_hdr(&self, incoming_hdr: &[u8; 44]) -> (u32, u32) {
        let guest_src_port = Self::hdr_u32(incoming_hdr, 16);
        if let Some(state) = self.streams.get(&guest_src_port) {
            (DEFAULT_BUF_ALLOC, state.fwd_cnt)
        } else {
            (DEFAULT_BUF_ALLOC, 0)
        }
    }

    fn queue_response(&mut self, incoming_hdr: &[u8; 44], op: u16, payload: Vec<u8>) {
        let (buf_alloc, fwd_cnt) = self.credit_for_hdr(incoming_hdr);
        let hdr =
            self.make_response_hdr(incoming_hdr, op, payload.len() as u32, buf_alloc, fwd_cnt);
        let guest_port = Self::hdr_u32(&hdr, 20);

        let per_port_pending = self
            .pending_by_guest_port
            .get(&guest_port)
            .copied()
            .unwrap_or(0);
        if per_port_pending >= MAX_PENDING_PER_PORT {
            warn!(
                "vsock(windows): pending RX per-port full (port={}, max={}), sending RST for op={}",
                guest_port, MAX_PENDING_PER_PORT, op
            );
            // Send RST to signal backpressure to the peer
            if op != VSOCK_OP_RST && op != VSOCK_OP_SHUTDOWN {
                self.queue_rst(&hdr);
            }
            return;
        }

        if self.pending_rx.len() >= MAX_PENDING_RX {
            warn!(
                "vsock(windows): pending RX queue full ({}), sending RST for op={}",
                MAX_PENDING_RX, op
            );
            // Send RST to signal backpressure to the peer
            if op != VSOCK_OP_RST && op != VSOCK_OP_SHUTDOWN {
                self.queue_rst(&hdr);
            }
            return;
        }
        self.pending_rx.push_back(PendingRx { hdr, payload });
        self.pending_by_guest_port
            .entry(guest_port)
            .and_modify(|v| *v += 1)
            .or_insert(1);
    }

    fn queue_credit_update(&mut self, incoming_hdr: &[u8; 44]) {
        self.queue_response(incoming_hdr, VSOCK_OP_CREDIT_UPDATE, Vec::new());
    }

    fn purge_pending_for_guest_port(&mut self, guest_port: u32) {
        let mut removed = 0usize;
        self.pending_rx.retain(|pending| {
            let keep = Self::hdr_u32(&pending.hdr, 20) != guest_port;
            if !keep {
                removed = removed.saturating_add(1);
            }
            keep
        });

        if removed > 0 {
            if let Some(v) = self.pending_by_guest_port.get_mut(&guest_port) {
                *v = v.saturating_sub(removed);
                if *v == 0 {
                    self.pending_by_guest_port.remove(&guest_port);
                }
            }
        }
    }

    fn close_stream_and_rst(&mut self, src_port: u32, incoming_hdr: &[u8; 44]) {
        self.streams.remove(&src_port);
        self.purge_pending_for_guest_port(src_port);
        self.queue_rst(incoming_hdr);
    }

    fn queue_rst(&mut self, incoming_hdr: &[u8; 44]) {
        let hdr = self.make_rst_response(incoming_hdr);
        let guest_port = Self::hdr_u32(&hdr, 20);

        let per_port_pending = self
            .pending_by_guest_port
            .get(&guest_port)
            .copied()
            .unwrap_or(0);
        if per_port_pending >= MAX_PENDING_PER_PORT {
            warn!(
                "vsock(windows): pending RX per-port full (port={}, max={}), dropping RST",
                guest_port, MAX_PENDING_PER_PORT
            );
            return;
        }

        if self.pending_rx.len() >= MAX_PENDING_RX {
            warn!(
                "vsock(windows): pending RX queue full ({}), dropping RST response",
                MAX_PENDING_RX
            );
            return;
        }
        self.pending_rx.push_back(PendingRx {
            hdr,
            payload: Vec::new(),
        });
        self.pending_by_guest_port
            .entry(guest_port)
            .and_modify(|v| *v += 1)
            .or_insert(1);
    }

    fn harvest_stream_reads(&mut self) {
        let mut responses: Vec<([u8; 44], Vec<u8>)> = Vec::new();
        let mut closed_ports: Vec<u32> = Vec::new();
        let mut closed_hdrs: Vec<[u8; 44]> = Vec::new();

        for (port, state) in &mut self.streams {
            let mut should_close = false;
            for _ in 0..MAX_READ_BURST_PER_STREAM {
                let mut rx_buf = [0_u8; 4096];
                match state.stream.read(&mut rx_buf) {
                    Ok(n) if n > 0 => {
                        responses.push((state.request_hdr, rx_buf[..n].to_vec()));
                    }
                    Ok(_) => {
                        should_close = true;
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        break;
                    }
                    Err(_) => {
                        should_close = true;
                        break;
                    }
                }
            }

            if should_close {
                closed_ports.push(*port);
                closed_hdrs.push(state.request_hdr);
            }
        }

        for port in closed_ports {
            self.streams.remove(&port);
        }

        for hdr in closed_hdrs {
            self.queue_rst(&hdr);
        }

        for (hdr, payload) in responses {
            self.queue_response(&hdr, VSOCK_OP_RW, payload);
        }
    }

    fn host_socket_addr(&self, guest_dst_port: u32) -> Option<SocketAddr> {
        let host_port_map = self.host_port_map.as_ref()?;
        let host_port = *host_port_map.get(&(guest_dst_port as u16))?;
        Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), host_port))
    }

    fn packet_targets_host(&self, hdr: &[u8; 44]) -> bool {
        Self::hdr_u64(hdr, 8) == VSOCK_HOST_CID
    }

    fn packet_from_guest_cid(&self, hdr: &[u8; 44]) -> bool {
        Self::hdr_u64(hdr, 0) == self.cid
    }

    fn op_requires_zero_len(op: u16) -> bool {
        matches!(
            op,
            VSOCK_OP_REQUEST
                | VSOCK_OP_RESPONSE
                | VSOCK_OP_RST
                | VSOCK_OP_SHUTDOWN
                | VSOCK_OP_CREDIT_UPDATE
                | VSOCK_OP_CREDIT_REQUEST
        )
    }

    fn process_tx_queue(&mut self) -> bool {
        let mem = match self.state {
            DeviceState::Activated(ref mem, _) => mem.clone(),
            DeviceState::Inactive => return false,
        };

        let mut used_any = false;
        while let Some(head) = self.queues[TXQ_INDEX].pop(&mem) {
            let head_index = head.index;
            let mut iter = head.into_iter();
            if let Some(hdr_desc) = iter.next() {
                if let Some(hdr) = Self::read_hdr(&mem, hdr_desc.addr) {
                    if !self.packet_targets_host(&hdr) || !self.packet_from_guest_cid(&hdr) {
                        self.queue_rst(&hdr);
                        if let Err(e) = self.queues[TXQ_INDEX].add_used(&mem, head_index, 0) {
                            error!("vsock(windows): failed to add TX used entry: {e:?}");
                        } else {
                            used_any = true;
                        }
                        continue;
                    }

                    let op = Self::hdr_u16(&hdr, 30);
                    let src_port = Self::hdr_u32(&hdr, 16);
                    let dst_port = Self::hdr_u32(&hdr, 20);
                    let data_len = Self::hdr_u32(&hdr, 24) as usize;
                    let pkt_type = Self::hdr_u16(&hdr, 28);

                    if Self::op_requires_zero_len(op) && data_len != 0 {
                        self.queue_rst(&hdr);
                        continue;
                    }

                    match op {
                        VSOCK_OP_REQUEST => {
                            if src_port == 0 || dst_port == 0 {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if data_len != 0 {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if pkt_type != VSOCK_TYPE_STREAM && pkt_type != VSOCK_TYPE_DGRAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            // Current Windows backend only supports stream-like forwarding.
                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            // Reconnect on same guest source port replaces the old stream.
                            if self.streams.contains_key(&src_port) {
                                self.streams.remove(&src_port);
                                self.purge_pending_for_guest_port(src_port);
                            }

                            if self.streams.len() >= MAX_STREAMS {
                                warn!(
                                    "vsock(windows): stream table full (max={MAX_STREAMS}), rejecting src_port={src_port}"
                                );
                                self.queue_rst(&hdr);
                                continue;
                            }

                            // Try Named Pipe first, then TCP
                            let stream_result = if let Some(pipe_map) = &self.pipe_port_map {
                                if let Some(pipe_name) = pipe_map.get(&dst_port) {
                                    // Connect to Named Pipe
                                    match NamedPipeStream::connect(pipe_name, CONNECT_TIMEOUT_MS as u32) {
                                        Ok(pipe) => {
                                            let _ = pipe.set_nonblocking(true);
                                            Some(StreamType::NamedPipe(pipe))
                                        }
                                        Err(e) => {
                                            debug!("vsock(windows): Named Pipe connect failed for {}: {}", pipe_name, e);
                                            None
                                        }
                                    }
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                            let stream_result = stream_result.or_else(|| {
                                // Fallback to TCP
                                if let Some(addr) = self.host_socket_addr(dst_port) {
                                    match TcpStream::connect_timeout(
                                        &addr,
                                        Duration::from_millis(CONNECT_TIMEOUT_MS),
                                    ) {
                                        Ok(stream) => {
                                            let _ = stream.set_nonblocking(true);
                                            let _ = stream.set_nodelay(true);
                                            Some(StreamType::Tcp(stream))
                                        }
                                        Err(_) => None,
                                    }
                                } else {
                                    None
                                }
                            });

                            if let Some(stream) = stream_result {
                                self.streams.insert(
                                    src_port,
                                    StreamState {
                                        stream,
                                        request_hdr: hdr,
                                        fwd_cnt: 0,
                                        guest_dst_port: dst_port,
                                    },
                                );
                                self.queue_response(&hdr, VSOCK_OP_RESPONSE, Vec::new());
                                self.queue_credit_update(&hdr);
                            } else {
                                self.queue_rst(&hdr);
                            }
                        }
                        VSOCK_OP_RW => {
                            if src_port == 0 {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if data_len > MAX_RW_PAYLOAD {
                                self.close_stream_and_rst(src_port, &hdr);
                                continue;
                            }

                            if let Some(state) = self.streams.get_mut(&src_port) {
                                if state.guest_dst_port != dst_port {
                                    self.close_stream_and_rst(src_port, &hdr);
                                    continue;
                                }

                                if data_len > 0 {
                                    let Some(buf_desc) = iter.next() else {
                                        self.close_stream_and_rst(src_port, &hdr);
                                        continue;
                                    };
                                    if buf_desc.len < data_len as u32 {
                                        self.close_stream_and_rst(src_port, &hdr);
                                        continue;
                                    }

                                    let mut payload = vec![0_u8; data_len];
                                    if mem.read_slice(&mut payload, buf_desc.addr).is_err() {
                                        self.close_stream_and_rst(src_port, &hdr);
                                        continue;
                                    }

                                    match state.stream.write_all(&payload) {
                                        Ok(()) => {
                                            state.fwd_cnt =
                                                state.fwd_cnt.saturating_add(payload.len() as u32);
                                        }
                                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                                        Err(_) => {
                                            self.close_stream_and_rst(src_port, &hdr);
                                            continue;
                                        }
                                    }
                                }
                                self.harvest_stream_reads();
                                self.queue_credit_update(&hdr);
                            } else {
                                self.queue_rst(&hdr);
                            }
                        }
                        VSOCK_OP_CREDIT_UPDATE => {
                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if let Some(state) = self.streams.get(&src_port) {
                                if state.guest_dst_port != dst_port {
                                    self.queue_rst(&hdr);
                                    continue;
                                }
                                // For now we only track host-side consumed bytes.
                            } else {
                                self.queue_rst(&hdr);
                            }
                        }
                        VSOCK_OP_CREDIT_REQUEST => {
                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if let Some(state) = self.streams.get(&src_port) {
                                if state.guest_dst_port != dst_port {
                                    self.queue_rst(&hdr);
                                    continue;
                                }
                                self.queue_credit_update(&hdr);
                            } else {
                                self.queue_rst(&hdr);
                            }
                        }
                        VSOCK_OP_SHUTDOWN | VSOCK_OP_RST => {
                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            let flags = Self::hdr_u32(&hdr, 32);
                            if flags & (VSOCK_FLAGS_SHUTDOWN_RCV | VSOCK_FLAGS_SHUTDOWN_SEND) != 0
                                || op == VSOCK_OP_RST
                            {
                                if let Some(state) = self.streams.get(&src_port) {
                                    if state.guest_dst_port != dst_port {
                                        self.queue_rst(&hdr);
                                        continue;
                                    }
                                }
                                self.close_stream_and_rst(src_port, &hdr);
                            } else {
                                self.queue_credit_update(&hdr);
                            }
                        }
                        _ => self.queue_rst(&hdr),
                    }
                }
            }

            if let Err(e) = self.queues[TXQ_INDEX].add_used(&mem, head_index, 0) {
                error!("vsock(windows): failed to add TX used entry: {e:?}");
            } else {
                used_any = true;
            }
        }
        used_any
    }

    fn process_rx_queue(&mut self) -> bool {
        let mem = match self.state {
            DeviceState::Activated(ref mem, _) => mem.clone(),
            DeviceState::Inactive => return false,
        };

        let mut used_any = false;
        while let Some(head) = self.queues[RXQ_INDEX].pop(&mem) {
            let head_index = head.index;
            let Some(pending) = self.pending_rx.front().cloned() else {
                self.queues[RXQ_INDEX].undo_pop();
                break;
            };

            let mut used = 0_u32;
            let mut iter = head.into_iter();
            if let Some(hdr_desc) = iter.next() {
                if hdr_desc.is_write_only() && Self::write_hdr(&mem, hdr_desc.addr, &pending.hdr) {
                    used = 44;

                    if !pending.payload.is_empty() {
                        let Some(buf_desc) = iter.next() else {
                            self.queues[RXQ_INDEX].undo_pop();
                            break;
                        };
                        if !buf_desc.is_write_only() || buf_desc.len < pending.payload.len() as u32
                        {
                            self.queues[RXQ_INDEX].undo_pop();
                            break;
                        }
                        if mem.write_slice(&pending.payload, buf_desc.addr).is_err() {
                            self.queues[RXQ_INDEX].undo_pop();
                            break;
                        }
                        used = used.saturating_add(pending.payload.len() as u32);
                    }

                    if let Some(sent) = self.pending_rx.pop_front() {
                        let sent_guest_port = Self::hdr_u32(&sent.hdr, 20);
                        if let Some(v) = self.pending_by_guest_port.get_mut(&sent_guest_port) {
                            *v = v.saturating_sub(1);
                            if *v == 0 {
                                self.pending_by_guest_port.remove(&sent_guest_port);
                            }
                        }
                    }
                }
            }

            if let Err(e) = self.queues[RXQ_INDEX].add_used(&mem, head_index, used) {
                error!("vsock(windows): failed to add RX used entry: {e:?}");
            } else {
                used_any = true;
            }
        }

        used_any
    }

    fn process_evq_queue(&mut self) -> bool {
        let mem = match self.state {
            DeviceState::Activated(ref mem, _) => mem.clone(),
            DeviceState::Inactive => return false,
        };

        let mut used_any = false;
        while let Some(head) = self.queues[EVQ_INDEX].pop(&mem) {
            if let Err(e) = self.queues[EVQ_INDEX].add_used(&mem, head.index, 0) {
                error!("vsock(windows): failed to add EVQ used entry: {e:?}");
            } else {
                used_any = true;
            }
        }

        used_any
    }
}

impl VirtioDevice for Vsock {
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
        TYPE_VSOCK
    }

    fn device_name(&self) -> &str {
        "vsock_windows"
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
        match offset {
            0 if data.len() == 8 => byte_order::write_le_u64(data, self.cid()),
            0 if data.len() == 4 => {
                byte_order::write_le_u32(data, (self.cid() & 0xffff_ffff) as u32)
            }
            4 if data.len() == 4 => {
                byte_order::write_le_u32(data, ((self.cid() >> 32) & 0xffff_ffff) as u32)
            }
            _ => {
                warn!(
                    "virtio-vsock(windows) invalid config read: offset={}, len={}",
                    offset,
                    data.len()
                );
            }
        }
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "virtio-vsock(windows) write config not supported: offset={offset:x}, len={}",
            data.len()
        );
    }

    fn activate(&mut self, mem: GuestMemoryMmap, interrupt: InterruptTransport) -> ActivateResult {
        if self.queues.len() != NUM_QUEUES {
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

impl Subscriber for Vsock {
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
        if source == self.queue_events[RXQ_INDEX].as_raw_fd() {
            let _ = self.queue_events[RXQ_INDEX].read();
            self.harvest_stream_reads();
            raise_irq |= self.process_rx_queue();
        } else if source == self.queue_events[TXQ_INDEX].as_raw_fd() {
            let _ = self.queue_events[TXQ_INDEX].read();
            raise_irq |= self.process_tx_queue();
            self.harvest_stream_reads();
            raise_irq |= self.process_rx_queue();
        } else if source == self.queue_events[EVQ_INDEX].as_raw_fd() {
            let _ = self.queue_events[EVQ_INDEX].read();
            self.harvest_stream_reads();
            raise_irq |= self.process_evq_queue();
            raise_irq |= self.process_rx_queue();
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
