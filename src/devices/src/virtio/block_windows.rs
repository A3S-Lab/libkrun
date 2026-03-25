// Copyright 2024 The libkrun Authors.
// SPDX-License-Identifier: Apache-2.0

//! Windows virtio-blk backend.
//!
//! Implements the virtio-blk protocol backed by a Windows file (raw image).
//! Uses standard Rust I/O (`std::fs::File` + `std::io::Seek`) so no
//! Win32-specific APIs are needed in this module.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

#[cfg(target_os = "windows")]
use std::os::windows::io::AsRawHandle;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::BOOLEAN;
#[cfg(target_os = "windows")]
use windows::Win32::System::IO::DeviceIoControl;

#[cfg(target_os = "windows")]
const FSCTL_SET_SPARSE: u32 = 0x000900c4; // CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 49, METHOD_BUFFERED, FILE_SPECIAL_ACCESS)

#[cfg(target_os = "windows")]
#[repr(C)]
#[allow(non_snake_case)]
struct FILE_SET_SPARSE_BUFFER {
    SetSparse: BOOLEAN,
}

use polly::event_manager::{EventManager, Subscriber};
use utils::epoll::{EpollEvent, EventSet};
use utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::{Bytes, GuestMemoryMmap};

use super::{
    ActivateError, ActivateResult, DescriptorChain, DeviceState, InterruptTransport, Queue,
    VirtioDevice,
};

// ── virtio constants ────────────────────────────────────────────────────────
const VIRTIO_F_VERSION_1: u32 = 32;
const VIRTIO_BLK_F_RO: u32 = 5; // device is read-only
const VIRTIO_BLK_F_FLUSH: u32 = 9; // device supports flush (VIRTIO_BLK_T_FLUSH)
const VIRTIO_ID_BLOCK: u32 = 2;

// virtio-blk request types
const VIRTIO_BLK_T_IN: u32 = 0; // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write
const VIRTIO_BLK_T_FLUSH: u32 = 4; // flush
const VIRTIO_BLK_T_GET_ID: u32 = 11; // get device id

// virtio-blk status values
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

const SECTOR_SHIFT: u8 = 9;
const SECTOR_SIZE: u64 = 1 << SECTOR_SHIFT; // 512 bytes

const NUM_QUEUES: usize = 1;
const QUEUE_SIZE: u16 = 256;
const REQ_QUEUE: usize = 0;

// virtio-blk request header: type(u32) + reserved(u32) + sector(u64) = 16 bytes
const REQ_HDR_SIZE: usize = 16;

// Capacity in 512-byte sectors is exposed as 8 bytes at config offset 0.
const CONFIG_SPACE_SIZE: usize = 8;

// ── Block ───────────────────────────────────────────────────────────────────

pub struct Block {
    id: String,
    disk: Mutex<File>,
    nsectors: u64,
    read_only: bool,
    queues: Vec<Queue>,
    queue_events: Vec<EventFd>,
    activate_evt: EventFd,
    state: DeviceState,
    acked_features: u64,
}

impl Block {
    /// Open a disk image at `path`.
    ///
    /// `read_only` maps to `O_RDONLY`; an attempt to write to a read-only
    /// device will be rejected with `VIRTIO_BLK_S_IOERR`.
    pub fn new(id: impl Into<String>, path: &str, read_only: bool) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(!read_only).open(path)?;

        let disk_size = file.metadata()?.len();
        let nsectors = disk_size / SECTOR_SIZE;

        // Enable sparse file support on Windows for better disk space efficiency
        #[cfg(target_os = "windows")]
        if !read_only {
            if let Err(e) = Self::set_sparse(&file) {
                log::warn!("block(windows): Failed to set sparse file attribute: {}", e);
                // Continue anyway - sparse files are an optimization, not required
            }
        }

        Ok(Self {
            id: id.into(),
            disk: Mutex::new(file),
            nsectors,
            read_only,
            queues: vec![Queue::new(QUEUE_SIZE)],
            queue_events: vec![EventFd::new(EFD_NONBLOCK)?],
            activate_evt: EventFd::new(EFD_NONBLOCK)?,
            state: DeviceState::Inactive,
            acked_features: 0,
        })
    }

    /// Set the sparse file attribute on Windows.
    /// This allows the filesystem to deallocate zero-filled regions.
    #[cfg(target_os = "windows")]
    fn set_sparse(file: &File) -> io::Result<()> {
        use windows::Win32::Foundation::HANDLE;

        let handle = HANDLE(file.as_raw_handle() as *mut _);

        // Set the sparse file attribute using FSCTL_SET_SPARSE
        let mut bytes_returned = 0u32;
        let set_sparse = FILE_SET_SPARSE_BUFFER {
            SetSparse: true.into(),
        };

        unsafe {
            DeviceIoControl(
                handle,
                FSCTL_SET_SPARSE,
                Some(&set_sparse as *const _ as *const _),
                std::mem::size_of::<FILE_SET_SPARSE_BUFFER>() as u32,
                None,
                0,
                Some(&mut bytes_returned),
                None,
            )
        }
        .map_err(|e| io::Error::other(format!("FSCTL_SET_SPARSE failed: {}", e)))?;

        Ok(())
    }

    /// Returns the device id used for registration in the MMIO manager.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn register_runtime_events(&self, event_manager: &mut EventManager) {
        let Ok(self_subscriber) = event_manager.subscriber(self.activate_evt.as_raw_fd()) else {
            return;
        };

        let fd = self.queue_events[REQ_QUEUE].as_raw_fd();
        let event = EpollEvent::new(EventSet::IN, fd as u64);
        if let Err(e) = event_manager.register(fd, event, self_subscriber.clone()) {
            error!("blk(windows): failed to register queue event {fd}: {e:?}");
        }

        let _ = event_manager.unregister(self.activate_evt.as_raw_fd());
    }

    fn process_queue(&mut self) -> bool {
        // Borrow mem from state; all processing helpers take explicit params so
        // they do not re-borrow `self` mutably while `mem` is live.
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        let mut have_used = false;

        while let Some(head) = self.queues[REQ_QUEUE].pop(mem) {
            let index = head.index;
            // Collect all descriptors in this chain.
            let descs: Vec<DescriptorChain<'_>> = head.into_iter().collect();

            let status = if descs.len() < 2 {
                error!("blk(windows): descriptor chain too short ({})", descs.len());
                VIRTIO_BLK_S_IOERR
            } else {
                let status_desc_idx = descs.len() - 1;
                let status_addr = descs[status_desc_idx].addr;

                // Parse the 16-byte request header from the first descriptor.
                let mut hdr = [0u8; REQ_HDR_SIZE];
                let st = if descs[0].len < REQ_HDR_SIZE as u32
                    || mem.read_slice(&mut hdr, descs[0].addr).is_err()
                {
                    VIRTIO_BLK_S_IOERR
                } else {
                    let req_type = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                    let sector = u64::from_le_bytes([
                        hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
                    ]);
                    let data = &descs[1..status_desc_idx];
                    match req_type {
                        VIRTIO_BLK_T_IN => {
                            Self::blk_read(&self.disk, self.nsectors, data, mem, sector)
                        }
                        VIRTIO_BLK_T_OUT => {
                            if self.read_only {
                                VIRTIO_BLK_S_IOERR
                            } else {
                                Self::blk_write(&self.disk, self.nsectors, data, mem, sector)
                            }
                        }
                        VIRTIO_BLK_T_FLUSH => Self::blk_flush(&self.disk),
                        VIRTIO_BLK_T_GET_ID => Self::blk_get_id(&self.id, data, mem),
                        _ => VIRTIO_BLK_S_UNSUPP,
                    }
                };

                if mem.write_slice(&[st], status_addr).is_err() {
                    error!("blk(windows): failed to write status byte");
                }
                st
            };

            let _ = status; // status was written to guest memory above
            have_used = true;
            if let Err(e) = self.queues[REQ_QUEUE].add_used(mem, index, 1) {
                error!("blk(windows): failed to add used entry: {e:?}");
            }
        }

        have_used
    }

    fn blk_read(
        disk: &Mutex<File>,
        nsectors: u64,
        data_descs: &[DescriptorChain<'_>],
        mem: &GuestMemoryMmap,
        start_sector: u64,
    ) -> u8 {
        if start_sector >= nsectors {
            return VIRTIO_BLK_S_IOERR;
        }
        let byte_offset = start_sector * SECTOR_SIZE;
        let mut disk = match disk.lock() {
            Ok(d) => d,
            Err(_) => return VIRTIO_BLK_S_IOERR,
        };
        if disk.seek(SeekFrom::Start(byte_offset)).is_err() {
            return VIRTIO_BLK_S_IOERR;
        }
        for desc in data_descs {
            if !desc.is_write_only() {
                continue;
            }
            let mut buf = vec![0u8; desc.len as usize];
            if disk.read_exact(&mut buf).is_err() {
                return VIRTIO_BLK_S_IOERR;
            }
            if mem.write_slice(&buf, desc.addr).is_err() {
                return VIRTIO_BLK_S_IOERR;
            }
        }
        VIRTIO_BLK_S_OK
    }

    fn blk_write(
        disk: &Mutex<File>,
        nsectors: u64,
        data_descs: &[DescriptorChain<'_>],
        mem: &GuestMemoryMmap,
        start_sector: u64,
    ) -> u8 {
        if start_sector >= nsectors {
            return VIRTIO_BLK_S_IOERR;
        }
        let byte_offset = start_sector * SECTOR_SIZE;
        let mut disk = match disk.lock() {
            Ok(d) => d,
            Err(_) => return VIRTIO_BLK_S_IOERR,
        };
        if disk.seek(SeekFrom::Start(byte_offset)).is_err() {
            return VIRTIO_BLK_S_IOERR;
        }
        for desc in data_descs {
            if desc.is_write_only() {
                continue;
            }
            let mut buf = vec![0u8; desc.len as usize];
            if mem.read_slice(&mut buf, desc.addr).is_err() {
                return VIRTIO_BLK_S_IOERR;
            }
            if disk.write_all(&buf).is_err() {
                return VIRTIO_BLK_S_IOERR;
            }
        }
        VIRTIO_BLK_S_OK
    }

    fn blk_flush(disk: &Mutex<File>) -> u8 {
        let mut disk = match disk.lock() {
            Ok(d) => d,
            Err(_) => return VIRTIO_BLK_S_IOERR,
        };
        if disk.flush().is_err() {
            VIRTIO_BLK_S_IOERR
        } else {
            VIRTIO_BLK_S_OK
        }
    }

    fn blk_get_id(id: &str, data_descs: &[DescriptorChain<'_>], mem: &GuestMemoryMmap) -> u8 {
        // The device ID string is at most 20 bytes, NUL-padded.
        let id_bytes = id.as_bytes();
        let mut id_buf = [0u8; 20];
        let copy_len = id_bytes.len().min(20);
        id_buf[..copy_len].copy_from_slice(&id_bytes[..copy_len]);
        for desc in data_descs {
            if !desc.is_write_only() {
                continue;
            }
            let write_len = (desc.len as usize).min(20);
            if mem.write_slice(&id_buf[..write_len], desc.addr).is_err() {
                return VIRTIO_BLK_S_IOERR;
            }
            break;
        }
        VIRTIO_BLK_S_OK
    }
}

impl VirtioDevice for Block {
    fn avail_features(&self) -> u64 {
        let mut f: u64 = 1 << VIRTIO_F_VERSION_1;
        f |= 1 << VIRTIO_BLK_F_FLUSH;
        if self.read_only {
            f |= 1 << VIRTIO_BLK_F_RO;
        }
        f
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features;
    }

    fn device_type(&self) -> u32 {
        VIRTIO_ID_BLOCK
    }

    fn device_name(&self) -> &str {
        "blk_windows"
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
        // Expose capacity (in sectors) at offset 0 as little-endian u64.
        let config: [u8; CONFIG_SPACE_SIZE] = self.nsectors.to_le_bytes();
        let end = (offset as usize)
            .saturating_add(data.len())
            .min(CONFIG_SPACE_SIZE);
        let start = (offset as usize).min(end);
        let slice = &config[start..end];
        data[..slice.len()].copy_from_slice(slice);
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "blk(windows): guest attempted to write config (offset={offset:#x}, len={})",
            data.len()
        );
    }

    fn activate(&mut self, mem: GuestMemoryMmap, interrupt: InterruptTransport) -> ActivateResult {
        if self.queues.len() != NUM_QUEUES {
            error!(
                "blk(windows): expected {NUM_QUEUES} queue(s), got {}",
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

impl Subscriber for Block {
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

        if source == self.queue_events[REQ_QUEUE].as_raw_fd() {
            let _ = self.queue_events[REQ_QUEUE].read();
            if self.process_queue() {
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
