// Copyright 2024 The libkrun Authors.
// SPDX-License-Identifier: Apache-2.0

//! Windows virtio-net backend.
//!
//! Implements virtio-net (device type 1) backed by an optional TCP socket.
//! Ethernet frames from the guest TX queue are forwarded to the TCP stream
//! (if one is connected). Frames from the TCP stream are injected into the
//! guest RX queue.  When no backend is connected TX frames are silently
//! dropped and the RX queue is never filled.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::io;

use polly::event_manager::{EventManager, Subscriber};
use utils::epoll::{EpollEvent, EventSet};
use utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

use super::{
    ActivateError, ActivateResult, DescriptorChain, DeviceState, InterruptTransport, Queue,
    VirtioDevice, TYPE_NET,
};

// ── virtio-net feature bits ───────────────────────────────────────────────────
const VIRTIO_F_VERSION_1: u32 = 32;
const VIRTIO_NET_F_CSUM: u32 = 0;        // device handles partial checksums
const VIRTIO_NET_F_GUEST_CSUM: u32 = 1;  // driver handles partial checksums
const VIRTIO_NET_F_MAC: u32 = 5;         // device has a MAC address
const VIRTIO_NET_F_HOST_TSO4: u32 = 11;  // device can receive TSOv4
const VIRTIO_NET_F_HOST_TSO6: u32 = 12;  // device can receive TSOv6
const VIRTIO_NET_F_GUEST_TSO4: u32 = 7;  // driver can receive TSOv4
const VIRTIO_NET_F_GUEST_TSO6: u32 = 8;  // driver can receive TSOv6

// ── queue indices ─────────────────────────────────────────────────────────────
const RX_INDEX: usize = 0;
const TX_INDEX: usize = 1;
const NUM_QUEUES: usize = 2;
const QUEUE_SIZE: u16 = 256;

// ── config space layout ───────────────────────────────────────────────────────
// Offset 0 : mac[6]              (6 bytes)
// Offset 6 : status              (2 bytes, 1 = link up)
// Offset 8 : max_virtqueue_pairs (2 bytes, always 1)
const CONFIG_SPACE_SIZE: usize = 10;

// virtio-net header (10 bytes, no VIRTIO_NET_F_MRG_RXBUF)
const VIRTIO_NET_HDR_SIZE: usize = 10;

// virtio-net header flags
const VIRTIO_NET_HDR_F_NEEDS_CSUM: u8 = 1;
const VIRTIO_NET_HDR_F_DATA_VALID: u8 = 2;

// virtio-net GSO types
const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;
const VIRTIO_NET_HDR_GSO_TCPV4: u8 = 1;
const VIRTIO_NET_HDR_GSO_TCPV6: u8 = 4;

// ── virtio-net header ─────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct VirtioNetHdr {
    flags: u8,
    gso_type: u8,
    hdr_len: u16,
    gso_size: u16,
    csum_start: u16,
    csum_offset: u16,
}

impl VirtioNetHdr {
    fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.len() < VIRTIO_NET_HDR_SIZE {
            return Self::default();
        }
        Self {
            flags: bytes[0],
            gso_type: bytes[1],
            hdr_len: u16::from_le_bytes([bytes[2], bytes[3]]),
            gso_size: u16::from_le_bytes([bytes[4], bytes[5]]),
            csum_start: u16::from_le_bytes([bytes[6], bytes[7]]),
            csum_offset: u16::from_le_bytes([bytes[8], bytes[9]]),
        }
    }

    fn to_bytes(&self) -> [u8; VIRTIO_NET_HDR_SIZE] {
        let mut bytes = [0u8; VIRTIO_NET_HDR_SIZE];
        bytes[0] = self.flags;
        bytes[1] = self.gso_type;
        bytes[2..4].copy_from_slice(&self.hdr_len.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.gso_size.to_le_bytes());
        bytes[6..8].copy_from_slice(&self.csum_start.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.csum_offset.to_le_bytes());
        bytes
    }
}

// ── Net ───────────────────────────────────────────────────────────────────────

pub struct Net {
    id: String,
    mac: [u8; 6],
    backend: Option<Mutex<TcpStream>>,
    queues: Vec<Queue>,
    queue_events: Vec<EventFd>,
    activate_evt: EventFd,
    state: DeviceState,
    acked_features: u64,
}

impl Net {
    /// Create a new virtio-net device.
    ///
    /// `id` is a unique identifier used when registering the device with the
    /// MMIO transport manager.
    /// `mac` is the 6-byte MAC address advertised to the guest.
    /// `backend` is an optional TCP stream used for packet I/O.  When `None`
    /// all TX frames are silently dropped and no RX frames are ever produced.
    pub fn new(id: impl Into<String>, mac: [u8; 6], backend: Option<TcpStream>) -> io::Result<Self> {
        // Validate MAC address
        if mac[0] & 0x01 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MAC address cannot be multicast (bit 0 of first byte must be 0)",
            ));
        }

        let queue_events = (0..NUM_QUEUES)
            .map(|_| EventFd::new(EFD_NONBLOCK))
            .collect::<io::Result<Vec<_>>>()?;

        Ok(Self {
            id: id.into(),
            mac,
            backend: backend.map(Mutex::new),
            queues: vec![Queue::new(QUEUE_SIZE); NUM_QUEUES],
            queue_events,
            activate_evt: EventFd::new(EFD_NONBLOCK)?,
            state: DeviceState::Inactive,
            acked_features: 0,
        })
    }

    /// Returns the device identifier used for MMIO registration.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn register_runtime_events(&self, event_manager: &mut EventManager) {
        let Ok(self_subscriber) = event_manager.subscriber(self.activate_evt.as_raw_fd()) else {
            return;
        };

        for evt in &self.queue_events {
            let fd = evt.as_raw_fd();
            let event = EpollEvent::new(EventSet::IN, fd as u64);
            if let Err(e) = event_manager.register(fd, event, self_subscriber.clone()) {
                error!("net(windows): failed to register queue event {fd}: {e:?}");
            }
        }

        let _ = event_manager.unregister(self.activate_evt.as_raw_fd());
    }

    /// Process the TX queue: consume guest descriptors and forward to backend.
    ///
    /// Each descriptor chain begins with a 10-byte virtio-net header followed
    /// by one or more read-only data descriptors containing the Ethernet frame.
    /// If VIRTIO_NET_F_CSUM is negotiated, the header may request checksum
    /// offload (NEEDS_CSUM flag). If VIRTIO_NET_F_HOST_TSO4/6 is negotiated,
    /// the header may request TCP segmentation (GSO).
    fn process_tx_queue(&mut self) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        let mut used_any = false;

        while let Some(head) = self.queues[TX_INDEX].pop(mem) {
            let index = head.index;
            let mut total_len: u32 = 0;
            let mut hdr_bytes = vec![0u8; VIRTIO_NET_HDR_SIZE];
            let mut hdr_bytes_read: usize = 0;
            let mut frame_data = Vec::new();

            let descs: Vec<DescriptorChain<'_>> = head.into_iter().collect();
            for desc in &descs {
                if desc.is_write_only() {
                    continue;
                }

                let len = desc.len as usize;
                total_len = total_len.saturating_add(desc.len);

                // Read the virtio-net header first
                if hdr_bytes_read < VIRTIO_NET_HDR_SIZE {
                    let to_read = (VIRTIO_NET_HDR_SIZE - hdr_bytes_read).min(len);
                    if mem.read_slice(&mut hdr_bytes[hdr_bytes_read..hdr_bytes_read + to_read], desc.addr).is_err() {
                        break;
                    }
                    hdr_bytes_read += to_read;

                    // Read remaining payload from this descriptor
                    if to_read < len {
                        let payload_len = len - to_read;
                        let payload_addr = GuestAddress(desc.addr.0 + to_read as u64);
                        let mut buf = vec![0u8; payload_len];
                        if mem.read_slice(&mut buf, payload_addr).is_ok() {
                            frame_data.extend_from_slice(&buf);
                        }
                    }
                } else {
                    // Pure payload descriptor
                    let mut buf = vec![0u8; len];
                    if mem.read_slice(&mut buf, desc.addr).is_ok() {
                        frame_data.extend_from_slice(&buf);
                    }
                }
            }

            // Process the frame with offload handling
            if !frame_data.is_empty() {
                if let Some(ref backend) = self.backend {
                    let hdr = VirtioNetHdr::from_bytes(&hdr_bytes);

                    // Handle checksum offload
                    if hdr.flags & VIRTIO_NET_HDR_F_NEEDS_CSUM != 0 {
                        Self::compute_checksum(&mut frame_data, hdr.csum_start as usize, hdr.csum_offset as usize);
                    }

                    // Handle TSO/GSO - for now just send as-is
                    // A full implementation would segment large packets here
                    if hdr.gso_type != VIRTIO_NET_HDR_GSO_NONE {
                        // TODO: Implement packet segmentation for TSO
                        // For now, just forward the large packet
                    }

                    if let Ok(mut stream) = backend.lock() {
                        let _ = stream.write_all(&frame_data);
                    }
                }
            }

            if let Err(e) = self.queues[TX_INDEX].add_used(mem, index, total_len) {
                error!("net(windows): TX failed to add used entry: {e:?}");
            } else {
                used_any = true;
            }
        }

        used_any
    }

    /// Compute Internet checksum for partial checksum offload.
    fn compute_checksum(data: &mut [u8], csum_start: usize, csum_offset: usize) {
        if csum_start + csum_offset + 2 > data.len() {
            return;
        }

        // Zero out the checksum field first
        data[csum_start + csum_offset] = 0;
        data[csum_start + csum_offset + 1] = 0;

        // Compute Internet checksum (RFC 1071)
        let mut sum: u32 = 0;
        let payload = &data[csum_start..];

        for chunk in payload.chunks(2) {
            let word = if chunk.len() == 2 {
                u16::from_be_bytes([chunk[0], chunk[1]]) as u32
            } else {
                (chunk[0] as u32) << 8
            };
            sum += word;
        }

        // Fold 32-bit sum to 16 bits
        while sum >> 16 != 0 {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }

        let checksum = !sum as u16;
        data[csum_start + csum_offset..csum_start + csum_offset + 2]
            .copy_from_slice(&checksum.to_be_bytes());
    }

    /// Process the RX queue: fill guest buffers with data from the backend.
    ///
    /// Each available descriptor provides a write-only buffer.  A
    /// virtio-net header is written first. If VIRTIO_NET_F_GUEST_CSUM is
    /// negotiated, the DATA_VALID flag is set to indicate checksums are good.
    /// The header is followed by as many bytes as the backend has ready.
    fn process_rx_queue(&mut self) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        let Some(ref backend) = self.backend else {
            // No backend — drain the avail ring to prevent guest from blocking.
            while self.queues[RX_INDEX].pop(mem).is_some() {
                // Descriptors are consumed but not returned to used ring
            }
            return false;
        };

        let mut used_any = false;

        // Build RX header with DATA_VALID flag if guest supports checksum offload
        let mut rx_hdr = VirtioNetHdr::default();
        if self.acked_features & (1u64 << VIRTIO_NET_F_GUEST_CSUM) != 0 {
            rx_hdr.flags = VIRTIO_NET_HDR_F_DATA_VALID;
        }
        let hdr_bytes = rx_hdr.to_bytes();

        while let Some(head) = self.queues[RX_INDEX].pop(mem) {
            let index = head.index;
            let mut hdr_written: usize = 0;
            let mut frame_written: u32 = 0;
            let mut frame_ready = false;

            for desc in head.into_iter() {
                if !desc.is_write_only() {
                    continue;
                }

                let desc_len = desc.len as usize;

                // Write (part of) the virtio-net header first.
                if hdr_written < VIRTIO_NET_HDR_SIZE {
                    let hdr_remaining = VIRTIO_NET_HDR_SIZE - hdr_written;
                    let hdr_to_write = hdr_remaining.min(desc_len);
                    if mem.write_slice(&hdr_bytes[hdr_written..hdr_written + hdr_to_write], desc.addr).is_err() {
                        break;
                    }
                    hdr_written += hdr_to_write;
                    frame_written = frame_written.saturating_add(hdr_to_write as u32);

                    // Payload portion of this descriptor (after the header)
                    let remaining = desc_len - hdr_to_write;
                    if remaining > 0 {
                        let mut buf = vec![0u8; remaining];
                        let n = match backend.lock() {
                            Ok(mut s) => s.read(&mut buf).unwrap_or(0),
                            Err(_) => 0,
                        };
                        if n > 0 {
                            let addr = GuestAddress(desc.addr.0 + hdr_to_write as u64);
                            if mem.write_slice(&buf[..n], addr).is_ok() {
                                frame_written = frame_written.saturating_add(n as u32);
                                frame_ready = true;
                            }
                        }
                    }
                } else {
                    // Pure payload descriptor.
                    let mut buf = vec![0u8; desc_len];
                    let n = match backend.lock() {
                        Ok(mut s) => s.read(&mut buf).unwrap_or(0),
                        Err(_) => 0,
                    };
                    if n > 0 && mem.write_slice(&buf[..n], desc.addr).is_ok() {
                        frame_written = frame_written.saturating_add(n as u32);
                        frame_ready = true;
                    }
                }
            }

            if frame_ready {
                if let Err(e) = self.queues[RX_INDEX].add_used(mem, index, frame_written) {
                    error!("net(windows): RX failed to add used entry: {e:?}");
                } else {
                    used_any = true;
                }
            }
        }

        used_any
    }
}

// ── VirtioDevice ──────────────────────────────────────────────────────────────

impl VirtioDevice for Net {
    fn avail_features(&self) -> u64 {
        (1u64 << VIRTIO_F_VERSION_1)
            | (1u64 << VIRTIO_NET_F_MAC)
            | (1u64 << VIRTIO_NET_F_CSUM)
            | (1u64 << VIRTIO_NET_F_GUEST_CSUM)
            | (1u64 << VIRTIO_NET_F_HOST_TSO4)
            | (1u64 << VIRTIO_NET_F_HOST_TSO6)
            | (1u64 << VIRTIO_NET_F_GUEST_TSO4)
            | (1u64 << VIRTIO_NET_F_GUEST_TSO6)
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features;
    }

    fn device_type(&self) -> u32 {
        TYPE_NET
    }

    fn device_name(&self) -> &str {
        "net_windows"
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
        // Build config space on the fly.
        let mut cfg = [0u8; CONFIG_SPACE_SIZE];
        cfg[..6].copy_from_slice(&self.mac);
        let status: u16 = 1; // VIRTIO_NET_S_LINK_UP
        cfg[6..8].copy_from_slice(&status.to_le_bytes());
        let max_pairs: u16 = 1;
        cfg[8..10].copy_from_slice(&max_pairs.to_le_bytes());

        let end = (offset as usize).saturating_add(data.len()).min(CONFIG_SPACE_SIZE);
        let start = (offset as usize).min(end);
        let slice = &cfg[start..end];
        data[..slice.len()].copy_from_slice(slice);
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "net(windows): guest attempted write to config (offset={offset:#x}, len={})",
            data.len()
        );
    }

    fn activate(&mut self, mem: GuestMemoryMmap, interrupt: InterruptTransport) -> ActivateResult {
        if self.queues.len() != NUM_QUEUES {
            error!(
                "net(windows): expected {NUM_QUEUES} queues, got {}",
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

// ── Subscriber ────────────────────────────────────────────────────────────────

impl Subscriber for Net {
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

        if source == self.queue_events[RX_INDEX].as_raw_fd() {
            let _ = self.queue_events[RX_INDEX].read();
            raise_irq |= self.process_rx_queue();
        } else if source == self.queue_events[TX_INDEX].as_raw_fd() {
            let _ = self.queue_events[TX_INDEX].read();
            raise_irq |= self.process_tx_queue();
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
