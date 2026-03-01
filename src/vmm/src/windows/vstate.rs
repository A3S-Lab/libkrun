// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter};
use std::io;
use std::result;
use std::thread;
use std::time::Duration;

use vm_memory::{Address, Bytes, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};
use windows::Win32::System::Hypervisor::*;

use super::whpx_vcpu::{VcpuEmulation, VcpuExit, WhpxVcpu};
use crate::{FC_EXIT_CODE_GENERIC_ERROR, FC_EXIT_CODE_OK};

// Boot-time x86_64 memory layout.
const BOOT_GDT_OFFSET: u64 = 0x500;
const BOOT_IDT_OFFSET: u64 = 0x520;
const PML4_START: u64 = 0x9000;
const PDPTE_START: u64 = 0xA000;
const PDE_START: u64 = 0xB000;

const BOOT_GDT_MAX: usize = 4;

const EFER_LMA: u64 = 0x400;
const EFER_LME: u64 = 0x100;
const X86_CR0_PE: u64 = 0x1;
const X86_CR0_PG: u64 = 0x8000_0000;
const X86_CR4_PAE: u64 = 0x20;

/// Errors associated with WHPX operations.
#[derive(Debug)]
pub enum Error {
    /// Invalid guest memory configuration.
    GuestMemoryMmap(vm_memory::GuestMemoryError),
    /// Cannot set the memory regions.
    SetUserMemoryRegion,
    /// Cannot configure the microvm.
    VmSetup,
    /// Cannot configure vCPU state.
    VcpuConfigure,
    /// Cannot run the VCPUs.
    VcpuRun,
    /// Cannot spawn a new vCPU thread.
    VcpuSpawn(std::io::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Error::GuestMemoryMmap(e) => write!(f, "Guest memory error: {e:?}"),
            Error::SetUserMemoryRegion => write!(f, "Cannot set the memory regions"),
            Error::VmSetup => write!(f, "Cannot configure the microvm"),
            Error::VcpuConfigure => write!(f, "Cannot configure the VCPU"),
            Error::VcpuRun => write!(f, "Cannot run the VCPUs"),
            Error::VcpuSpawn(e) => write!(f, "Cannot spawn a new vCPU thread: {e}"),
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

fn write_boot_state_to_guest(guest_mem: &GuestMemoryMmap) -> Result<()> {
    let gdt_table: [u64; BOOT_GDT_MAX] = [
        0x0000_0000_0000_0000,
        0x00AF_9B00_0000_FFFF,
        0x00CF_9300_0000_FFFF,
        0x008F_8B00_0000_FFFF,
    ];

    for (index, entry) in gdt_table.iter().enumerate() {
        let addr = guest_mem
            .checked_offset(
                GuestAddress(BOOT_GDT_OFFSET),
                index * std::mem::size_of::<u64>(),
            )
            .ok_or(Error::VcpuConfigure)?;
        guest_mem
            .write_obj(*entry, addr)
            .map_err(|_| Error::VcpuConfigure)?;
    }

    guest_mem
        .write_obj(0_u64, GuestAddress(BOOT_IDT_OFFSET))
        .map_err(|_| Error::VcpuConfigure)?;

    guest_mem
        .write_obj(PDPTE_START | 0x03, GuestAddress(PML4_START))
        .map_err(|_| Error::VcpuConfigure)?;
    guest_mem
        .write_obj(PDE_START | 0x03, GuestAddress(PDPTE_START))
        .map_err(|_| Error::VcpuConfigure)?;

    for i in 0..512 {
        guest_mem
            .write_obj(
                (i << 21) as u64 | 0x83,
                GuestAddress(PDE_START + (i * 8) as u64),
            )
            .map_err(|_| Error::VcpuConfigure)?;
    }

    Ok(())
}

/// A wrapper around creating and using a WHPX VM.
pub struct Vm {
    partition: WHV_PARTITION_HANDLE,
}

impl Vm {
    /// Constructs a new `Vm` using WHPX.
    pub fn new(_nested_enabled: bool, vcpu_count: u32) -> Result<Self> {
        unsafe {
            let partition = WHvCreatePartition().map_err(|_| Error::VmSetup)?;

            let mut property: WHV_PARTITION_PROPERTY = std::mem::zeroed();
            property.ProcessorCount = vcpu_count;
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
                .map_err(Error::GuestMemoryMmap)?;

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
    whpx_vcpu: WhpxVcpu,
    partition: WHV_PARTITION_HANDLE,
    guest_mem: GuestMemoryMmap,
    boot_entry_addr: u64,
    io_bus: devices::Bus,
    mmio_bus: Option<devices::Bus>,
    exit_evt: utils::eventfd::EventFd,
    event_receiver: crossbeam_channel::Receiver<VcpuEvent>,
    event_sender: Option<crossbeam_channel::Sender<VcpuEvent>>,
    response_receiver: Option<crossbeam_channel::Receiver<VcpuResponse>>,
    response_sender: crossbeam_channel::Sender<VcpuResponse>,
}

impl Vcpu {
    /// Registers a signal handler for kicking vCPUs.
    ///
    /// WHPX backend currently relies on synchronous exit handling, so this is a no-op.
    pub fn register_kick_signal_handler() {}

    /// Constructs a new x86_64 VCPU for `vm`.
    pub fn new(
        id: u8,
        partition: WHV_PARTITION_HANDLE,
        guest_mem: GuestMemoryMmap,
        boot_entry_addr: GuestAddress,
        io_bus: devices::Bus,
        exit_evt: utils::eventfd::EventFd,
    ) -> Result<Self> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let (response_sender, response_receiver) = crossbeam_channel::unbounded();

        let whpx_vcpu = WhpxVcpu::new(partition, id as u32).map_err(Error::VcpuSpawn)?;

        Ok(Vcpu {
            id,
            whpx_vcpu,
            partition,
            guest_mem,
            boot_entry_addr: boot_entry_addr.raw_value(),
            io_bus,
            mmio_bus: None,
            exit_evt,
            event_receiver,
            event_sender: Some(event_sender),
            response_receiver: Some(response_receiver),
            response_sender,
        })
    }

    /// Returns the cpu index as seen by the guest OS.
    pub fn cpu_index(&self) -> u8 {
        self.id
    }

    /// Sets a MMIO bus for this vcpu.
    pub fn set_mmio_bus(&mut self, mmio_bus: devices::Bus) {
        self.mmio_bus = Some(mmio_bus);
    }

    /// Configures x86_64 boot registers and tables for this vCPU.
    pub fn configure_x86_64(
        &mut self,
        guest_mem: &GuestMemoryMmap,
        kernel_start_addr: GuestAddress,
    ) -> Result<()> {
        self.write_boot_state(guest_mem)?;

        let code_seg = WHV_X64_SEGMENT_REGISTER {
            Base: 0,
            Limit: 0xFFFFF,
            Selector: 0x08,
            Anonymous: WHV_X64_SEGMENT_REGISTER_0 { Attributes: 0xA09B },
        };
        let data_seg = WHV_X64_SEGMENT_REGISTER {
            Base: 0,
            Limit: 0xFFFFF,
            Selector: 0x10,
            Anonymous: WHV_X64_SEGMENT_REGISTER_0 { Attributes: 0xC093 },
        };
        let tss_seg = WHV_X64_SEGMENT_REGISTER {
            Base: 0,
            Limit: 0xFFFFF,
            Selector: 0x18,
            Anonymous: WHV_X64_SEGMENT_REGISTER_0 { Attributes: 0x808B },
        };

        let gdtr = WHV_X64_TABLE_REGISTER {
            Pad: [0; 3],
            Limit: (BOOT_GDT_MAX * std::mem::size_of::<u64>() - 1) as u16,
            Base: BOOT_GDT_OFFSET,
        };
        let idtr = WHV_X64_TABLE_REGISTER {
            Pad: [0; 3],
            Limit: (std::mem::size_of::<u64>() - 1) as u16,
            Base: BOOT_IDT_OFFSET,
        };

        let reg_names = [
            WHvX64RegisterRip,
            WHvX64RegisterRsp,
            WHvX64RegisterRbp,
            WHvX64RegisterRsi,
            WHvX64RegisterRflags,
            WHvX64RegisterCs,
            WHvX64RegisterDs,
            WHvX64RegisterEs,
            WHvX64RegisterFs,
            WHvX64RegisterGs,
            WHvX64RegisterSs,
            WHvX64RegisterTr,
            WHvX64RegisterGdtr,
            WHvX64RegisterIdtr,
            WHvX64RegisterCr0,
            WHvX64RegisterCr3,
            WHvX64RegisterCr4,
            WHvX64RegisterEfer,
        ];

        let reg_values = [
            WHV_REGISTER_VALUE {
                Reg64: kernel_start_addr.raw_value(),
            },
            WHV_REGISTER_VALUE {
                Reg64: arch::x86_64::layout::BOOT_STACK_POINTER,
            },
            WHV_REGISTER_VALUE {
                Reg64: arch::x86_64::layout::BOOT_STACK_POINTER,
            },
            WHV_REGISTER_VALUE {
                Reg64: arch::x86_64::layout::ZERO_PAGE_START,
            },
            WHV_REGISTER_VALUE { Reg64: 0x2 },
            WHV_REGISTER_VALUE { Segment: code_seg },
            WHV_REGISTER_VALUE { Segment: data_seg },
            WHV_REGISTER_VALUE { Segment: data_seg },
            WHV_REGISTER_VALUE { Segment: data_seg },
            WHV_REGISTER_VALUE { Segment: data_seg },
            WHV_REGISTER_VALUE { Segment: data_seg },
            WHV_REGISTER_VALUE { Segment: tss_seg },
            WHV_REGISTER_VALUE { Table: gdtr },
            WHV_REGISTER_VALUE { Table: idtr },
            WHV_REGISTER_VALUE {
                Reg64: X86_CR0_PE | X86_CR0_PG,
            },
            WHV_REGISTER_VALUE { Reg64: PML4_START },
            WHV_REGISTER_VALUE { Reg64: X86_CR4_PAE },
            WHV_REGISTER_VALUE {
                Reg64: EFER_LME | EFER_LMA,
            },
        ];

        unsafe {
            WHvSetVirtualProcessorRegisters(
                self.partition,
                self.id as u32,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_ptr(),
            )
            .map_err(|e| {
                error!("Failed to set x86_64 registers for vCPU {}: {e}", self.id);
                Error::VcpuConfigure
            })?;
        }

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

                let guest_mem = self.guest_mem.clone();
                if let Err(e) =
                    self.configure_x86_64(&guest_mem, GuestAddress(self.boot_entry_addr))
                {
                    error!("Failed to configure WHPX vCPU {}: {e}", self.id);
                    self.exit(FC_EXIT_CODE_GENERIC_ERROR);
                    return;
                }

                loop {
                    match self.run() {
                        Ok(VcpuEmulation::Halted) => thread::sleep(Duration::from_millis(1)),
                        Ok(VcpuEmulation::Stopped) => {
                            self.exit(FC_EXIT_CODE_OK);
                            break;
                        }
                        Ok(VcpuEmulation::Handled) => continue,
                        Err(e) => {
                            error!("Error running WHPX vCPU {}: {e}", self.id);
                            self.exit(FC_EXIT_CODE_GENERIC_ERROR);
                            break;
                        }
                    }
                }
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
    pub fn run_emulation(&mut self, exit: VcpuExit) -> VcpuEmulation {
        match exit {
            VcpuExit::MmioRead(addr, data) => {
                if let Some(mmio_bus) = &self.mmio_bus {
                    if mmio_bus.read(self.id as u64, addr, data) {
                        if let Err(e) = self.whpx_vcpu.complete_mmio_read(data) {
                            error!(
                                "Failed to complete WHPX MMIO read emulation on vCPU {}: {e}",
                                self.id
                            );
                            self.whpx_vcpu.clear_pending_mmio();
                            return VcpuEmulation::Stopped;
                        }
                        return VcpuEmulation::Handled;
                    }
                }
                self.whpx_vcpu.clear_pending_mmio();
                VcpuEmulation::Stopped
            }
            VcpuExit::MmioWrite(addr, data) => {
                if let Some(mmio_bus) = &self.mmio_bus {
                    if mmio_bus.write(self.id as u64, addr, data) {
                        if let Err(e) = self.whpx_vcpu.complete_mmio_write() {
                            error!(
                                "Failed to complete WHPX MMIO write emulation on vCPU {}: {e}",
                                self.id
                            );
                            self.whpx_vcpu.clear_pending_mmio();
                            return VcpuEmulation::Stopped;
                        }
                        return VcpuEmulation::Handled;
                    }
                }
                self.whpx_vcpu.clear_pending_mmio();
                VcpuEmulation::Stopped
            }
            VcpuExit::IoPortRead(port, data) => {
                if self.io_bus.read(self.id as u64, port as u64, data) {
                    if let Err(e) = self.whpx_vcpu.complete_io_read(data) {
                        error!(
                            "Failed to complete WHPX I/O read emulation on vCPU {}: {e}",
                            self.id
                        );
                        self.whpx_vcpu.clear_pending_io();
                        return VcpuEmulation::Stopped;
                    }
                    return VcpuEmulation::Handled;
                }
                self.whpx_vcpu.clear_pending_io();
                VcpuEmulation::Stopped
            }
            VcpuExit::IoPortWrite(port, data) => {
                if self.io_bus.write(self.id as u64, port as u64, data) {
                    if let Err(e) = self.whpx_vcpu.complete_io_write() {
                        error!(
                            "Failed to complete WHPX I/O write emulation on vCPU {}: {e}",
                            self.id
                        );
                        self.whpx_vcpu.clear_pending_io();
                        return VcpuEmulation::Stopped;
                    }
                    return VcpuEmulation::Handled;
                }
                self.whpx_vcpu.clear_pending_io();
                VcpuEmulation::Stopped
            }
            VcpuExit::Halted => {
                self.whpx_vcpu.clear_pending_mmio();
                self.whpx_vcpu.clear_pending_io();
                VcpuEmulation::Halted
            }
            VcpuExit::Shutdown => {
                self.whpx_vcpu.clear_pending_mmio();
                self.whpx_vcpu.clear_pending_io();
                VcpuEmulation::Stopped
            }
        }
    }

    /// Main vCPU run loop for x86_64.
    pub fn run(&mut self) -> result::Result<VcpuEmulation, io::Error> {
        loop {
            while let Ok(event) = self.event_receiver.try_recv() {
                match event {
                    VcpuEvent::Pause => {
                        self.response_sender
                            .send(VcpuResponse::Paused)
                            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;

                        loop {
                            match self.event_receiver.recv() {
                                Ok(VcpuEvent::Resume) => {
                                    self.response_sender
                                        .send(VcpuResponse::Resumed)
                                        .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
                                    break;
                                }
                                Ok(VcpuEvent::Pause) => {
                                    self.response_sender
                                        .send(VcpuResponse::Paused)
                                        .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
                                }
                                Err(_) => return Ok(VcpuEmulation::Stopped),
                            }
                        }
                    }
                    VcpuEvent::Resume => {
                        self.response_sender
                            .send(VcpuResponse::Resumed)
                            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
                    }
                }
            }

            let emulation = match self.whpx_vcpu.run()? {
                VcpuExit::MmioRead(addr, data) => {
                    if let Some(mmio_bus) = &self.mmio_bus {
                        if mmio_bus.read(self.id as u64, addr, data) {
                            let mut completion = [0_u8; 8];
                            completion[..data.len()].copy_from_slice(data);
                            let completion = &completion[..data.len()];
                            let _ = data;
                            if let Err(e) = self.whpx_vcpu.complete_mmio_read(completion) {
                                error!(
                                    "Failed to complete WHPX MMIO read emulation on vCPU {}: {e}",
                                    self.id
                                );
                                self.whpx_vcpu.clear_pending_mmio();
                                VcpuEmulation::Stopped
                            } else {
                                VcpuEmulation::Handled
                            }
                        } else {
                            self.whpx_vcpu.clear_pending_mmio();
                            VcpuEmulation::Stopped
                        }
                    } else {
                        self.whpx_vcpu.clear_pending_mmio();
                        VcpuEmulation::Stopped
                    }
                }
                VcpuExit::MmioWrite(addr, data) => {
                    if let Some(mmio_bus) = &self.mmio_bus {
                        if mmio_bus.write(self.id as u64, addr, data) {
                            let _ = data;
                            if let Err(e) = self.whpx_vcpu.complete_mmio_write() {
                                error!(
                                    "Failed to complete WHPX MMIO write emulation on vCPU {}: {e}",
                                    self.id
                                );
                                self.whpx_vcpu.clear_pending_mmio();
                                VcpuEmulation::Stopped
                            } else {
                                VcpuEmulation::Handled
                            }
                        } else {
                            self.whpx_vcpu.clear_pending_mmio();
                            VcpuEmulation::Stopped
                        }
                    } else {
                        self.whpx_vcpu.clear_pending_mmio();
                        VcpuEmulation::Stopped
                    }
                }
                VcpuExit::IoPortRead(port, data) => {
                    if self.io_bus.read(self.id as u64, port as u64, data) {
                        let mut completion = [0_u8; 8];
                        completion[..data.len()].copy_from_slice(data);
                        let completion = &completion[..data.len()];
                        let _ = data;
                        if let Err(e) = self.whpx_vcpu.complete_io_read(completion) {
                            error!(
                                "Failed to complete WHPX I/O read emulation on vCPU {}: {e}",
                                self.id
                            );
                            self.whpx_vcpu.clear_pending_io();
                            VcpuEmulation::Stopped
                        } else {
                            VcpuEmulation::Handled
                        }
                    } else {
                        self.whpx_vcpu.clear_pending_io();
                        VcpuEmulation::Stopped
                    }
                }
                VcpuExit::IoPortWrite(port, data) => {
                    if self.io_bus.write(self.id as u64, port as u64, data) {
                        let _ = data;
                        if let Err(e) = self.whpx_vcpu.complete_io_write() {
                            error!(
                                "Failed to complete WHPX I/O write emulation on vCPU {}: {e}",
                                self.id
                            );
                            self.whpx_vcpu.clear_pending_io();
                            VcpuEmulation::Stopped
                        } else {
                            VcpuEmulation::Handled
                        }
                    } else {
                        self.whpx_vcpu.clear_pending_io();
                        VcpuEmulation::Stopped
                    }
                }
                VcpuExit::Halted => {
                    self.whpx_vcpu.clear_pending_mmio();
                    self.whpx_vcpu.clear_pending_io();
                    VcpuEmulation::Halted
                }
                VcpuExit::Shutdown => {
                    self.whpx_vcpu.clear_pending_mmio();
                    self.whpx_vcpu.clear_pending_io();
                    VcpuEmulation::Stopped
                }
            };

            match emulation {
                VcpuEmulation::Handled => continue,
                VcpuEmulation::Stopped | VcpuEmulation::Halted => return Ok(emulation),
            }
        }
    }

    fn write_boot_state(&self, guest_mem: &GuestMemoryMmap) -> Result<()> {
        write_boot_state_to_guest(guest_mem)
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
        self.event_sender.send(event).map_err(|_| Error::VcpuRun)
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

#[cfg(test)]
mod tests {
    use super::*;
    use vm_memory::GuestAddress;

    #[test]
    fn test_error_display_messages() {
        assert!(Error::VmSetup
            .to_string()
            .contains("Cannot configure the microvm"));
        assert!(Error::VcpuRun.to_string().contains("Cannot run the VCPUs"));
        assert!(
            Error::VcpuSpawn(io::Error::new(io::ErrorKind::Other, "spawn"))
                .to_string()
                .contains("Cannot spawn a new vCPU thread")
        );
    }

    #[test]
    fn test_vcpu_handle_send_event_and_receive_response() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (response_tx, response_rx) = crossbeam_channel::unbounded();

        let worker = std::thread::spawn(move || {
            if let Ok(VcpuEvent::Resume) = event_rx.recv() {
                let _ = response_tx.send(VcpuResponse::Resumed);
            }
        });

        let handle = VcpuHandle::new(event_tx, response_rx, worker);
        handle.send_event(VcpuEvent::Resume).unwrap();

        let response = handle
            .response_receiver()
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        assert_eq!(response, VcpuResponse::Resumed);
    }

    #[test]
    fn test_vcpu_handle_send_event_closed_channel() {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (_response_tx, response_rx) = crossbeam_channel::unbounded();
        drop(event_rx);

        let worker = std::thread::spawn(|| {});
        let handle = VcpuHandle::new(event_tx, response_rx, worker);

        assert!(matches!(
            handle.send_event(VcpuEvent::Pause),
            Err(Error::VcpuRun)
        ));
    }

    #[test]
    fn test_write_boot_state_to_guest_populates_expected_entries() {
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20_000)]).unwrap();

        write_boot_state_to_guest(&guest_mem).unwrap();

        let gdt0 = guest_mem
            .read_obj::<u64>(GuestAddress(BOOT_GDT_OFFSET))
            .unwrap();
        let gdt1 = guest_mem
            .read_obj::<u64>(GuestAddress(BOOT_GDT_OFFSET + 8))
            .unwrap();
        let idt = guest_mem
            .read_obj::<u64>(GuestAddress(BOOT_IDT_OFFSET))
            .unwrap();
        let pml4e = guest_mem.read_obj::<u64>(GuestAddress(PML4_START)).unwrap();
        let pdpte = guest_mem
            .read_obj::<u64>(GuestAddress(PDPTE_START))
            .unwrap();
        let pde0 = guest_mem.read_obj::<u64>(GuestAddress(PDE_START)).unwrap();
        let pde1 = guest_mem
            .read_obj::<u64>(GuestAddress(PDE_START + 8))
            .unwrap();
        let pde_last = guest_mem
            .read_obj::<u64>(GuestAddress(PDE_START + (511 * 8) as u64))
            .unwrap();

        assert_eq!(gdt0, 0);
        assert_eq!(gdt1, 0x00AF_9B00_0000_FFFF);
        assert_eq!(idt, 0);
        assert_eq!(pml4e, PDPTE_START | 0x03);
        assert_eq!(pdpte, PDE_START | 0x03);
        assert_eq!(pde0, 0x83);
        assert_eq!(pde1, (1_u64 << 21) | 0x83);
        assert_eq!(pde_last, (511_u64 << 21) | 0x83);
    }

    #[test]
    fn test_write_boot_state_to_guest_fails_on_small_memory() {
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x1000)]).unwrap();

        assert!(matches!(
            write_boot_state_to_guest(&guest_mem),
            Err(Error::VcpuConfigure)
        ));
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vm_lifecycle_smoke() {
        let _vm = Vm::new(false, 1).unwrap();
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vm_memory_init_smoke() {
        let mut vm = Vm::new(false, 1).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20_000)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();
    }
}
