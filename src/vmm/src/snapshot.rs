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
    std::env::var("KRUN_SNAPSHOT_MEM_FILE")
        .ok()
        .filter(|p| !p.is_empty())
}

/// The unix socket on which to serve snapshot requests, if enabled.
pub fn trigger_sock_path() -> Option<String> {
    std::env::var("KRUN_SNAPSHOT_SOCK")
        .ok()
        .filter(|p| !p.is_empty())
}

/// When set, boot is a RESTORE from this state file (paired with the RAM file in
/// `KRUN_SNAPSHOT_MEM_FILE`): guest RAM is mapped `MAP_PRIVATE` (CoW) from the
/// RAM file, the kernel load + boot setup are skipped, and VM/vCPU KVM state is
/// restored from this file before the vCPUs resume.
pub fn restore_state_path() -> Option<String> {
    std::env::var("KRUN_RESTORE_FROM")
        .ok()
        .filter(|p| !p.is_empty())
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

/// Serde mirror of `devices::virtio::QueueState` (the `devices` crate is serde-free).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct QueueStateSer {
    pub size: u16,
    pub ready: bool,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,
    pub next_avail: u16,
    pub next_used: u16,
    pub event_idx_enabled: bool,
    pub num_added: u16,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl From<&devices::virtio::QueueState> for QueueStateSer {
    fn from(q: &devices::virtio::QueueState) -> Self {
        Self {
            size: q.size,
            ready: q.ready,
            desc_table: q.desc_table,
            avail_ring: q.avail_ring,
            used_ring: q.used_ring,
            next_avail: q.next_avail,
            next_used: q.next_used,
            event_idx_enabled: q.event_idx_enabled,
            num_added: q.num_added,
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl From<&QueueStateSer> for devices::virtio::QueueState {
    fn from(q: &QueueStateSer) -> Self {
        Self {
            size: q.size,
            ready: q.ready,
            desc_table: q.desc_table,
            avail_ring: q.avail_ring,
            used_ring: q.used_ring,
            next_avail: q.next_avail,
            next_used: q.next_used,
            event_idx_enabled: q.event_idx_enabled,
            num_added: q.num_added,
        }
    }
}

/// Serde mirror of `devices::virtio::MmioDeviceState`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MmioDeviceStateSer {
    pub device_type: u32,
    pub features_select: u32,
    pub acked_features_select: u32,
    pub queue_select: u32,
    pub device_status: u32,
    pub config_generation: u32,
    pub acked_features: u64,
    pub queues: Vec<QueueStateSer>,
    #[serde(default)]
    pub device_blob: Vec<u8>,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl From<&devices::virtio::MmioDeviceState> for MmioDeviceStateSer {
    fn from(d: &devices::virtio::MmioDeviceState) -> Self {
        Self {
            device_type: d.device_type,
            features_select: d.features_select,
            acked_features_select: d.acked_features_select,
            queue_select: d.queue_select,
            device_status: d.device_status,
            config_generation: d.config_generation,
            acked_features: d.acked_features,
            queues: d.queues.iter().map(QueueStateSer::from).collect(),
            device_blob: d.device_blob.clone(),
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl From<&MmioDeviceStateSer> for devices::virtio::MmioDeviceState {
    fn from(d: &MmioDeviceStateSer) -> Self {
        Self {
            device_type: d.device_type,
            features_select: d.features_select,
            acked_features_select: d.acked_features_select,
            queue_select: d.queue_select,
            device_status: d.device_status,
            config_generation: d.config_generation,
            acked_features: d.acked_features,
            queues: d
                .queues
                .iter()
                .map(devices::virtio::QueueState::from)
                .collect(),
            device_blob: d.device_blob.clone(),
        }
    }
}

/// Serialized snapshot: KVM VM state + per-vCPU state + RAM layout + device state.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SnapshotState {
    pub vm_state: crate::linux::vstate::VmState,
    pub vcpu_states: Vec<crate::linux::vstate::VcpuState>,
    /// RAM file layout: (guest_addr_raw, size, file_offset) per region.
    pub mem_layout: Vec<(u64, usize, u64)>,
    /// virtio device state keyed by MMIO base address.
    #[serde(default)]
    pub device_states: Vec<(u64, MmioDeviceStateSer)>,
}

/// Write the snapshot state file (bincode).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn write_state_file(
    path: &str,
    vm_state: &crate::linux::vstate::VmState,
    vcpu_states: &[crate::linux::vstate::VcpuState],
    device_states: &[(u64, devices::virtio::MmioDeviceState)],
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

    let device_states: Vec<(u64, MmioDeviceStateSer)> = device_states
        .iter()
        .map(|(addr, d)| (*addr, MmioDeviceStateSer::from(d)))
        .collect();

    // SnapshotState borrows nothing: serialize via a reference-shaped tuple to
    // avoid cloning large vCPU states.
    #[derive(serde::Serialize)]
    struct SnapshotStateRef<'a> {
        vm_state: &'a crate::linux::vstate::VmState,
        vcpu_states: &'a [crate::linux::vstate::VcpuState],
        mem_layout: Vec<(u64, usize, u64)>,
        device_states: Vec<(u64, MmioDeviceStateSer)>,
    }

    let bytes = bincode::serialize(&SnapshotStateRef {
        vm_state,
        vcpu_states,
        mem_layout,
        device_states,
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, bytes)
}
