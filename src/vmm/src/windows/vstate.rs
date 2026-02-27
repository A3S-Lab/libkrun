// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter};
use std::result;

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
    // TODO: Add WHPX partition handle
}

impl Vm {
    /// Constructs a new `Vm` using WHPX
    pub fn new(_nested_enabled: bool) -> Result<Self> {
        // TODO: Call WHvCreatePartition
        Ok(Vm {})
    }

    /// Initializes the guest memory
    pub fn memory_init(&mut self, _guest_mem: &vm_memory::GuestMemoryMmap) -> Result<()> {
        // TODO: Call WHvMapGpaRange for each memory region
        Ok(())
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
