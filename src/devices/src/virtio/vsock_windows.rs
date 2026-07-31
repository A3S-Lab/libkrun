use std::collections::HashMap;
use std::collections::VecDeque;
use std::io;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bitflags::bitflags;
use polly::event_manager::{EventManager, Subscriber};
use utils::byte_order;
use utils::epoll::{EpollEvent, EventSet};
use utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::{Address, Bytes, GuestMemoryMmap};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileA, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::Pipes::{PeekNamedPipe, WaitNamedPipeA};

use super::linux_errno::linux_errno_raw;
use super::{
    ActivateError, ActivateResult, DescriptorChain, DeviceState, InterruptTransport, Queue,
    VirtioDevice,
};

pub const TYPE_VSOCK: u32 = 19;

const RXQ_INDEX: usize = 0;
const TXQ_INDEX: usize = 1;
const EVQ_INDEX: usize = 2;
const NUM_QUEUES: usize = 3;
const QUEUE_SIZE: u16 = 256;

const VIRTIO_F_VERSION_1: u32 = 32;
const VIRTIO_F_IN_ORDER: usize = 35;
#[allow(dead_code)] // Reserved for future DGRAM implementation
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
const TSI_LISTEN_PORT: u32 = 1029;
const TSI_LISTEN_REQUEST_LEN: usize = 4 * 4 + 128;

const DEFAULT_BUF_ALLOC: u32 = 256 * 1024;
// Keep stream credit advertisement aligned with the existing muxer/TSI paths.
const STREAM_BUF_ALLOC: u32 = 8 * 1024 * 1024;
const MAX_PENDING_RX: usize = 4096; // Increased from 1024
const MAX_PENDING_PER_PORT: usize = 512; // Increased from 128
const MAX_STREAMS: usize = 4096; // Increased from 1024
                                 // The Windows guest may reconnect to the same named pipe control channel very
                                 // quickly (for example after a failed probe or early reconnect). Give the host
                                 // pipe server enough time to tear down and recreate the next instance.
const CONNECT_TIMEOUT_MS: u64 = 1_500;
const CONTROL_STREAM_READ_GRACE_MS: u64 = 200;
const CONTROL_STREAM_POLL_INTERVAL_MS: u64 = 100;
const CONTROL_STREAM_POLL_INTERVAL_MAX_MS: u64 = 1_000;
const CONTROL_STREAM_KEEPALIVE_POLL_MS: u64 = 250;
const CONTROL_STREAM_KEEPALIVE_POLLS: usize = 480;
const MAX_RW_PAYLOAD: usize = 64 * 1024;
const MAX_READ_BURST_PER_STREAM: usize = 8;
// Linux AF_VSOCK may expose a receive descriptor smaller than the 4 KiB
// Windows pipe read buffer after reserving its virtio header and skb metadata.
// Keep each stream packet below that descriptor while preserving byte-stream
// ordering across consecutive packets.
const MAX_STREAM_RX_CHUNK_BYTES: usize = 3 * 1024;

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
    control_aliases: HashMap<u32, u32>, // alias guest src port -> canonical guest src port
    dgram_sockets: HashMap<u32, UdpSocket>, // guest_port -> UDP socket
    pending_rx: VecDeque<PendingRx>,
    pending_by_guest_port: HashMap<u32, usize>,
    connect_timeout_ms: u64, // Configurable connection timeout
    defer_control_rx: bool,
    tsi_flags: TsiFlags,
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
    pipe_path: String,
}

impl NamedPipeStream {
    fn connect(pipe_name: &str, timeout_ms: u32) -> io::Result<Self> {
        let pipe_path = format!("\\\\.\\pipe\\{}", pipe_name);
        vsock_debug_log(format!(
            "named pipe connect begin pipe={} timeout_ms={}",
            pipe_path, timeout_ms
        ));
        let c_path = std::ffi::CString::new(pipe_path.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid pipe name"))?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1) as u64);
        let mut last_err = io::Error::new(io::ErrorKind::TimedOut, "Named pipe not available");

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                vsock_debug_log(format!(
                    "named pipe wait failed pipe={} error={}",
                    pipe_path, last_err
                ));
                return Err(last_err);
            }

            let wait_ms = remaining.as_millis().clamp(1, u32::MAX as u128) as u32;
            let wait_ready = unsafe {
                WaitNamedPipeA(windows::core::PCSTR(c_path.as_ptr() as *const u8), wait_ms).is_ok()
            };
            if !wait_ready {
                last_err = io::Error::new(io::ErrorKind::TimedOut, "Named pipe not available");
                thread::sleep(Duration::from_millis(10));
                continue;
            }

            let handle = unsafe {
                CreateFileA(
                    windows::core::PCSTR(c_path.as_ptr() as *const u8),
                    0x80000000u32 | 0x40000000u32, // GENERIC_READ | GENERIC_WRITE
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            };

            match handle {
                Ok(h) if h != INVALID_HANDLE_VALUE => {
                    vsock_debug_log(format!("named pipe connect success pipe={}", pipe_path));
                    return Ok(Self {
                        handle: h,
                        pipe_path,
                    });
                }
                _ => {
                    last_err = io::Error::last_os_error();
                    if remaining <= Duration::from_millis(25) {
                        vsock_debug_log(format!(
                            "named pipe connect failed pipe={} error={}",
                            pipe_path, last_err
                        ));
                        return Err(last_err);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
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
        let mut bytes_available = 0u32;
        unsafe {
            PeekNamedPipe(self.handle, None, 0, None, Some(&mut bytes_available), None)
                .map_err(|e| io::Error::other(format!("PeekNamedPipe failed: {}", e)))?;
        }

        if bytes_available == 0 {
            vsock_control_trace_log(|| {
                format!("named pipe read would_block pipe={}", self.pipe_path)
            });
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Named pipe has no pending data",
            ));
        }

        vsock_control_trace_log(|| {
            format!(
                "named pipe read begin pipe={} available={} request={}",
                self.pipe_path,
                bytes_available,
                buf.len()
            )
        });

        let mut bytes_read = 0u32;
        unsafe {
            ReadFile(self.handle, Some(buf), Some(&mut bytes_read), None)
                .map_err(|e| io::Error::other(format!("ReadFile failed: {}", e)))?;
        }
        vsock_control_trace_log(|| {
            format!(
                "named pipe read ok pipe={} bytes={}",
                self.pipe_path, bytes_read
            )
        });
        Ok(bytes_read as usize)
    }
}

impl Write for NamedPipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        vsock_control_trace_log(|| {
            format!(
                "named pipe write begin pipe={} bytes={}",
                self.pipe_path,
                buf.len()
            )
        });
        let mut bytes_written = 0u32;
        unsafe {
            WriteFile(self.handle, Some(buf), Some(&mut bytes_written), None)
                .map_err(|e| io::Error::other(format!("WriteFile failed: {}", e)))?;
        }
        vsock_control_trace_log(|| {
            format!(
                "named pipe write ok pipe={} bytes={}",
                self.pipe_path, bytes_written
            )
        });
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
    fwd_cnt: u32, // Bytes we've forwarded to the stream
    guest_dst_port: u32,
    peer_buf_alloc: u32, // Peer's buffer allocation
    peer_fwd_cnt: u32,   // Peer's forward count (bytes consumed)
    tx_cnt: u32,         // Bytes we've sent to peer
    response_pending: bool,
    control_read_ready_at: Option<Instant>,
    control_poll_interval_ms: u64,
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
        tsi_flags: TsiFlags,
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

        if let Some(pipe_port_map) = &pipe_port_map {
            vsock_debug_log(format!(
                "vsock init cid={} pipe_port_map={:?} host_port_map_present={}",
                cid,
                pipe_port_map,
                host_port_map.is_some()
            ));
        } else {
            vsock_debug_log(format!(
                "vsock init cid={} pipe_port_map=<none> host_port_map_present={}",
                cid,
                host_port_map.is_some()
            ));
        }

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
            control_aliases: HashMap::new(),
            dgram_sockets: HashMap::new(),
            pending_rx: VecDeque::new(),
            pending_by_guest_port: HashMap::new(),
            connect_timeout_ms: CONNECT_TIMEOUT_MS, // Use default timeout
            defer_control_rx: false,
            tsi_flags,
        })
    }

    /// Set the connection timeout in milliseconds.
    /// Default is 100ms. Increase for slower services.
    pub fn set_connect_timeout(&mut self, timeout_ms: u64) {
        self.connect_timeout_ms = timeout_ms;
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

        if self
            .pipe_port_map
            .as_ref()
            .is_some_and(|m| m.contains_key(&4093))
        {
            let fds: Vec<i32> = self
                .queue_events
                .iter()
                .map(|evt| evt.as_raw_fd())
                .collect();
            vsock_debug_log(format!(
                "register_runtime_events activate_fd={} queue_fds={:?}",
                self.activate_evt.as_raw_fd(),
                fds
            ));
        }

        for eventfd in &self.queue_events {
            let fd = eventfd.as_raw_fd();
            let event = EpollEvent::new(EventSet::IN, fd as u64);
            if let Err(e) = event_manager.register(fd, event, self_subscriber.clone()) {
                error!("vsock(windows): failed to register queue event {fd}: {e:?}");
            }
        }

        let _ = event_manager.unregister(self.activate_evt.as_raw_fd());
    }

    fn schedule_control_pipe_harvest(&self, src_port: u32) {
        self.schedule_control_pipe_harvest_after(src_port, CONTROL_STREAM_READ_GRACE_MS);
    }

    fn schedule_control_pipe_keepalive_polling(&self, src_port: u32) {
        let Ok(wake_evt) = self.queue_events[RXQ_INDEX].try_clone() else {
            vsock_debug_log(format!(
                "schedule_control_pipe_keepalive_polling clone_failed src_port={}",
                src_port
            ));
            return;
        };

        thread::spawn(move || {
            for tick in 0..CONTROL_STREAM_KEEPALIVE_POLLS {
                thread::sleep(Duration::from_millis(CONTROL_STREAM_KEEPALIVE_POLL_MS));
                let _ = wake_evt.write(1);
                vsock_control_trace_log(|| {
                    format!(
                        "schedule_control_pipe_keepalive_poll wake src_port={} tick={} delay_ms={}",
                        src_port,
                        tick + 1,
                        CONTROL_STREAM_KEEPALIVE_POLL_MS
                    )
                });
            }
        });
    }

    fn schedule_control_pipe_harvest_after(&self, src_port: u32, delay_ms: u64) {
        let Ok(wake_evt) = self.queue_events[RXQ_INDEX].try_clone() else {
            vsock_debug_log(format!(
                "schedule_control_pipe_harvest clone_failed src_port={} delay_ms={}",
                src_port, delay_ms
            ));
            return;
        };

        thread::spawn(move || {
            thread::sleep(Duration::from_millis(delay_ms));
            let _ = wake_evt.write(1);
            vsock_control_trace_log(|| {
                format!(
                    "schedule_control_pipe_harvest wake src_port={} delay_ms={}",
                    src_port, delay_ms
                )
            });
        });
    }

    fn schedule_control_response_irq_retry(&self, src_port: u32) {
        let DeviceState::Activated(_, interrupt) = &self.state else {
            vsock_debug_log(format!(
                "schedule_control_response_irq_retry inactive src_port={}",
                src_port
            ));
            return;
        };

        let interrupt = interrupt.clone();
        thread::spawn(move || {
            // Exponential backoff: 50ms, 100ms, 200ms, 400ms, 800ms, 1600ms, 3200ms, 6400ms
            // Total coverage ~12.7s — long enough for the guest vCPU to be schedulable even
            // when the 2-connection (alias) path delays virtio interrupt processing.
            for delay_ms in [50_u64, 100, 200, 400, 800, 1600, 3200, 6400] {
                thread::sleep(Duration::from_millis(delay_ms));
                interrupt.signal_used_queue();
                vsock_debug_log(format!(
                    "schedule_control_response_irq_retry kick src_port={} delay_ms={}",
                    src_port, delay_ms
                ));
            }

            // Keep retrying after the initial backoff window. Some Windows runs
            // do not service the first control-response interrupt for 17s+.
            for _ in 0..40 {
                thread::sleep(Duration::from_secs(1));
                interrupt.signal_used_queue();
                vsock_debug_log(format!(
                    "schedule_control_response_irq_retry kick src_port={} delay_ms=1000",
                    src_port
                ));
            }
        });
    }

    fn read_hdr(mem: &GuestMemoryMmap, addr: vm_memory::GuestAddress) -> Option<[u8; 44]> {
        let mut hdr = [0_u8; 44];
        mem.read_slice(&mut hdr, addr).ok()?;
        Some(hdr)
    }

    fn write_hdr(mem: &GuestMemoryMmap, addr: vm_memory::GuestAddress, hdr: &[u8; 44]) -> bool {
        mem.write_slice(hdr, addr).is_ok()
    }

    #[inline]
    fn hdr_u16(hdr: &[u8; 44], off: usize) -> u16 {
        byte_order::read_le_u16(&hdr[off..off + 2])
    }

    #[inline]
    fn hdr_u32(hdr: &[u8; 44], off: usize) -> u32 {
        byte_order::read_le_u32(&hdr[off..off + 4])
    }

    #[inline]
    fn hdr_u64(hdr: &[u8; 44], off: usize) -> u64 {
        byte_order::read_le_u64(&hdr[off..off + 8])
    }

    #[inline]
    fn set_u16(hdr: &mut [u8; 44], off: usize, value: u16) {
        byte_order::write_le_u16(&mut hdr[off..off + 2], value)
    }

    #[inline]
    fn set_u32(hdr: &mut [u8; 44], off: usize, value: u32) {
        byte_order::write_le_u32(&mut hdr[off..off + 4], value)
    }

    #[inline]
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

    fn should_trace_control_hdr(hdr: &[u8; 44]) -> bool {
        matches!(Self::hdr_u32(hdr, 16), 4089 | 4093)
            || matches!(Self::hdr_u32(hdr, 20), 4089 | 4093)
    }

    fn describe_control_frame(payload: &[u8]) -> Option<String> {
        if payload.len() < 9 {
            return None;
        }

        let kind = payload[0];
        let stream_id = u32::from_be_bytes([payload[1], payload[2], payload[3], payload[4]]);
        let frame_len = u32::from_be_bytes([payload[5], payload[6], payload[7], payload[8]]);
        let kind_name = match kind {
            1 => "FRAME_OPEN",
            2 => "FRAME_OPEN_ACK",
            3 => "FRAME_DATA",
            4 => "FRAME_CLOSE",
            _ => "FRAME_UNKNOWN",
        };

        Some(format!(
            "{} kind={} stream_id={} frame_len={} chunk_len={}",
            kind_name,
            kind,
            stream_id,
            frame_len,
            payload.len()
        ))
    }

    fn trace_control_payload(direction: &str, src_port: u32, dst_port: u32, payload: &[u8]) {
        if !vsock_control_trace_enabled() {
            return;
        }
        if let Some(frame) = Self::describe_control_frame(payload) {
            vsock_debug_log(format!(
                "control_payload {} src_port={} dst_port={} {}",
                direction, src_port, dst_port, frame
            ));
        } else {
            vsock_debug_log(format!(
                "control_payload {} src_port={} dst_port={} chunk_len={} short_frame",
                direction,
                src_port,
                dst_port,
                payload.len()
            ));
        }
    }

    fn trace_control_tx(&self, context: &str, hdr: &[u8; 44]) {
        if Self::should_trace_control_hdr(hdr) {
            vsock_debug_log(format!(
                "{} op={} src_port={} dst_port={} type={} len={} flags={} buf_alloc={} fwd_cnt={}",
                context,
                Self::hdr_u16(hdr, 30),
                Self::hdr_u32(hdr, 16),
                Self::hdr_u32(hdr, 20),
                Self::hdr_u16(hdr, 28),
                Self::hdr_u32(hdr, 24),
                Self::hdr_u32(hdr, 32),
                Self::hdr_u32(hdr, 36),
                Self::hdr_u32(hdr, 40)
            ));
        }
    }

    fn queue_rst_with_reason(&mut self, incoming_hdr: &[u8; 44], reason: &str) {
        if Self::should_trace_control_hdr(incoming_hdr) {
            vsock_debug_log(format!(
                "queue_rst reason={} incoming_op={} src_port={} dst_port={} type={} len={} flags={} buf_alloc={} fwd_cnt={}",
                reason,
                Self::hdr_u16(incoming_hdr, 30),
                Self::hdr_u32(incoming_hdr, 16),
                Self::hdr_u32(incoming_hdr, 20),
                Self::hdr_u16(incoming_hdr, 28),
                Self::hdr_u32(incoming_hdr, 24),
                Self::hdr_u32(incoming_hdr, 32),
                Self::hdr_u32(incoming_hdr, 36),
                Self::hdr_u32(incoming_hdr, 40)
            ));
        }
        self.queue_rst(incoming_hdr);
    }

    fn credit_for_hdr(&self, incoming_hdr: &[u8; 44]) -> (u32, u32) {
        if Self::hdr_u16(incoming_hdr, 28) != VSOCK_TYPE_STREAM {
            return (0, 0);
        }

        let guest_src_port = Self::hdr_u32(incoming_hdr, 16);
        if let Some(state) = self.streams.get(&guest_src_port) {
            (STREAM_BUF_ALLOC, state.fwd_cnt)
        } else {
            (STREAM_BUF_ALLOC, 0)
        }
    }

    fn queue_response(&mut self, incoming_hdr: &[u8; 44], op: u16, payload: Vec<u8>) {
        let (mut buf_alloc, fwd_cnt) = self.credit_for_hdr(incoming_hdr);
        if op == VSOCK_OP_RESPONSE && Self::hdr_u32(incoming_hdr, 20) == 4093 {
            buf_alloc = Self::hdr_u32(incoming_hdr, 36).max(DEFAULT_BUF_ALLOC);
        }
        let hdr =
            self.make_response_hdr(incoming_hdr, op, payload.len() as u32, buf_alloc, fwd_cnt);
        let guest_port = Self::hdr_u32(&hdr, 20);
        let defer_control_response = op == VSOCK_OP_RESPONSE
            && Self::hdr_u32(incoming_hdr, 20) == 4093
            && std::env::var("LIBKRUN_WINDOWS_VSOCK_DEFER_CONTROL_RESPONSE").as_deref() == Ok("1");

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
        if defer_control_response {
            self.defer_control_rx = true;
            vsock_debug_log(format!(
                "defer_control_rx queued src_port={} dst_port={} pending_rx_len={}",
                Self::hdr_u32(&hdr, 16),
                Self::hdr_u32(&hdr, 20),
                self.pending_rx.len()
            ));
        }
        self.trace_control_tx("queue_response_hdr", &hdr);
        if Self::should_trace_control_hdr(&hdr) {
            vsock_debug_log(format!(
                "queue_response op={} src_port={} dst_port={} pending_rx_len={} guest_port_pending={}",
                Self::hdr_u16(&hdr, 30),
                Self::hdr_u32(&hdr, 16),
                Self::hdr_u32(&hdr, 20),
                self.pending_rx.len(),
                self.pending_by_guest_port.get(&guest_port).copied().unwrap_or(0)
            ));
        }
    }

    fn queue_credit_update(&mut self, incoming_hdr: &[u8; 44]) {
        self.queue_response(incoming_hdr, VSOCK_OP_CREDIT_UPDATE, Vec::new());
    }

    fn queue_stream_data(&mut self, incoming_hdr: &[u8; 44], payload: Vec<u8>) {
        for chunk in payload.chunks(MAX_STREAM_RX_CHUNK_BYTES) {
            self.queue_response(incoming_hdr, VSOCK_OP_RW, chunk.to_vec());
        }
    }

    fn reject_unsupported_tsi_listen(&mut self, incoming_hdr: &[u8; 44]) -> bool {
        if !self.tsi_flags.tsi_enabled()
            || Self::hdr_u64(incoming_hdr, 8) != VSOCK_HOST_CID
            || Self::hdr_u32(incoming_hdr, 20) != TSI_LISTEN_PORT
            || Self::hdr_u16(incoming_hdr, 28) != VSOCK_TYPE_DGRAM
            || Self::hdr_u16(incoming_hdr, 30) != VSOCK_OP_RW
        {
            return false;
        }

        let request_len = Self::hdr_u32(incoming_hdr, 24) as usize;
        if request_len != TSI_LISTEN_REQUEST_LEN {
            warn!(
                "vsock(windows): rejecting TSI_LISTEN request with unexpected length \
                 (actual={request_len}, expected={TSI_LISTEN_REQUEST_LEN})"
            );
        }

        // Published ports on Windows are implemented by guest-init's named-pipe
        // bridge, not by a host-side TSI listener. The guest kernel only falls
        // back to its already-bound native INET socket for selected Linux errors.
        // Returning -EPERM lets that socket enter listen() so the bridge can
        // connect to it over guest loopback.
        let result = -linux_errno_raw(libc::EPERM);
        self.queue_response(incoming_hdr, VSOCK_OP_RW, result.to_le_bytes().to_vec());
        true
    }

    fn read_tx_payload(
        mem: &GuestMemoryMmap,
        hdr_desc: &DescriptorChain<'_>,
        payload_descs: &[DescriptorChain<'_>],
        data_len: usize,
    ) -> io::Result<Vec<u8>> {
        let mut payload = vec![0u8; data_len];
        let mut copied = 0usize;

        let inline_cap = hdr_desc.len.saturating_sub(44) as usize;
        if inline_cap > 0 {
            let inline_len = inline_cap.min(data_len);
            let inline_addr = hdr_desc.addr.checked_add(44).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "inline addr overflow")
            })?;
            mem.read_slice(&mut payload[..inline_len], inline_addr)
                .map_err(|e| io::Error::other(format!("inline payload read failed: {e}")))?;
            copied = inline_len;
        }

        for desc in payload_descs {
            if copied >= data_len {
                break;
            }
            if desc.is_write_only() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "payload descriptor unexpectedly write-only",
                ));
            }

            let chunk_len = (data_len - copied).min(desc.len as usize);
            if chunk_len == 0 {
                continue;
            }

            mem.read_slice(&mut payload[copied..copied + chunk_len], desc.addr)
                .map_err(|e| io::Error::other(format!("payload read failed: {e}")))?;
            copied += chunk_len;
        }

        if copied != data_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("payload truncated: copied {copied} of {data_len} bytes"),
            ));
        }

        Ok(payload)
    }

    /// Calculate available TX credit for a stream.
    ///
    /// Credit-based flow control ensures we don't overflow the peer's receive buffer.
    /// The formula is: available_credit = peer_buf_alloc - (tx_cnt - peer_fwd_cnt)
    ///
    /// Where:
    /// - peer_buf_alloc: Total buffer space the peer has allocated
    /// - tx_cnt: Total bytes we've sent to the peer
    /// - peer_fwd_cnt: Total bytes the peer has consumed (forwarded to application)
    /// - in_flight: Bytes sent but not yet consumed by peer (tx_cnt - peer_fwd_cnt)
    ///
    /// Returns the number of bytes we can safely send without overflowing peer's buffer.
    fn available_tx_credit(state: &StreamState) -> u32 {
        // Credit = peer_buf_alloc - (tx_cnt - peer_fwd_cnt)
        // This represents how much buffer space the peer has available
        let in_flight = state.tx_cnt.saturating_sub(state.peer_fwd_cnt);
        state.peer_buf_alloc.saturating_sub(in_flight)
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
        self.control_aliases
            .retain(|_, canonical| *canonical != src_port);
        self.streams.remove(&src_port);
        self.purge_pending_for_guest_port(src_port);
        self.queue_rst_with_reason(incoming_hdr, "close_stream_and_rst");
    }

    fn find_idle_control_stream(&self, exclude_src_port: u32) -> Option<u32> {
        self.streams.iter().find_map(|(guest_src_port, state)| {
            (state.guest_dst_port == 4093
                && *guest_src_port != exclude_src_port
                && state.fwd_cnt == 0
                && state.tx_cnt == 0)
                .then_some(*guest_src_port)
        })
    }

    fn resolve_stream_src_port(&self, src_port: u32) -> u32 {
        self.control_aliases
            .get(&src_port)
            .copied()
            .unwrap_or(src_port)
    }

    fn promote_control_alias(&mut self, src_port: u32, incoming_hdr: &[u8; 44]) -> u32 {
        let Some(canonical_src_port) = self.control_aliases.remove(&src_port) else {
            return src_port;
        };

        let Some(mut state) = self.streams.remove(&canonical_src_port) else {
            return src_port;
        };

        self.purge_pending_for_guest_port(canonical_src_port);
        state.request_hdr = *incoming_hdr;
        state.peer_buf_alloc = Self::hdr_u32(incoming_hdr, 36);
        state.peer_fwd_cnt = Self::hdr_u32(incoming_hdr, 40);
        state.response_pending = false;
        self.streams.insert(src_port, state);
        for canonical in self.control_aliases.values_mut() {
            if *canonical == canonical_src_port {
                *canonical = src_port;
            }
        }
        vsock_debug_log(format!(
            "promote_control_alias old_src_port={} new_src_port={} dst_port=4093",
            canonical_src_port, src_port
        ));
        src_port
    }

    fn drop_control_alias(&mut self, src_port: u32) -> bool {
        let removed = self.control_aliases.remove(&src_port).is_some();
        if removed {
            vsock_debug_log(format!(
                "drop_control_alias src_port={} dst_port=4093",
                src_port
            ));
        }
        removed
    }

    fn rebind_idle_control_stream(&mut self, new_src_port: u32, incoming_hdr: &[u8; 44]) -> bool {
        let Some(existing_src_port) = self.streams.iter().find_map(|(guest_src_port, state)| {
            (state.guest_dst_port == 4093
                && *guest_src_port != new_src_port
                && state.fwd_cnt == 0
                && state.tx_cnt == 0)
                .then_some(*guest_src_port)
        }) else {
            return false;
        };

        let Some(mut state) = self.streams.remove(&existing_src_port) else {
            return false;
        };

        self.purge_pending_for_guest_port(existing_src_port);
        state.request_hdr = *incoming_hdr;
        state.peer_buf_alloc = Self::hdr_u32(incoming_hdr, 36);
        state.peer_fwd_cnt = Self::hdr_u32(incoming_hdr, 40);
        state.tx_cnt = 0;
        state.fwd_cnt = 0;
        state.response_pending = true;
        self.streams.insert(new_src_port, state);

        vsock_debug_log(format!(
            "rebind_control_stream old_src_port={} new_src_port={} dst_port=4093",
            existing_src_port, new_src_port
        ));
        self.queue_response(incoming_hdr, VSOCK_OP_RESPONSE, Vec::new());
        true
    }

    fn has_pending_control_response(&self) -> bool {
        self.streams
            .values()
            .any(|state| state.guest_dst_port == 4093 && state.response_pending)
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
        if Self::should_trace_control_hdr(&hdr) {
            vsock_debug_log(format!(
                "queue_rst src_port={} dst_port={} pending_rx_len={} guest_port_pending={}",
                Self::hdr_u32(&hdr, 16),
                Self::hdr_u32(&hdr, 20),
                self.pending_rx.len(),
                self.pending_by_guest_port
                    .get(&guest_port)
                    .copied()
                    .unwrap_or(0)
            ));
        }
    }

    /// Read data from all active streams and queue RX packets to the guest.
    ///
    /// This function implements credit-based flow control:
    /// - Checks available TX credit before reading from each stream
    /// - Only reads up to the available credit amount
    /// - Updates tx_cnt to track bytes sent to the peer
    ///
    /// For each stream, reads up to MAX_READ_BURST_PER_STREAM times or until
    /// no more data is available or credit is exhausted.
    fn harvest_stream_reads(&mut self) {
        let mut responses: Vec<([u8; 44], Vec<u8>)> = Vec::new();
        let mut control_credit_updates: Vec<[u8; 44]> = Vec::new();
        let mut closed_ports: Vec<u32> = Vec::new();
        let mut closed_hdrs: Vec<[u8; 44]> = Vec::new();
        let mut rescheduled_control_ports: Vec<(u32, u64)> = Vec::new();

        for (port, state) in &mut self.streams {
            if let Some(ready_at) = state.control_read_ready_at {
                let now = Instant::now();
                if now < ready_at {
                    let wait_ms = ready_at.saturating_duration_since(now).as_millis() as u64;
                    if state.guest_dst_port == 4093 {
                        vsock_control_trace_log(|| {
                            format!(
                                "defer_control_pipe_read src_port={} wait_ms={}",
                                port, wait_ms
                            )
                        });
                        // Timer jitter can wake us before the control stream's
                        // backoff window has elapsed. Re-arm the wake or control
                        // polling can stop permanently until an unrelated queue
                        // event happens to poke RX processing again.
                        rescheduled_control_ports.push((*port, wait_ms.max(1)));
                    }
                    continue;
                }
                state.control_read_ready_at = None;
            }
            let mut should_close = false;
            for _ in 0..MAX_READ_BURST_PER_STREAM {
                // Check available TX credit before reading
                let available_credit = Self::available_tx_credit(state);
                if available_credit == 0 {
                    // No credit available, skip reading for now
                    break;
                }

                // Read up to the available credit amount
                let read_size = (available_credit as usize).min(4096);
                let mut rx_buf = [0u8; 4096];
                match state.stream.read(&mut rx_buf[..read_size]) {
                    Ok(n) if n > 0 => {
                        // Update tx_cnt to track bytes sent to peer
                        state.tx_cnt = state.tx_cnt.saturating_add(n as u32);
                        if state.guest_dst_port == 4093 {
                            state.control_poll_interval_ms = CONTROL_STREAM_POLL_INTERVAL_MS;
                            Self::trace_control_payload(
                                "host_to_guest",
                                *port,
                                state.guest_dst_port,
                                &rx_buf[..n],
                            );
                            control_credit_updates.push(state.request_hdr);
                        }
                        responses.push((state.request_hdr, rx_buf[..n].to_vec()));
                    }
                    Ok(_) => {
                        should_close = true;
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if state.guest_dst_port == 4093 {
                            let poll_delay_ms = state.control_poll_interval_ms;
                            state.control_read_ready_at =
                                Some(Instant::now() + Duration::from_millis(poll_delay_ms));
                            state.control_poll_interval_ms = std::cmp::min(
                                poll_delay_ms.saturating_mul(2),
                                CONTROL_STREAM_POLL_INTERVAL_MAX_MS,
                            );
                            rescheduled_control_ports.push((*port, poll_delay_ms));
                        }
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
            self.queue_rst_with_reason(&hdr, "harvest_stream_reads_close");
        }

        for (port, delay_ms) in rescheduled_control_ports {
            self.schedule_control_pipe_harvest_after(port, delay_ms);
        }

        for (hdr, payload) in responses {
            self.queue_stream_data(&hdr, payload);
        }

        for hdr in control_credit_updates {
            self.queue_credit_update(&hdr);
        }
    }

    /// Read data from all DGRAM sockets and queue RX packets to the guest.
    ///
    /// DGRAM sockets are connectionless, so we don't need credit flow control.
    /// Each datagram is sent as a separate RW packet.
    fn harvest_dgram_reads(&mut self) {
        let mut dgram_responses: Vec<(u32, u32, Vec<u8>)> = Vec::new(); // (src_port, dst_port, payload)

        for (guest_port, socket) in &self.dgram_sockets {
            let mut rx_buf = [0u8; 4096];
            match socket.recv_from(&mut rx_buf) {
                Ok((n, peer_addr)) => {
                    if n > 0 {
                        // Try to map peer address back to a guest port
                        // For now, use a simple heuristic: use peer port as dst_port
                        let dst_port = peer_addr.port() as u32;
                        dgram_responses.push((*guest_port, dst_port, rx_buf[..n].to_vec()));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // No data available, continue to next socket
                }
                Err(_) => {
                    // Socket error, ignore for now
                }
            }
        }

        // Queue DGRAM responses
        for (src_port, dst_port, payload) in dgram_responses {
            let mut hdr = [0u8; 44];
            Self::set_u64(&mut hdr, 0, VSOCK_HOST_CID);
            Self::set_u64(&mut hdr, 8, self.cid);
            Self::set_u32(&mut hdr, 16, dst_port); // Host port -> guest dst port
            Self::set_u32(&mut hdr, 20, src_port); // Guest port -> guest src port
            Self::set_u32(&mut hdr, 24, payload.len() as u32);
            Self::set_u16(&mut hdr, 28, VSOCK_TYPE_DGRAM);
            Self::set_u16(&mut hdr, 30, VSOCK_OP_RW);
            Self::set_u32(&mut hdr, 32, 0); // flags
            Self::set_u32(&mut hdr, 36, 0); // buf_alloc (not used for DGRAM)
            Self::set_u32(&mut hdr, 40, 0); // fwd_cnt (not used for DGRAM)

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
        let mut popped_heads = 0usize;
        while let Some(head) = self.queues[TXQ_INDEX].pop(&mem) {
            popped_heads = popped_heads.saturating_add(1);
            let head_index = head.index;
            let descs: Vec<DescriptorChain<'_>> = head.into_iter().collect();
            if let Some(hdr_desc) = descs.first() {
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
                    self.trace_control_tx("tx", &hdr);

                    if Self::op_requires_zero_len(op) && data_len != 0 {
                        self.queue_rst(&hdr);
                        continue;
                    }

                    match op {
                        VSOCK_OP_REQUEST => {
                            vsock_debug_log(format!(
                                "request packet src_cid={} dst_cid={} src_port={} dst_port={} type={} len={} pipe_map_present={}",
                                Self::hdr_u64(&hdr, 0),
                                Self::hdr_u64(&hdr, 8),
                                src_port,
                                dst_port,
                                pkt_type,
                                data_len,
                                self.pipe_port_map.is_some()
                            ));
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

                            // Handle DGRAM type
                            if pkt_type == VSOCK_TYPE_DGRAM {
                                // DGRAM doesn't use REQUEST/RESPONSE handshake
                                // Just send RST to indicate we don't support connection-oriented DGRAM
                                self.queue_rst(&hdr);
                                continue;
                            }

                            // STREAM type handling
                            // Reconnect on same guest source port replaces the old stream.
                            if self.streams.contains_key(&src_port) {
                                self.control_aliases.retain(|alias, canonical| {
                                    *alias != src_port && *canonical != src_port
                                });
                                self.streams.remove(&src_port);
                                self.purge_pending_for_guest_port(src_port);
                            }

                            let aliased_control_stream = if dst_port == 4093 {
                                self.find_idle_control_stream(src_port)
                            } else {
                                None
                            };

                            if let Some(canonical_src_port) = aliased_control_stream {
                                self.control_aliases.insert(src_port, canonical_src_port);
                                vsock_debug_log(format!(
                                    "alias_control_stream canonical_src_port={} alias_src_port={} dst_port=4093",
                                    canonical_src_port, src_port
                                ));
                                self.queue_response(&hdr, VSOCK_OP_RESPONSE, Vec::new());
                            }

                            if aliased_control_stream.is_none() && self.streams.len() >= MAX_STREAMS
                            {
                                warn!(
                                    "vsock(windows): stream table full (max={MAX_STREAMS}), rejecting src_port={src_port}"
                                );
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if dst_port == 4093 && aliased_control_stream.is_none() {
                                vsock_debug_log(format!(
                                    "request dst_port=4093 opening new control stream src_port={}",
                                    src_port
                                ));
                            }

                            // Try Named Pipe first, then TCP.
                            let stream_result = if aliased_control_stream.is_some() {
                                None
                            } else if let Some(pipe_map) = &self.pipe_port_map {
                                if let Some(pipe_name) = pipe_map.get(&dst_port) {
                                    if dst_port == 4093 {
                                        vsock_debug_log(format!(
                                            "request dst_port={} resolved pipe_name={}",
                                            dst_port, pipe_name
                                        ));
                                    }
                                    // Connect to Named Pipe
                                    let connect_timeout_ms = if dst_port == 4093 {
                                        self.connect_timeout_ms as u32
                                    } else {
                                        self.connect_timeout_ms as u32
                                    };
                                    match NamedPipeStream::connect(pipe_name, connect_timeout_ms) {
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
                                    if dst_port == 4093 {
                                        vsock_debug_log(format!(
                                            "request dst_port={} missing pipe mapping",
                                            dst_port
                                        ));
                                    }
                                    None
                                }
                            } else {
                                if dst_port == 4093 {
                                    vsock_debug_log("request missing any pipe map".to_string());
                                }
                                None
                            };

                            let stream_result = if aliased_control_stream.is_some() {
                                None
                            } else {
                                stream_result.or_else(|| {
                                    // Fallback to TCP
                                    if let Some(addr) = self.host_socket_addr(dst_port) {
                                        match TcpStream::connect_timeout(
                                            &addr,
                                            Duration::from_millis(self.connect_timeout_ms),
                                        ) {
                                            Ok(stream) => {
                                                if dst_port == 4093 {
                                                    vsock_debug_log(format!(
                                                        "request dst_port={} tcp fallback success addr={}",
                                                        dst_port, addr
                                                    ));
                                                }
                                                let _ = stream.set_nonblocking(true);
                                                let _ = stream.set_nodelay(true);
                                                Some(StreamType::Tcp(stream))
                                            }
                                            Err(err) => {
                                                if dst_port == 4093 {
                                                    vsock_debug_log(format!(
                                                        "request dst_port={} tcp fallback failed addr={} error={}",
                                                        dst_port, addr, err
                                                    ));
                                                }
                                                None
                                            }
                                        }
                                    } else {
                                        None
                                    }
                                })
                            };

                            if aliased_control_stream.is_some() {
                            } else if let Some(stream) = stream_result {
                                if dst_port == 4093 {
                                    vsock_debug_log(format!(
                                        "request accepted src_port={} dst_port={}",
                                        src_port, dst_port
                                    ));
                                }
                                // Extract peer's initial credit info from REQUEST header
                                let peer_buf_alloc = Self::hdr_u32(&hdr, 36);
                                let peer_fwd_cnt = Self::hdr_u32(&hdr, 40);

                                self.streams.insert(
                                    src_port,
                                    StreamState {
                                        stream,
                                        request_hdr: hdr,
                                        fwd_cnt: 0,
                                        guest_dst_port: dst_port,
                                        peer_buf_alloc,
                                        peer_fwd_cnt,
                                        tx_cnt: 0,
                                        response_pending: true,
                                        control_read_ready_at: (dst_port == 4093).then(|| {
                                            Instant::now()
                                                + Duration::from_millis(
                                                    CONTROL_STREAM_READ_GRACE_MS,
                                                )
                                        }),
                                        control_poll_interval_ms: CONTROL_STREAM_POLL_INTERVAL_MS,
                                    },
                                );
                                if dst_port == 4093 {
                                    self.schedule_control_pipe_harvest(src_port);
                                    self.schedule_control_pipe_keepalive_polling(src_port);
                                    self.schedule_control_response_irq_retry(src_port);
                                }
                                self.queue_response(&hdr, VSOCK_OP_RESPONSE, Vec::new());
                            } else {
                                if dst_port == 4093 {
                                    vsock_debug_log(format!(
                                        "request rejected src_port={} dst_port={}",
                                        src_port, dst_port
                                    ));
                                }
                                self.queue_rst_with_reason(&hdr, "request_connect_failed");
                            }
                        }
                        VSOCK_OP_RW => {
                            if src_port == 0 {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            if data_len > MAX_RW_PAYLOAD {
                                if pkt_type == VSOCK_TYPE_STREAM {
                                    self.close_stream_and_rst(src_port, &hdr);
                                } else {
                                    self.queue_rst(&hdr);
                                }
                                continue;
                            }

                            // Handle DGRAM type
                            if pkt_type == VSOCK_TYPE_DGRAM {
                                if self.reject_unsupported_tsi_listen(&hdr) {
                                    continue;
                                }

                                // For DGRAM, create socket on first use
                                if !self.dgram_sockets.contains_key(&src_port) {
                                    // Create UDP socket bound to any port
                                    match UdpSocket::bind("0.0.0.0:0") {
                                        Ok(socket) => {
                                            let _ = socket.set_nonblocking(true);
                                            self.dgram_sockets.insert(src_port, socket);
                                        }
                                        Err(_) => {
                                            self.queue_rst(&hdr);
                                            continue;
                                        }
                                    }
                                }

                                if let Some(socket) = self.dgram_sockets.get(&src_port) {
                                    if data_len > 0 {
                                        let payload = match Self::read_tx_payload(
                                            &mem,
                                            hdr_desc,
                                            &descs[1..],
                                            data_len,
                                        ) {
                                            Ok(payload) => payload,
                                            Err(err) => {
                                                if dst_port == 4093 {
                                                    vsock_debug_log(format!(
                                                        "tx dgram payload read failed src_port={} dst_port={} error={}",
                                                        src_port, dst_port, err
                                                    ));
                                                }
                                                continue;
                                            }
                                        };

                                        if payload.len() != data_len {
                                            continue;
                                        }

                                        // Send to host port if mapped
                                        if let Some(addr) = self.host_socket_addr(dst_port) {
                                            let _ = socket.send_to(&payload, addr);
                                        }
                                    }
                                }
                                continue;
                            }

                            // STREAM type handling
                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            let resolved_src_port = self.resolve_stream_src_port(src_port);
                            let active_src_port = if resolved_src_port != src_port {
                                self.promote_control_alias(src_port, &hdr)
                            } else {
                                src_port
                            };

                            if let Some(state) = self.streams.get_mut(&active_src_port) {
                                state.response_pending = false;
                                if state.guest_dst_port != dst_port {
                                    self.close_stream_and_rst(active_src_port, &hdr);
                                    continue;
                                }

                                if data_len > 0 {
                                    let payload = match Self::read_tx_payload(
                                        &mem,
                                        hdr_desc,
                                        &descs[1..],
                                        data_len,
                                    ) {
                                        Ok(payload) => payload,
                                        Err(err) => {
                                            if dst_port == 4093 || src_port == 4093 {
                                                vsock_debug_log(format!(
                                                    "tx stream payload read failed src_port={} dst_port={} len={} error={}",
                                                    src_port, dst_port, data_len, err
                                                ));
                                            }
                                            self.close_stream_and_rst(active_src_port, &hdr);
                                            continue;
                                        }
                                    };

                                    if dst_port == 4093 || src_port == 4093 {
                                        vsock_control_trace_log(|| {
                                            format!(
                                                "tx stream payload read src_port={} dst_port={} len={} inline_cap={} extra_descs={}",
                                                src_port,
                                                dst_port,
                                                data_len,
                                                hdr_desc.len.saturating_sub(44),
                                                descs.len().saturating_sub(1)
                                            )
                                        });
                                        if dst_port == 4093 {
                                            Self::trace_control_payload(
                                                "guest_to_host",
                                                src_port,
                                                dst_port,
                                                &payload,
                                            );
                                        }
                                    }

                                    if payload.len() != data_len {
                                        self.close_stream_and_rst(active_src_port, &hdr);
                                        continue;
                                    }

                                    match state.stream.write_all(&payload) {
                                        Ok(()) => {
                                            state.fwd_cnt =
                                                state.fwd_cnt.saturating_add(payload.len() as u32);
                                        }
                                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                                        Err(_) => {
                                            self.close_stream_and_rst(active_src_port, &hdr);
                                            continue;
                                        }
                                    }
                                }
                                self.harvest_stream_reads();
                                self.harvest_dgram_reads();
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

                            let resolved_src_port = self.resolve_stream_src_port(src_port);
                            let active_src_port = if resolved_src_port != src_port {
                                self.promote_control_alias(src_port, &hdr)
                            } else {
                                src_port
                            };

                            if let Some(state) = self.streams.get_mut(&active_src_port) {
                                state.response_pending = false;
                                if state.guest_dst_port != dst_port {
                                    self.queue_rst_with_reason(&hdr, "credit_update_bad_dst_port");
                                    continue;
                                }
                                // Update peer's credit information
                                state.peer_buf_alloc = Self::hdr_u32(&hdr, 36);
                                state.peer_fwd_cnt = Self::hdr_u32(&hdr, 40);
                            } else {
                                self.queue_rst_with_reason(&hdr, "credit_update_missing_stream");
                            }
                        }
                        VSOCK_OP_CREDIT_REQUEST => {
                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst(&hdr);
                                continue;
                            }

                            let resolved_src_port = self.resolve_stream_src_port(src_port);
                            let active_src_port = if resolved_src_port != src_port {
                                self.promote_control_alias(src_port, &hdr)
                            } else {
                                src_port
                            };

                            if let Some(state) = self.streams.get_mut(&active_src_port) {
                                state.response_pending = false;
                                if state.guest_dst_port != dst_port {
                                    self.queue_rst_with_reason(&hdr, "credit_request_bad_dst_port");
                                    continue;
                                }
                                self.queue_credit_update(&hdr);
                            } else {
                                self.queue_rst_with_reason(&hdr, "credit_request_missing_stream");
                            }
                        }
                        VSOCK_OP_SHUTDOWN | VSOCK_OP_RST => {
                            if pkt_type != VSOCK_TYPE_STREAM {
                                self.queue_rst_with_reason(&hdr, "shutdown_or_rst_non_stream");
                                continue;
                            }

                            let flags = Self::hdr_u32(&hdr, 32);
                            if flags & (VSOCK_FLAGS_SHUTDOWN_RCV | VSOCK_FLAGS_SHUTDOWN_SEND) != 0
                                || op == VSOCK_OP_RST
                            {
                                if Self::should_trace_control_hdr(&hdr) {
                                    vsock_debug_log(format!(
                                        "guest_close op={} src_port={} dst_port={} flags={}",
                                        op, src_port, dst_port, flags
                                    ));
                                }
                                if self.drop_control_alias(src_port) {
                                    continue;
                                }
                                let active_src_port = self.resolve_stream_src_port(src_port);
                                if let Some(state) = self.streams.get(&active_src_port) {
                                    if state.guest_dst_port != dst_port {
                                        self.queue_rst_with_reason(
                                            &hdr,
                                            "shutdown_or_rst_bad_dst_port",
                                        );
                                        continue;
                                    }
                                }
                                self.close_stream_and_rst(active_src_port, &hdr);
                            } else {
                                self.queue_credit_update(&hdr);
                            }
                        }
                        _ => self.queue_rst(&hdr),
                    }
                } else if self
                    .pipe_port_map
                    .as_ref()
                    .is_some_and(|m| m.contains_key(&4093))
                {
                    vsock_debug_log(format!(
                        "process_tx_queue read_hdr_failed head_index={} addr={:?}",
                        head_index, hdr_desc.addr
                    ));
                }
            } else if self
                .pipe_port_map
                .as_ref()
                .is_some_and(|m| m.contains_key(&4093))
            {
                vsock_debug_log(format!(
                    "process_tx_queue missing_hdr_desc head_index={}",
                    head_index
                ));
            }

            if let Err(e) = self.queues[TXQ_INDEX].add_used(&mem, head_index, 0) {
                error!("vsock(windows): failed to add TX used entry: {e:?}");
            } else {
                used_any = true;
            }
        }
        if popped_heads == 0
            && self
                .pipe_port_map
                .as_ref()
                .is_some_and(|m| m.contains_key(&4093))
        {
            vsock_debug_log("process_tx_queue no_heads".to_string());
        }
        used_any
    }

    fn process_rx_queue(&mut self) -> bool {
        let mem = match self.state {
            DeviceState::Activated(ref mem, _) => mem.clone(),
            DeviceState::Inactive => return false,
        };

        let mut used_any = false;
        let mut popped_heads = 0usize;
        while let Some(head) = self.queues[RXQ_INDEX].pop(&mem) {
            popped_heads = popped_heads.saturating_add(1);
            let head_index = head.index;
            let Some(pending) = self.pending_rx.front().cloned() else {
                self.queues[RXQ_INDEX].undo_pop();
                break;
            };
            let trace_control = Self::should_trace_control_hdr(&pending.hdr);
            if trace_control {
                vsock_debug_log(format!(
                    "process_rx_queue head_index={} op={} src_port={} dst_port={} payload_len={}",
                    head_index,
                    Self::hdr_u16(&pending.hdr, 30),
                    Self::hdr_u32(&pending.hdr, 16),
                    Self::hdr_u32(&pending.hdr, 20),
                    pending.payload.len()
                ));
            }

            let mut used = 0_u32;
            let mut iter = head.into_iter();
            if let Some(hdr_desc) = iter.next() {
                if hdr_desc.is_write_only() && Self::write_hdr(&mem, hdr_desc.addr, &pending.hdr) {
                    used = 44;

                    if !pending.payload.is_empty() {
                        let inline_payload_cap = hdr_desc.len.saturating_sub(used) as usize;
                        if inline_payload_cap >= pending.payload.len() {
                            let inline_addr =
                                vm_memory::GuestAddress(hdr_desc.addr.0 + used as u64);
                            if mem.write_slice(&pending.payload, inline_addr).is_err() {
                                if trace_control {
                                    vsock_debug_log(format!(
                                        "process_rx_queue undo_pop inline_payload_write_failed head_index={}",
                                        head_index
                                    ));
                                }
                                self.queues[RXQ_INDEX].undo_pop();
                                break;
                            }
                            if trace_control {
                                vsock_debug_log(format!(
                                    "process_rx_queue inline_payload head_index={} len={} hdr_desc_len={}",
                                    head_index,
                                    pending.payload.len(),
                                    hdr_desc.len
                                ));
                            }
                            used = used.saturating_add(pending.payload.len() as u32);
                        } else {
                            let Some(buf_desc) = iter.next() else {
                                if trace_control {
                                    vsock_debug_log(format!(
                                        "process_rx_queue undo_pop missing_payload_desc head_index={} hdr_desc_len={} inline_cap={} need={}",
                                        head_index,
                                        hdr_desc.len,
                                        inline_payload_cap,
                                        pending.payload.len()
                                    ));
                                }
                                self.queues[RXQ_INDEX].undo_pop();
                                break;
                            };
                            if !buf_desc.is_write_only()
                                || buf_desc.len < pending.payload.len() as u32
                            {
                                if trace_control {
                                    vsock_debug_log(format!(
                                        "process_rx_queue undo_pop bad_payload_desc head_index={} write_only={} len={} need={}",
                                        head_index,
                                        buf_desc.is_write_only(),
                                        buf_desc.len,
                                        pending.payload.len()
                                    ));
                                }
                                self.queues[RXQ_INDEX].undo_pop();
                                break;
                            }
                            if mem.write_slice(&pending.payload, buf_desc.addr).is_err() {
                                if trace_control {
                                    vsock_debug_log(format!(
                                        "process_rx_queue undo_pop payload_write_failed head_index={}",
                                        head_index
                                    ));
                                }
                                self.queues[RXQ_INDEX].undo_pop();
                                break;
                            }
                            used = used.saturating_add(pending.payload.len() as u32);
                        }
                    }

                    if let Some(sent) = self.pending_rx.pop_front() {
                        let sent_guest_port = Self::hdr_u32(&sent.hdr, 20);
                        if let Some(v) = self.pending_by_guest_port.get_mut(&sent_guest_port) {
                            *v = v.saturating_sub(1);
                            if *v == 0 {
                                self.pending_by_guest_port.remove(&sent_guest_port);
                            }
                        }
                        if trace_control {
                            vsock_debug_log(format!(
                                "process_rx_queue sent op={} src_port={} dst_port={} used={}",
                                Self::hdr_u16(&sent.hdr, 30),
                                Self::hdr_u32(&sent.hdr, 16),
                                Self::hdr_u32(&sent.hdr, 20),
                                used
                            ));
                        }
                    }
                } else if trace_control {
                    vsock_debug_log(format!(
                        "process_rx_queue hdr_not_written head_index={} write_only={}",
                        head_index,
                        hdr_desc.is_write_only()
                    ));
                }
            }

            if let Err(e) = self.queues[RXQ_INDEX].add_used(&mem, head_index, used) {
                error!("vsock(windows): failed to add RX used entry: {e:?}");
            } else {
                if trace_control {
                    vsock_debug_log(format!(
                        "process_rx_queue add_used head_index={} used={}",
                        head_index, used
                    ));
                }
                used_any = true;
            }
        }

        if popped_heads == 0 {
            if let Some(pending) = self.pending_rx.front() {
                if Self::should_trace_control_hdr(&pending.hdr) {
                    vsock_debug_log(format!(
                        "process_rx_queue no_heads pending_op={} src_port={} dst_port={} pending_rx_len={}",
                        Self::hdr_u16(&pending.hdr, 30),
                        Self::hdr_u32(&pending.hdr, 16),
                        Self::hdr_u32(&pending.hdr, 20),
                        self.pending_rx.len()
                    ));
                }
            }
        }

        used_any
    }

    fn process_evq_queue(&mut self) -> bool {
        // The event queue is reserved for transport events (for example
        // transport reset). Normal operation does not consume EVQ buffers.
        // Mirror the non-Windows vsock behavior here and only use the EVQ
        // doorbell as a wake-up hint for TX/RX processing.
        false
    }

    fn flush_deferred_control_rx(&mut self) -> bool {
        if !self.defer_control_rx {
            return false;
        }

        if !self.has_pending_control_response() {
            self.defer_control_rx = false;
            return false;
        }

        self.defer_control_rx = false;
        vsock_debug_log(format!(
            "flush_deferred_control_rx pending_rx={} streams={}",
            self.pending_rx.len(),
            self.streams.len()
        ));
        self.process_rx_queue()
    }
}

fn vsock_debug_log(message: String) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    utils::windows_debug_log(
        "vsock.log",
        format!(
            "[{:>10}.{:03}] {}",
            now.as_secs(),
            now.subsec_millis(),
            message
        ),
    );
}

fn vsock_control_trace_enabled() -> bool {
    static TRACE_CONTROL: OnceLock<bool> = OnceLock::new();
    *TRACE_CONTROL
        .get_or_init(|| std::env::var("LIBKRUN_WINDOWS_VSOCK_TRACE_CONTROL").as_deref() == Ok("1"))
}

fn vsock_control_trace_log(message: impl FnOnce() -> String) {
    if vsock_control_trace_enabled() {
        vsock_debug_log(message());
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
            error!(
                "vsock(windows): expected {NUM_QUEUES} queues, got {}",
                self.queues.len()
            );
            return Err(ActivateError::BadActivate);
        }
        self.state = DeviceState::Activated(mem, interrupt);
        self.activate_evt
            .write(1)
            .map_err(|_| ActivateError::BadActivate)?;

        if self
            .pipe_port_map
            .as_ref()
            .is_some_and(|m| m.contains_key(&4093))
        {
            let fds: Vec<i32> = self
                .queue_events
                .iter()
                .map(|evt| evt.as_raw_fd())
                .collect();
            vsock_debug_log(format!("activate cid={} queue_fds={:?}", self.cid, fds));
        }

        debug!(
            "vsock(windows): device activated, CID={}, streams={}, pending_rx={}",
            self.cid,
            self.streams.len(),
            self.pending_rx.len()
        );
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
            if self
                .pipe_port_map
                .as_ref()
                .is_some_and(|m| m.contains_key(&4093))
            {
                vsock_debug_log(format!(
                    "event rx source={} pending_rx={} streams={}",
                    source,
                    self.pending_rx.len(),
                    self.streams.len()
                ));
            }
            let _ = self.queue_events[RXQ_INDEX].read();
            raise_irq |= self.flush_deferred_control_rx();
            self.harvest_stream_reads();
            self.harvest_dgram_reads();
            raise_irq |= self.process_rx_queue();
        } else if source == self.queue_events[TXQ_INDEX].as_raw_fd() {
            if self
                .pipe_port_map
                .as_ref()
                .is_some_and(|m| m.contains_key(&4093))
            {
                vsock_debug_log(format!(
                    "event tx source={} pending_rx={} streams={}",
                    source,
                    self.pending_rx.len(),
                    self.streams.len()
                ));
            }
            let _ = self.queue_events[TXQ_INDEX].read();
            raise_irq |= self.flush_deferred_control_rx();
            raise_irq |= self.process_tx_queue();
            self.harvest_stream_reads();
            self.harvest_dgram_reads();
            if self.defer_control_rx {
                vsock_debug_log(format!(
                    "skip_process_rx_queue_deferred_control pending_rx={} streams={}",
                    self.pending_rx.len(),
                    self.streams.len()
                ));
            } else {
                raise_irq |= self.process_rx_queue();
                // A freshly connected control pipe can receive FRAME_OPEN
                // immediately after we deliver the RESPONSE. Harvest once more
                // so the Windows debug path does not depend on a later queue
                // event to notice host pipe readability.
                self.harvest_stream_reads();
                self.harvest_dgram_reads();
                raise_irq |= self.process_rx_queue();
            }
        } else if source == self.queue_events[EVQ_INDEX].as_raw_fd() {
            if self
                .pipe_port_map
                .as_ref()
                .is_some_and(|m| m.contains_key(&4093))
            {
                vsock_debug_log(format!(
                    "event ev source={} pending_rx={} streams={}",
                    source,
                    self.pending_rx.len(),
                    self.streams.len()
                ));
            }
            let _ = self.queue_events[EVQ_INDEX].read();
            raise_irq |= self.flush_deferred_control_rx();
            // Some Windows guests appear to miss the initial TX queue wake for
            // the first 4093 control connect. Opportunistically draining TXQ on
            // EVQ activity keeps the control handshake moving without affecting
            // non-Windows targets.
            raise_irq |= self.process_tx_queue();
            self.harvest_stream_reads();
            self.harvest_dgram_reads();
            raise_irq |= self.process_evq_queue();
            raise_irq |= self.process_rx_queue();
        }

        if raise_irq {
            if self
                .pipe_port_map
                .as_ref()
                .is_some_and(|m| m.contains_key(&4093))
            {
                vsock_debug_log(format!(
                    "signal_used_queue pending_rx={} streams={}",
                    self.pending_rx.len(),
                    self.streams.len()
                ));
            }
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

#[cfg(test)]
mod tests;
