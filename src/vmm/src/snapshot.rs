//! Snapshot support: file-backed guest RAM (Phase A of native snapshot-fork).
//!
//! When `KRUN_SNAPSHOT_MEM_FILE` is set, guest RAM regions are backed by that
//! file with `MAP_SHARED`, so the file always holds the live RAM contents: a
//! snapshot of memory is the file itself (after a pause + msync), and a restore
//! can map the same file `MAP_PRIVATE` for kernel page-level copy-on-write.

use std::fs::File;
use std::sync::Mutex;

use vm_memory::GuestAddress;

/// Layout of one file-backed guest RAM region: (guest_addr, size, file_offset).
pub type MemRegionLayout = (GuestAddress, usize, u64);

/// The file backing guest RAM plus the region layout inside it.
pub struct MemBacking {
    pub file: File,
    pub layout: Vec<MemRegionLayout>,
}

/// Registered at guest-memory creation; consumed by the snapshot step.
pub static MEM_BACKING: Mutex<Option<MemBacking>> = Mutex::new(None);

/// The snapshot-memory file path, if file-backed RAM was requested.
pub fn mem_file_path() -> Option<String> {
    std::env::var("KRUN_SNAPSHOT_MEM_FILE").ok().filter(|p| !p.is_empty())
}
