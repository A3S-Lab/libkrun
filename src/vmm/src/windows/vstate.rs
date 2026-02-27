// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter};
use std::result;

use crossbeam_channel::Sender;
use vm_memory::{Address, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};
use windows::Win32::System::Hypervisor::*;

use super::whpx_vcpu::{VcpuExit, VcpuEmulation, WhpxVcpu};

#[cfg(target_arch = "x86_64")]
use std::io;

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
            let mut property: WHV_PARTITION_PROPERTY = std::mem::zeroed();
            property.Anonymous.ProcessorCount = 1;
            WHvSetPartitionProperty(
                partition,
                WHvPartitionPropertyCodeProcessorCount,
                &property as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
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

/// Encapsulates configuration parameters for the guest vCPUS.
#[derive(Debug, Eq, PartialEq)]
pub struct VcpuConfig {
    /// Number of guest VCPUs.
    pub vcpu_count: u8,
    /// Enable hyperthreading in the CPUID configuration.
    pub ht_enabled: bool,
    /// CPUID template to use.
    pub cpu_template: Option<crate::vmm_config::machine_config::CpuFeaturesTemplate>,
}

/// A wrapper around creating and using a WHPX VCPU.
pub struct Vcpu {
    id: u8,
    /// The WHPX virtual CPU implementation
    #[cfg(target_arch = "x86_64")]
    whpx_vcpu: WhpxVcpu,
    #[cfg(target_arch = "x86_64")]
    partition: WHV_PARTITION_HANDLE,
    boot_entry_addr: u64,
    boot_receiver: Option<crossbeam_channel::Receiver<u64>>,
    boot_senders: Option<std::collections::HashMap<u64, crossbeam_channel::Sender<u64>>>,
    fdt_addr: u64,
    mmio_bus: Option<devices::Bus>,
    exit_evt: utils::eventfd::EventFd,
    mpidr: u64,
    event_receiver: crossbeam_channel::Receiver<VcpuEvent>,
    event_sender: Option<crossbeam_channel::Sender<VcpuEvent>>,
    response_receiver: Option<crossbeam_channel::Receiver<VcpuResponse>>,
    response_sender: crossbeam_channel::Sender<VcpuResponse>,
    vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
    nested_enabled: bool,
}

impl Vcpu {
    /// Constructs a new VCPU for `vm`.
    pub fn new_aarch64(
        id: u8,
        boot_entry_addr: vm_memory::GuestAddress,
        boot_receiver: Option<crossbeam_channel::Receiver<u64>>,
        exit_evt: utils::eventfd::EventFd,
        vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
        nested_enabled: bool,
    ) -> Result<Self> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let (response_sender, response_receiver) = crossbeam_channel::unbounded();

        Ok(Vcpu {
            id,
            boot_entry_addr: boot_entry_addr.raw_value(),
            boot_receiver,
            boot_senders: None,
            fdt_addr: 0,
            mmio_bus: None,
            exit_evt,
            mpidr: id as u64,
            event_receiver,
            event_sender: Some(event_sender),
            response_receiver: Some(response_receiver),
            response_sender,
            vcpu_list,
            nested_enabled,
        })
    }

    /// Constructs a new x86_64 VCPU for `vm`.
    #[cfg(target_arch = "x86_64")]
    pub fn new(
        id: u8,
        partition: WHV_PARTITION_HANDLE,
        exit_evt: utils::eventfd::EventFd,
        vcpu_list: std::sync::Arc<devices::legacy::VcpuList>,
        nested_enabled: bool,
    ) -> Result<Self> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let (response_sender, response_receiver) = crossbeam_channel::unbounded();

        let vcpu_index = id as u32;

        // Create the WHPX vCPU
        let whpx_vcpu = WhpxVcpu::new(partition, vcpu_index)
            .map_err(|e| {
                error!("Failed to create WHPX vCPU: {}", e);
                Error::VcpuSpawn(e)
            })?;

        // Initialize basic x86_64 registers
        let mut reg_names = [
            WHV_REGISTER_NAME(WHvX64RegisterRip.0),
            WHV_REGISTER_NAME(WHvX64RegisterRsp.0),
            WHV_REGISTER_NAME(WHvX64RegisterRflags.0),
        ];

        let mut reg_values = [
            WHV_REGISTER_VALUE { Reg64: 0x0 },  // RIP = 0x0
            WHV_REGISTER_VALUE { Reg64: 0x0 },  // RSP = 0x0
            WHV_REGISTER_VALUE { Reg64: 0x2 },  // RFLAGS = 0x2 (reserved bit)
        ];

        unsafe {
            WHvSetVirtualProcessorRegisters(
                partition,
                vcpu_index,
                reg_names.as_ptr(),
                3,
                reg_values.as_ptr(),
            ).map_err(|e| {
                error!("Failed to set registers: {}", e);
                Error::VcpuSpawn(io::Error::new(io::ErrorKind::Other, format!("Failed to set registers: {}", e)))
            })?;
        }

        Ok(Vcpu {
            id,
            whpx_vcpu,
            partition,
            boot_entry_addr: 0,
            boot_receiver: None,
            boot_senders: None,
            fdt_addr: 0,
            mmio_bus: None,
            exit_evt,
            mpidr: id as u64,
            event_receiver,
            event_sender: Some(event_sender),
            response_receiver: Some(response_receiver),
            response_sender,
            vcpu_list,
            nested_enabled,
        })
    }

    /// Returns the cpu index as seen by the guest OS.
    pub fn cpu_index(&self) -> u8 {
        self.id
    }

    /// Gets the MPIDR register value.
    pub fn get_mpidr(&self) -> u64 {
        self.mpidr
    }

    /// Sets a MMIO bus for this vcpu.
    pub fn set_mmio_bus(&mut self, mmio_bus: devices::Bus) {
        self.mmio_bus = Some(mmio_bus);
    }

    pub fn set_boot_senders(
        &mut self,
        boot_senders: std::collections::HashMap<u64, crossbeam_channel::Sender<u64>>,
    ) {
        self.boot_senders = Some(boot_senders);
    }

    /// Configures an aarch64 specific vcpu.
    pub fn configure_aarch64(&mut self, mem_info: &arch::ArchMemoryInfo) -> Result<()> {
        self.fdt_addr = mem_info.fdt_addr;
        Ok(())
    }

    /// Moves the vcpu to its own thread and constructs a VcpuHandle.
    pub fn start_threaded(mut self) -> Result<VcpuHandle> {
        let event_sender = self.event_sender.take().unwrap();
        let response_receiver = self.response_receiver.take().unwrap();
        let (init_tls_sender, init_tls_receiver) = crossbeam_channel::unbounded::<bool>();

        let vcpu_thread = std::thread::Builder::new()
            .name(format!("fc_vcpu {}", self.cpu_index()))
            .spawn(move || {
                init_tls_sender
                    .send(true)
                    .expect("Cannot notify vcpu TLS initialization.");
                // TODO: Implement WHPX vCPU run loop
            })
            .map_err(Error::VcpuSpawn)?;

        init_tls_receiver
            .recv()
            .expect("Error waiting for TLS initialization.");

        Ok(VcpuHandle::new(
            event_sender,
            response_receiver,
            vcpu_thread,
        ))
    }

    fn exit(&mut self, exit_code: u8) {
        self.response_sender
            .send(VcpuResponse::Exited(exit_code))
            .expect("failed to send Exited status");

        if let Err(e) = self.exit_evt.write(1) {
            error!("Failed signaling vcpu exit event: {e}");
        }
    }

    /// Handles a VM exit by delegating to the appropriate device.
    ///
    /// # Arguments
    /// * `exit` - The VM exit to handle
    ///
    /// # Returns
    /// Returns how the VMM should proceed after handling the exit.
    #[cfg(target_arch = "x86_64")]
    pub fn run_emulation(&mut self, exit: VcpuExit) -> VcpuEmulation {
        match exit {
            VcpuExit::MmioRead(addr, data) => {
                // Delegate to MMIO bus for MMIO read
                if let Some(mmio_bus) = &self.mmio_bus {
                    if mmio_bus.read(self.id as u64, addr, data) {
                        return VcpuEmulation::Handled;
                    }
                }
                VcpuEmulation::Stopped
            }
            VcpuExit::MmioWrite(addr, data) => {
                // Delegate to MMIO bus for MMIO write
                if let Some(mmio_bus) = &self.mmio_bus {
                    if mmio_bus.write(self.id as u64, addr, data) {
                        return VcpuEmulation::Handled;
                    }
                }
                VcpuEmulation::Stopped
            }
            VcpuExit::IoPortRead(port, data) => {
                // Delegate to MMIO bus for IO port read
                if let Some(mmio_bus) = &self.mmio_bus {
                    if mmio_bus.read(self.id as u64, port as u64, data) {
                        return VcpuEmulation::Handled;
                    }
                }
                VcpuEmulation::Stopped
            }
            VcpuExit::IoPortWrite(port, data) => {
                // Delegate to MMIO bus for IO port write
                if let Some(mmio_bus) = &self.mmio_bus {
                    if mmio_bus.write(self.id as u64, port as u64, data) {
                        return VcpuEmulation::Handled;
                    }
                }
                VcpuEmulation::Stopped
            }
            VcpuExit::Halted => VcpuEmulation::Halted,
            VcpuExit::Shutdown => VcpuEmulation::Stopped,
        }
    }

    /// Main vCPU run loop for x86_64.
    ///
    /// Continuously runs the vCPU, handling exits until the VM stops or halts.
    ///
    /// # Returns
    /// Returns the final emulation state (Stopped or Halted).
    ///
    /// # Errors
    /// Returns an error if the vCPU fails to run.
    #[cfg(target_arch = "x86_64")]
    pub fn run(&mut self) -> Result<VcpuEmulation, std::io::Error> {
        loop {
            let exit = self.whpx_vcpu.run()?;
            let emulation = self.run_emulation(exit);

            match emulation {
                VcpuEmulation::Handled => continue,
                VcpuEmulation::Stopped | VcpuEmulation::Halted => return Ok(emulation),
            }
        }
    }
}

/// Wrapper over Vcpu that hides the underlying interactions with the Vcpu thread.
pub struct VcpuHandle {
    event_sender: crossbeam_channel::Sender<VcpuEvent>,
    response_receiver: crossbeam_channel::Receiver<VcpuResponse>,
    #[allow(dead_code)]
    vcpu_thread: std::thread::JoinHandle<()>,
}

impl VcpuHandle {
    pub fn new(
        event_sender: crossbeam_channel::Sender<VcpuEvent>,
        response_receiver: crossbeam_channel::Receiver<VcpuResponse>,
        vcpu_thread: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            event_sender,
            response_receiver,
            vcpu_thread,
        }
    }

    pub fn send_event(&self, event: VcpuEvent) -> Result<()> {
        self.event_sender
            .send(event)
            .map_err(|_| Error::VcpuRun)
    }

    pub fn response_receiver(&self) -> &crossbeam_channel::Receiver<VcpuResponse> {
        &self.response_receiver
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
