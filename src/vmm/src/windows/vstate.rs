// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter};
use std::result;

use crossbeam_channel::Sender;
use vm_memory::{Address, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};
use windows::Win32::System::Hypervisor::*;

/// Errors associated with WHPX operations
#[derive(Debug)]
pub enum Error {
    /// Invalid guest memory configuration
    GuestMemoryMmap(vm_memory::GuestMemoryError),
    /// Cannot set the memory regions
    SetUserMemoryRegion,
    /// Cannot configure the microvm
    VmSetup,
    /// Cannot run the VCPUs
    VcpuRun,
    /// Cannot spawn a new vCPU thread
    VcpuSpawn(std::io::Error),
    /// Vcpu not present in TLS
    VcpuTlsNotPresent,
    /// Cannot cleanly initialize vcpu TLS
    VcpuTlsInit,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Error::GuestMemoryMmap(e) => write!(f, "Guest memory error: {e:?}"),
            Error::SetUserMemoryRegion => write!(f, "Cannot set the memory regions"),
            Error::VmSetup => write!(f, "Cannot configure the microvm"),
            Error::VcpuRun => write!(f, "Cannot run the VCPUs"),
            Error::VcpuSpawn(e) => write!(f, "Cannot spawn a new vCPU thread: {e}"),
            Error::VcpuTlsNotPresent => write!(f, "Vcpu not present in TLS"),
            Error::VcpuTlsInit => write!(f, "Cannot clean init vcpu TLS"),
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

/// A wrapper around creating and using a WHPX VM
pub struct Vm {
    partition: WHV_PARTITION_HANDLE,
}

impl Vm {
    /// Constructs a new `Vm` using WHPX
    pub fn new(_nested_enabled: bool) -> Result<Self> {
        unsafe {
            let mut partition: WHV_PARTITION_HANDLE = std::mem::zeroed();
            WHvCreatePartition(&mut partition).map_err(|_| Error::VmSetup)?;

            // Set processor count to 1 initially (will be updated when vCPUs are created)
            let property = WHV_PARTITION_PROPERTY {
                ProcessorCount: 1,
                ..Default::default()
            };
            WHvSetPartitionProperty(
                partition,
                WHvPartitionPropertyCodeProcessorCount,
                &property as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<WHV_PARTITION_PROPERTY>() as u32,
            )
            .map_err(|_| {
                let _ = WHvDeletePartition(partition);
                Error::VmSetup
            })?;

            WHvSetupPartition(partition).map_err(|_| {
                let _ = WHvDeletePartition(partition);
                Error::VmSetup
            })?;

            Ok(Vm { partition })
        }
    }

    pub fn partition(&self) -> WHV_PARTITION_HANDLE {
        self.partition
    }

    /// Initializes the guest memory.
    pub fn memory_init(&mut self, guest_mem: &GuestMemoryMmap) -> Result<()> {
        for region in guest_mem.iter() {
            let host_addr = guest_mem
                .get_host_address(region.start_addr())
                .ok_or(Error::SetUserMemoryRegion)?;

            unsafe {
                WHvMapGpaRange(
                    self.partition,
                    host_addr as *const std::ffi::c_void,
                    region.start_addr().raw_value(),
                    region.len(),
                    WHV_MAP_GPA_RANGE_FLAGS(
                        WHvMapGpaRangeFlagRead.0
                            | WHvMapGpaRangeFlagWrite.0
                            | WHvMapGpaRangeFlagExecute.0,
                    ),
                )
                .map_err(|_| Error::SetUserMemoryRegion)?;
            }
        }
        Ok(())
    }

    pub fn add_mapping(
        &self,
        reply_sender: crossbeam_channel::Sender<bool>,
        host_addr: u64,
        guest_addr: u64,
        len: u64,
    ) {
        unsafe {
            // Unmap first in case there's an existing mapping
            let _ = WHvUnmapGpaRange(self.partition, guest_addr, len);

            match WHvMapGpaRange(
                self.partition,
                host_addr as *const std::ffi::c_void,
                guest_addr,
                len,
                WHV_MAP_GPA_RANGE_FLAGS(
                    WHvMapGpaRangeFlagRead.0
                        | WHvMapGpaRangeFlagWrite.0
                        | WHvMapGpaRangeFlagExecute.0,
                ),
            ) {
                Ok(_) => reply_sender.send(true).unwrap(),
                Err(e) => {
                    error!("Error adding memory map: {e:?}");
                    reply_sender.send(false).unwrap();
                }
            }
        }
    }

    pub fn remove_mapping(
        &self,
        reply_sender: crossbeam_channel::Sender<bool>,
        guest_addr: u64,
        len: u64,
    ) {
        unsafe {
            match WHvUnmapGpaRange(self.partition, guest_addr, len) {
                Ok(_) => reply_sender.send(true).unwrap(),
                Err(e) => {
                    error!("Error removing memory map: {e:?}");
                    reply_sender.send(false).unwrap();
                }
            }
        }
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        unsafe {
            let _ = WHvDeletePartition(self.partition);
        }
    }
}

/// A wrapper around creating and using a WHPX VCPU
pub struct Vcpu {
    id: u8,
}

impl Vcpu {
    /// Constructs a new VCPU for WHPX
    pub fn new_aarch64(
        id: u8,
        _boot_entry_addr: vm_memory::GuestAddress,
        _boot_receiver: Option<crossbeam_channel::Receiver<u64>>,
        _exit_evt: utils::eventfd::EventFd,
        _vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
        _nested_enabled: bool,
    ) -> Result<Self> {
        Ok(Vcpu { id })
    }

    /// Returns the cpu index
    pub fn cpu_index(&self) -> u8 {
        self.id
    }
}

/// Wrapper over Vcpu that hides the underlying interactions
pub struct VcpuHandle {
    // TODO: Add event channels
}

impl VcpuHandle {
    pub fn new(
        _event_sender: crossbeam_channel::Sender<VcpuEvent>,
        _response_receiver: crossbeam_channel::Receiver<VcpuResponse>,
        _vcpu_thread: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {}
    }
}

#[derive(Debug)]
pub enum VcpuEvent {
    Pause,
    Resume,
}

#[derive(Debug, Eq, PartialEq)]
pub enum VcpuResponse {
    Paused,
    Resumed,
    Exited(u8),
}
