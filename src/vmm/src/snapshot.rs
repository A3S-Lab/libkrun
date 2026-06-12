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

/// The unix socket on which to serve snapshot requests, if enabled.
pub fn trigger_sock_path() -> Option<String> {
    std::env::var("KRUN_SNAPSHOT_SOCK").ok().filter(|p| !p.is_empty())
}

/// When set, boot is a RESTORE from this state file (paired with the RAM file in
/// `KRUN_SNAPSHOT_MEM_FILE`): guest RAM is mapped `MAP_PRIVATE` (CoW) from the
/// RAM file, the kernel load + boot setup are skipped, and VM/vCPU KVM state is
/// restored from this file before the vCPUs resume.
pub fn restore_state_path() -> Option<String> {
    std::env::var("KRUN_RESTORE_FROM").ok().filter(|p| !p.is_empty())
}

/// Read a snapshot state file written by [`write_state_file`].
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn read_state_file(path: &str) -> std::io::Result<SnapshotState> {
    let bytes = std::fs::read(path)?;
    bincode::deserialize(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Flush the file-backed guest RAM to disk (fsync of the MAP_SHARED file).
pub fn sync_mem_backing() -> std::io::Result<()> {
    let guard = MEM_BACKING.lock().unwrap();
    match guard.as_ref() {
        Some(backing) => backing.file.sync_all(),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "guest RAM is not file-backed (KRUN_SNAPSHOT_MEM_FILE unset)",
        )),
    }
}

/// Serialized snapshot: KVM VM state + per-vCPU state + the RAM region layout.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SnapshotState {
    pub vm_state: crate::linux::vstate::VmState,
    pub vcpu_states: Vec<crate::linux::vstate::VcpuState>,
    /// RAM file layout: (guest_addr_raw, size, file_offset) per region.
    pub mem_layout: Vec<(u64, usize, u64)>,
}

/// Write the snapshot state file (bincode).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn write_state_file(
    path: &str,
    vm_state: &crate::linux::vstate::VmState,
    vcpu_states: &[crate::linux::vstate::VcpuState],
) -> std::io::Result<()> {
    let mem_layout = MEM_BACKING
        .lock()
        .unwrap()
        .as_ref()
        .map(|b| {
            b.layout
                .iter()
                .map(|(addr, size, off)| (addr.0, *size, *off))
                .collect()
        })
        .unwrap_or_default();

    // SnapshotState borrows nothing: serialize via a reference-shaped tuple to
    // avoid cloning large vCPU states.
    #[derive(serde::Serialize)]
    struct SnapshotStateRef<'a> {
        vm_state: &'a crate::linux::vstate::VmState,
        vcpu_states: &'a [crate::linux::vstate::VcpuState],
        mem_layout: Vec<(u64, usize, u64)>,
    }

    let bytes = bincode::serialize(&SnapshotStateRef {
        vm_state,
        vcpu_states,
        mem_layout,
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, bytes)
}
