// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter};
use std::io;
use std::result;

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

        let reg_values: [WHV_REGISTER_VALUE; 18] = unsafe {
            let mut v: [WHV_REGISTER_VALUE; 18] = std::mem::zeroed();
            v[0].Reg64 = kernel_start_addr.raw_value();
            v[1].Reg64 = arch::x86_64::layout::BOOT_STACK_POINTER;
            v[2].Reg64 = arch::x86_64::layout::BOOT_STACK_POINTER;
            v[3].Reg64 = arch::x86_64::layout::ZERO_PAGE_START;
            v[4].Reg64 = 0x2;
            v[5].Segment = code_seg;
            v[6].Segment = data_seg;
            v[7].Segment = data_seg;
            v[8].Segment = data_seg;
            v[9].Segment = data_seg;
            v[10].Segment = data_seg;
            v[11].Segment = tss_seg;
            v[12].Table = gdtr;
            v[13].Table = idtr;
            v[14].Reg64 = X86_CR0_PE | X86_CR0_PG;
            v[15].Reg64 = PML4_START;
            v[16].Reg64 = X86_CR4_PAE;
            v[17].Reg64 = EFER_LME | EFER_LMA;
            v
        };

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
                        Ok(VcpuEmulation::Halted) => {
                            self.exit(FC_EXIT_CODE_OK);
                            break;
                        }
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

            let io_bus_ptr = &self.io_bus as *const devices::Bus;
            let guest_mem_ptr = &self.guest_mem as *const GuestMemoryMmap;
            let vcpu_id = self.id as u64;
            let emulation = match self.whpx_vcpu.run(io_bus_ptr, guest_mem_ptr, vcpu_id)? {
                VcpuExit::MmioRead(addr, data) => {
                    // Always attempt the bus read; unregistered addresses leave
                    // data zeroed (bus default).  Mirrors the IO-port path which
                    // always returns Handled regardless of whether a device claimed
                    // the port.
                    if let Some(mmio_bus) = &self.mmio_bus {
                        mmio_bus.read(self.id as u64, addr, data);
                    }
                    // Copy data before releasing the borrow so complete_mmio_read
                    // can take &mut self.whpx_vcpu.
                    let mut completion = [0_u8; 8];
                    completion[..data.len()].copy_from_slice(data);
                    let len = data.len();
                    let _ = data;
                    if let Err(e) = self.whpx_vcpu.complete_mmio_read(&completion[..len]) {
                        error!(
                            "Failed to complete WHPX MMIO read on vCPU {}: {e}",
                            self.id
                        );
                        self.whpx_vcpu.clear_pending_mmio();
                        VcpuEmulation::Stopped
                    } else {
                        VcpuEmulation::Handled
                    }
                }
                VcpuExit::MmioWrite(addr, data) => {
                    // Always attempt the bus write; unregistered addresses are
                    // silently ignored.  Mirrors the IO-port path.
                    if let Some(mmio_bus) = &self.mmio_bus {
                        mmio_bus.write(self.id as u64, addr, data);
                    }
                    let _ = data;
                    if let Err(e) = self.whpx_vcpu.complete_mmio_write() {
                        error!(
                            "Failed to complete WHPX MMIO write on vCPU {}: {e}",
                            self.id
                        );
                        self.whpx_vcpu.clear_pending_mmio();
                        VcpuEmulation::Stopped
                    } else {
                        VcpuEmulation::Handled
                    }
                }
                VcpuExit::IoPortRead(port, data) => {
                    self.io_bus.read(self.id as u64, port as u64, data);
                    // Copy data to release the borrow on self.whpx_vcpu before
                    // calling complete_io_read.
                    let mut completion = [0_u8; 8];
                    completion[..data.len()].copy_from_slice(data);
                    let len = data.len();
                    let _ = data;
                    if let Err(e) = self.whpx_vcpu.complete_io_read(&completion[..len]) {
                        error!(
                            "Failed to complete WHPX I/O read emulation on vCPU {}: {e}",
                            self.id
                        );
                        self.whpx_vcpu.clear_pending_io();
                        VcpuEmulation::Stopped
                    } else {
                        VcpuEmulation::Handled
                    }
                }
                VcpuExit::IoPortWrite(port, data) => {
                    self.io_bus.write(self.id as u64, port as u64, data);
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
    use std::time::Duration;
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

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vcpu_create_smoke() {
        const MEM_SIZE: usize = 0x40_0000;
        let mut vm = Vm::new(false, 1).unwrap();
        let guest_mem =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();
        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let io_bus = devices::Bus::new();
        let _vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(0x10000),
            io_bus,
            exit_evt,
        )
        .unwrap();
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vcpu_configure_smoke() {
        const MEM_SIZE: usize = 0x40_0000;
        let mut vm = Vm::new(false, 1).unwrap();
        let guest_mem =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();
        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let io_bus = devices::Bus::new();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(0x10000),
            io_bus,
            exit_evt,
        )
        .unwrap();
        vcpu.configure_x86_64(&guest_mem, GuestAddress(0x10000))
            .unwrap();
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vm_hlt_boot() {
        const ENTRY_ADDR: u64 = 0x10000;
        const MEM_SIZE: usize = 0x40_0000; // 4 MB — page tables end at ~0xC000, code at 0x10000

        // 1. Create WHPX partition and map guest memory.
        let mut vm = Vm::new(false, 1).unwrap();
        let guest_mem =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();

        // 2. Place a single HLT (0xF4) at the entry point.
        guest_mem
            .write_obj::<u8>(0xF4, GuestAddress(ENTRY_ADDR))
            .unwrap();

        // 3. Build a minimal vCPU (no MMIO bus needed for a pure HLT test).
        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let io_bus = devices::Bus::new();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(ENTRY_ADDR),
            io_bus,
            exit_evt,
        )
        .unwrap();

        // 4. Set up long-mode boot state: GDT, IDT, PML4/PDPTE/PDE, all segment
        //    registers, CR0/CR3/CR4, EFER, RIP=ENTRY_ADDR.
        vcpu.configure_x86_64(&guest_mem, GuestAddress(ENTRY_ADDR))
            .unwrap();

        // 5. Run: guest executes HLT → WHvRunReasonX64Halt → VcpuEmulation::Halted.
        let result = vcpu.run().unwrap();
        assert_eq!(result, VcpuEmulation::Halted);
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vm_threaded_boot() {
        const ENTRY_ADDR: u64 = 0x10000;
        const MEM_SIZE: usize = 0x40_0000; // 4 MB

        // 1. Create WHPX partition and map guest memory.
        let mut vm = Vm::new(false, 1).unwrap();
        let guest_mem =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();

        // 2. Write a single `HLT` (F4) at the entry point.
        //    With the WHvEmulator fix, IO exits properly advance RIP; start_threaded()
        //    now treats Halted as terminal (exits with FC_EXIT_CODE_OK).
        guest_mem
            .write_obj::<u8>(0xF4, GuestAddress(ENTRY_ADDR))
            .unwrap();

        // 3. Build a minimal vCPU with an empty IO bus (no devices registered).
        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let io_bus = devices::Bus::new();
        let vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(ENTRY_ADDR),
            io_bus,
            exit_evt,
        )
        .unwrap();

        // 4. Launch the vCPU in its own thread (the production path).
        //    start_threaded() internally calls configure_x86_64() and then runs the
        //    vCPU loop: guest executes HLT → Halted → start_threaded() calls
        //    exit(FC_EXIT_CODE_OK) and breaks.
        let handle = vcpu.start_threaded().unwrap();

        // 5. Expect the thread to report a clean exit within 5 seconds.
        let response = handle
            .response_receiver()
            .recv_timeout(Duration::from_secs(5))
            .expect("vCPU thread did not respond within timeout");

        assert_eq!(response, VcpuResponse::Exited(FC_EXIT_CODE_OK));
    }

    #[test]
    fn test_elf_loader_smoke() {
        use linux_loader::loader::{Elf, KernelLoader};

        // Minimal ELF64 executable: one PT_LOAD segment at p_paddr=0x1000,
        // entry point e_entry=0x1000.  Total size = ELF header (64) + phdr (56) = 120 bytes.
        #[rustfmt::skip]
        let elf_bytes: &[u8] = &[
            // ELF header (64 bytes)
            0x7f, b'E', b'L', b'F',              // magic
            0x02,                                 // ELFCLASS64
            0x01,                                 // ELFDATA2LSB
            0x01,                                 // EV_CURRENT
            0x00,                                 // ELFOSABI_NONE
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // padding
            0x02, 0x00,                           // ET_EXEC
            0x3e, 0x00,                           // EM_X86_64
            0x01, 0x00, 0x00, 0x00,               // e_version = 1
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_entry = 0x1000
            0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_phoff = 64
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // e_shoff = 0
            0x00, 0x00, 0x00, 0x00,               // e_flags = 0
            0x40, 0x00,                           // e_ehsize = 64
            0x38, 0x00,                           // e_phentsize = 56
            0x01, 0x00,                           // e_phnum = 1
            0x40, 0x00,                           // e_shentsize = 64
            0x00, 0x00,                           // e_shnum = 0
            0x00, 0x00,                           // e_shstrndx = 0
            // Program header (56 bytes)
            0x01, 0x00, 0x00, 0x00,               // p_type = PT_LOAD
            0x05, 0x00, 0x00, 0x00,               // p_flags = PF_R|PF_X
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_offset = 0
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_vaddr = 0x1000
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_paddr = 0x1000
            0x78, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_filesz = 120
            0x78, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_memsz = 120
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // p_align = 0x1000
        ];
        assert_eq!(elf_bytes.len(), 120);

        let mem: GuestMemoryMmap =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10_000)]).unwrap();
        let mut cursor = std::io::Cursor::new(elf_bytes);
        let result = Elf::load(&mem, None, &mut cursor, None).unwrap();
        assert_eq!(result.kernel_load, GuestAddress(0x1000));
    }

    /// Minimal IO port write test: no ELF loading, no configure_system.
    /// Directly writes `OUT 0x30, AL; HLT` bytes and verifies:
    ///   IoPortWrite(0x30) → CaptureDevice → complete_io_write → HLT → Halted.
    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_io_port_write_smoke() {
        use std::sync::{Arc, Mutex};

        use devices::{Bus, BusDevice};

        const ENTRY_ADDR: u64 = 0x10000;
        const MEM_SIZE: usize = 0x40_0000;

        // Payload:  B0 48  mov al,'H'
        //           E6 30  out 0x30, al
        //           F4     hlt
        let payload: [u8; 5] = [0xB0, 0x48, 0xE6, 0x30, 0xF4];

        let mut vm = Vm::new(false, 1).unwrap();
        let guest_mem =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();

        for (i, b) in payload.iter().enumerate() {
            guest_mem
                .write_obj::<u8>(*b, GuestAddress(ENTRY_ADDR + i as u64))
                .unwrap();
        }

        struct CaptureDevice {
            captured: Vec<u8>,
        }
        impl BusDevice for CaptureDevice {
            fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
                if offset == 0 {
                    self.captured.extend_from_slice(data);
                }
            }
        }
        let capture = Arc::new(Mutex::new(CaptureDevice {
            captured: Vec::new(),
        }));
        let mut io_bus = Bus::new();
        io_bus.insert(capture.clone(), 0x30, 0x1).unwrap();

        let exit_evt =
            utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(ENTRY_ADDR),
            io_bus,
            exit_evt,
        )
        .unwrap();
        vcpu.configure_x86_64(&guest_mem, GuestAddress(ENTRY_ADDR))
            .unwrap();

        let result = vcpu.run().unwrap();
        // With WHvEmulatorTryIoEmulation, RIP is correctly advanced past the OUT
        // instruction, so the HLT at 0x10004 is reached and the run ends with Halted.
        assert_eq!(
            result,
            VcpuEmulation::Halted,
            "expected Halted after IO write + HLT (emulator path)"
        );

        let captured = capture.lock().unwrap();
        assert!(!captured.captured.is_empty(), "no bytes captured on port 0x30");
        assert_eq!(captured.captured[0], b'H', "expected 'H' on port 0x30");
    }

    /// Full closed-loop integration test:
    ///   ELF::load → configure_system → configure_x86_64 → run → IO capture → HLT
    ///
    /// The ELF payload is a 5-byte bare-metal stub:
    ///   B0 48        mov al, 'H'
    ///   E6 30        out 0x30, al   ; port 0x30 — not in string-IO fallback list
    ///   F4           hlt
    ///
    /// Port 0x30 is chosen because it is outside the COM1/COM2/COM3/COM4 ranges
    /// that trigger `allow_string_io_fallback`, ensuring a clean IoPortWrite exit.
    /// A CaptureDevice on the IO bus at 0x30 records the byte so we can assert it.
    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_minimal_kernel_boot() {
        use std::sync::{Arc, Mutex};

        use devices::{Bus, BusDevice};
        use linux_loader::loader::{Elf, KernelLoader};

        // ── 1. Build ELF64 binary ──────────────────────────────────────────────
        // Layout: ELF header (64) + program header (56) + payload (5) = 125 bytes.
        // PT_LOAD: file offset 120 → guest paddr 0x1000, 5 bytes.
        //
        // Payload:  B0 48  mov al,'H'
        //           E6 30  out 0x30, al   (immediate port, 2 bytes)
        //           F4     hlt            (1 byte)
        let payload: [u8; 5] = [0xB0, 0x48, 0xE6, 0x30, 0xF4];

        #[rustfmt::skip]
        let mut elf_bytes: Vec<u8> = vec![
            // ELF header (64 bytes)
            0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // padding
            0x02, 0x00,                                        // ET_EXEC
            0x3e, 0x00,                                        // EM_X86_64
            0x01, 0x00, 0x00, 0x00,                            // e_version
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // e_entry = 0x1000
            0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // e_phoff = 64
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // e_shoff = 0
            0x00, 0x00, 0x00, 0x00,                            // e_flags
            0x40, 0x00,                                        // e_ehsize = 64
            0x38, 0x00,                                        // e_phentsize = 56
            0x01, 0x00,                                        // e_phnum = 1
            0x40, 0x00, 0x00, 0x00, 0x00, 0x00,               // e_shentsize/shnum/shstrndx
            // Program header (56 bytes): PT_LOAD at file offset 120 → paddr 0x1000
            0x01, 0x00, 0x00, 0x00,                            // p_type = PT_LOAD
            0x05, 0x00, 0x00, 0x00,                            // p_flags = PF_R|PF_X
            0x78, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // p_offset = 120
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // p_vaddr = 0x1000
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // p_paddr = 0x1000
            0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // p_filesz = 5
            0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // p_memsz = 5
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,   // p_align = 0x1000
        ];
        elf_bytes.extend_from_slice(&payload);
        assert_eq!(elf_bytes.len(), 125);

        // ── 2. WHPX partition + guest memory ──────────────────────────────────
        const MEM_SIZE: usize = 0x40_0000; // 4 MB
        let mut vm = Vm::new(false, 1).unwrap();
        let (arch_mem_info, arch_mem_regions) =
            arch::arch_memory_regions(MEM_SIZE, None, 0, 0, None);
        let guest_mem =
            GuestMemoryMmap::from_ranges(&arch_mem_regions).unwrap();
        vm.memory_init(&guest_mem).unwrap();

        // ── 3. Load ELF → kernel_entry ────────────────────────────────────────
        let mut cursor = std::io::Cursor::new(&elf_bytes);
        let load_result = Elf::load(&guest_mem, None, &mut cursor, None).unwrap();
        let kernel_entry = load_result.kernel_load;
        assert_eq!(kernel_entry, GuestAddress(0x1000));

        // ── 4. Write zero page (Linux boot protocol) ──────────────────────────
        arch::configure_system(
            &guest_mem,
            &arch_mem_info,
            GuestAddress(arch::x86_64::layout::CMDLINE_START),
            0,
            &None,
            1,
        )
        .unwrap();

        // ── 5. IO bus with capture device at port 0x30 ────────────────────────
        // Port 0x30 is outside all COM/debug port ranges that trigger the
        // allow_string_io_fallback path, ensuring a clean IoPortWrite exit.
        struct CaptureDevice {
            captured: Vec<u8>,
        }
        impl BusDevice for CaptureDevice {
            fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
                if offset == 0 {
                    self.captured.extend_from_slice(data);
                }
            }
        }
        let capture = Arc::new(Mutex::new(CaptureDevice {
            captured: Vec::new(),
        }));
        let mut io_bus = Bus::new();
        io_bus.insert(capture.clone(), 0x30, 0x1).unwrap();

        // ── 6. Create vCPU ────────────────────────────────────────────────────
        let exit_evt =
            utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            kernel_entry,
            io_bus,
            exit_evt,
        )
        .unwrap();

        // ── 7. Configure long-mode register state (RIP = kernel_entry) ────────
        vcpu.configure_x86_64(&guest_mem, kernel_entry).unwrap();

        // ── 8. Run: OUT 0x30 handled → RIP advanced by emulator → HLT → Halted ─
        let result = vcpu.run().unwrap();
        assert_eq!(
            result,
            VcpuEmulation::Halted,
            "expected Halted after IO write + HLT (emulator path)"
        );

        // ── 9. Assert 'H' was captured on port 0x30 ───────────────────────────
        let captured = capture.lock().unwrap();
        assert!(!captured.captured.is_empty(), "no bytes captured on port 0x30");
        assert_eq!(captured.captured[0], b'H', "expected 'H' on port 0x30");
    }

    /// COM1 serial boot test — exercises the `OUT DX, AL` instruction form used
    /// by real Linux kernels for early serial output.
    ///
    /// Port 0x3F8 (COM1) requires a 16-bit DX register in the OUT instruction
    /// because the port number exceeds 0xFF (the limit of the `OUT imm8, AL`
    /// encoding). This exercises a different instruction decode path than
    /// `OUT imm8, AL` used in `test_whpx_io_port_write_smoke`.
    ///
    /// Payload (9 bytes):
    ///   BA F8 03 00 00    mov edx, 0x3F8   ; COM1 base address (imm32 encoding)
    ///   B0 48             mov al, 'H'      ; character to send
    ///   EE                out dx, al       ; write to COM1 data register
    ///   F4                hlt
    ///
    /// CaptureDevice is registered at COM1 base (0x3F8, size 8).
    /// The test asserts 'H' is captured at offset 0 and the run ends with Halted.
    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vm_com1_serial_boot() {
        use std::sync::{Arc, Mutex};

        use devices::{Bus, BusDevice};

        const ENTRY_ADDR: u64 = 0x10000;
        const MEM_SIZE: usize = 0x40_0000;

        // Payload: mov edx,0x3F8 | mov al,'H' | out dx,al | hlt
        // Note: 0xBA is MOV EDX,imm32 (5-byte form) in 32/64-bit mode.
        // Using imm32 sets DX correctly without needing the 0x66 operand-size prefix.
        let payload: [u8; 9] = [0xBA, 0xF8, 0x03, 0x00, 0x00, 0xB0, 0x48, 0xEE, 0xF4];

        let mut vm = Vm::new(false, 1).unwrap();
        let guest_mem =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();

        for (i, b) in payload.iter().enumerate() {
            guest_mem
                .write_obj::<u8>(*b, GuestAddress(ENTRY_ADDR + i as u64))
                .unwrap();
        }

        struct CaptureDevice {
            captured: Vec<u8>,
        }
        impl BusDevice for CaptureDevice {
            fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
                if offset == 0 {
                    self.captured.extend_from_slice(data);
                }
            }
        }
        let capture = Arc::new(Mutex::new(CaptureDevice {
            captured: Vec::new(),
        }));
        let mut io_bus = Bus::new();
        // Register capture device at COM1 base (0x3F8), size 8 (0x3F8-0x3FF).
        io_bus.insert(capture.clone(), 0x3F8, 0x8).unwrap();

        let exit_evt =
            utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(ENTRY_ADDR),
            io_bus,
            exit_evt,
        )
        .unwrap();
        vcpu.configure_x86_64(&guest_mem, GuestAddress(ENTRY_ADDR))
            .unwrap();

        let result = vcpu.run().unwrap();
        assert_eq!(
            result,
            VcpuEmulation::Halted,
            "expected Halted after COM1 write + HLT"
        );

        let captured = capture.lock().unwrap();
        assert!(!captured.captured.is_empty(), "no bytes captured on COM1 (0x3F8)");
        assert_eq!(captured.captured[0], b'H', "expected 'H' on COM1 (0x3F8)");
    }

    // ── virtio-blk Windows backend smoke tests ──────────────────────────────

    /// Verify that `BlockWindows` can open a disk image, reports the correct
    /// capacity and features, and exposes them via the VirtioDevice trait.
    /// This test does NOT require WHPX and runs in the regular PR CI job.
    #[test]
    fn test_whpx_blk_init_smoke() {
        use std::io::Write;
        use devices::virtio::{BlockWindows, VirtioDevice};

        // Create a 2-sector (1 KiB) temp disk image.
        let dir = std::env::temp_dir();
        let path = dir.join("libkrun_whpx_blk_init_smoke.img");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            let mut sector0 = [0u8; 512];
            sector0[..8].copy_from_slice(b"LIBKRUN!");
            f.write_all(&sector0).unwrap();
            f.write_all(&[0u8; 512]).unwrap(); // sector 1 (zeroed)
        }

        let blk = BlockWindows::new("blk-smoke-init", path.to_str().unwrap(), true /* ro */)
            .expect("BlockWindows::new failed");

        // Device identity.
        assert_eq!(blk.id(), "blk-smoke-init");
        assert_eq!(blk.device_type(), 2); // VIRTIO_ID_BLOCK

        // Config space: capacity must be 2 sectors.
        let mut cfg = [0u8; 8];
        blk.read_config(0, &mut cfg);
        assert_eq!(
            u64::from_le_bytes(cfg),
            2,
            "config space capacity mismatch"
        );

        // Features: VIRTIO_F_VERSION_1 (bit 32), VIRTIO_BLK_F_FLUSH (bit 9),
        // VIRTIO_BLK_F_RO (bit 5) because the image is opened read-only.
        let features = blk.avail_features();
        assert_ne!(features & (1u64 << 32), 0, "VIRTIO_F_VERSION_1 not set");
        assert_ne!(features & (1u64 << 9), 0, "VIRTIO_BLK_F_FLUSH not set");
        assert_ne!(features & (1u64 << 5), 0, "VIRTIO_BLK_F_RO not set for ro disk");

        let _ = std::fs::remove_file(&path);
    }

    /// Verify that `BlockWindows` reads sector data correctly by constructing
    /// a minimal virtio-blk request in guest memory and processing the queue.
    /// This test does NOT require WHPX and runs in the regular PR CI job.
    #[test]
    fn test_whpx_blk_read_smoke() {
        use std::io::Write;
        use devices::virtio::{BlockWindows, InterruptTransport, VirtioDevice};
        use devices::legacy::DummyIrqChip;
        use std::sync::{Arc, Mutex};
        use vm_memory::{GuestAddress, GuestMemoryMmap};

        // ── 1. Prepare a disk image with a known sector-0 payload ───────────
        let dir = std::env::temp_dir();
        let path = dir.join("libkrun_whpx_blk_read_smoke.img");
        const MAGIC: &[u8; 8] = b"MAGICBLK";
        {
            let mut f = std::fs::File::create(&path).unwrap();
            let mut sector0 = [0u8; 512];
            sector0[..8].copy_from_slice(MAGIC);
            f.write_all(&sector0).unwrap();
        }

        let mut blk =
            BlockWindows::new("blk-smoke-read", path.to_str().unwrap(), true /* ro */)
                .expect("BlockWindows::new failed");

        // ── 2. Set up a 4 MiB guest memory region ───────────────────────────
        // Layout (all within the first 4 KiB):
        //   0x0000: virtio descriptor table (qsize=4 descriptors, 4×16=64 bytes)
        //   0x0100: avail ring
        //   0x0200: used ring
        //   0x1000: request header (16 bytes)
        //   0x1100: data buffer (512 bytes, write-only from device side)
        //   0x1200: status byte (1 byte, write-only from device side)
        const MEM_SIZE: usize = 4 << 20;
        let mem: GuestMemoryMmap =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();

        // ── 3. Virtio queue layout ───────────────────────────────────────────
        // desc[0] → request header  (read-only,  16 bytes) → flags=NEXT, next=1
        // desc[1] → data buffer     (write-only, 512 bytes) → flags=WRITE|NEXT, next=2
        // desc[2] → status byte     (write-only, 1 byte)   → flags=WRITE, next=0
        const DESC_TABLE: u64 = 0x0000;
        const AVAIL_RING: u64 = 0x0100;
        const USED_RING: u64 = 0x0200;
        const HDR_ADDR: u64 = 0x1000;
        const DATA_ADDR: u64 = 0x1100;
        const STATUS_ADDR: u64 = 0x1200;

        const VIRTQ_DESC_F_NEXT: u16 = 0x1;
        const VIRTQ_DESC_F_WRITE: u16 = 0x2;

        // Write descriptor table (each entry: addr(8) + len(4) + flags(2) + next(2) = 16 bytes).
        let write_desc = |idx: usize, addr: u64, len: u32, flags: u16, next: u16| {
            let base = GuestAddress(DESC_TABLE + idx as u64 * 16);
            mem.write_slice(&addr.to_le_bytes(), base).unwrap();
            mem.write_slice(&len.to_le_bytes(), GuestAddress(base.0 + 8)).unwrap();
            mem.write_slice(&flags.to_le_bytes(), GuestAddress(base.0 + 12)).unwrap();
            mem.write_slice(&next.to_le_bytes(), GuestAddress(base.0 + 14)).unwrap();
        };
        write_desc(0, HDR_ADDR, 16, VIRTQ_DESC_F_NEXT, 1);
        write_desc(1, DATA_ADDR, 512, VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT, 2);
        write_desc(2, STATUS_ADDR, 1, VIRTQ_DESC_F_WRITE, 0);

        // Write request header: type=IN(0), reserved=0, sector=0.
        const VIRTIO_BLK_T_IN: u32 = 0;
        let mut hdr = [0u8; 16];
        hdr[..4].copy_from_slice(&VIRTIO_BLK_T_IN.to_le_bytes());
        // sector at bytes [8..16] = 0 (already zero)
        mem.write_slice(&hdr, GuestAddress(HDR_ADDR)).unwrap();

        // Avail ring: flags=0(0x0100), idx=1(0x0102), ring[0]=0(0x0104)
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING)).unwrap();     // flags
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2)).unwrap(); // idx
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4)).unwrap(); // ring[0]=0 (desc idx)

        // Used ring: flags=0, idx=0 (device fills this).
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING)).unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2)).unwrap();

        // ── 4. Configure the queue on the Block device ───────────────────────
        {
            let q = &mut blk.queues_mut()[0];
            q.size = 4;
            q.ready = true;
            q.desc_table = GuestAddress(DESC_TABLE);
            q.avail_ring = GuestAddress(AVAIL_RING);
            q.used_ring = GuestAddress(USED_RING);
        }

        // Activate the device with a no-op DummyIrqChip-based interrupt transport.
        let dummy_irq: devices::legacy::IrqChip = DummyIrqChip::new().into();
        let interrupt_transport =
            InterruptTransport::new(dummy_irq, "blk-smoke".into()).unwrap();
        blk.activate(mem.clone(), interrupt_transport).unwrap();

        // ── 5. Run the EventManager to process activate_evt then the queue event ──
        // Write to the queue event to simulate a guest kick.
        blk.queue_events()[0].try_clone().unwrap().write(1).unwrap();

        let mut evmgr = polly::event_manager::EventManager::new().unwrap();
        let blk = Arc::new(Mutex::new(blk));
        evmgr.add_subscriber(blk.clone()).unwrap();
        // First run processes activate_evt and registers the queue event.
        // Second run processes the queue event and calls process_queue().
        let _ = evmgr.run_with_timeout(200);
        let _ = evmgr.run_with_timeout(200);

        // ── 6. Verify the read result ────────────────────────────────────────
        // GuestMemoryMmap::clone() is a shallow Arc clone — both `mem` here and
        // the copy stored inside the device share the same underlying pages.

        // Status byte should be 0 (VIRTIO_BLK_S_OK).
        let mut status = [0xffu8];
        mem.read_slice(&mut status, GuestAddress(STATUS_ADDR)).unwrap();
        assert_eq!(status[0], 0, "expected VIRTIO_BLK_S_OK (0) in status byte");

        // Data buffer should contain the magic bytes at offset 0.
        let mut data = [0u8; 8];
        mem.read_slice(&mut data, GuestAddress(DATA_ADDR)).unwrap();
        assert_eq!(&data, MAGIC, "sector-0 data mismatch");

        let _ = std::fs::remove_file(&path);
    }

    // ── virtio-net Windows backend smoke tests ────────────────────────────

    /// Verify that `NetWindows` exposes correct features, device type, and
    /// config space (MAC address + link-up status).
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_net_init_smoke() {
        use devices::virtio::{NetWindows, VirtioDevice};

        let mac: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let net = NetWindows::new("net-smoke-init", mac, None /* no TCP backend */)
            .expect("NetWindows::new failed");

        // Device type: TYPE_NET = 1
        assert_eq!(net.device_type(), 1, "expected TYPE_NET=1");

        // Features: VIRTIO_F_VERSION_1 (bit 32) and VIRTIO_NET_F_MAC (bit 5)
        let features = net.avail_features();
        assert_ne!(features & (1u64 << 32), 0, "VIRTIO_F_VERSION_1 not set");
        assert_ne!(features & (1u64 << 5), 0, "VIRTIO_NET_F_MAC not set");

        // Config space: MAC at offset 0, status=1 (link up) at offset 6.
        let mut cfg = [0u8; 10];
        net.read_config(0, &mut cfg);
        assert_eq!(&cfg[..6], &mac, "config MAC mismatch");
        let status = u16::from_le_bytes([cfg[6], cfg[7]]);
        assert_eq!(status, 1, "expected link status = 1 (up)");
    }

    /// Verify that `NetWindows` processes a TX queue entry end-to-end:
    /// a descriptor chain with a virtio-net header + Ethernet frame is
    /// consumed and the used ring index advances to 1.
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_net_tx_smoke() {
        use devices::legacy::DummyIrqChip;
        use devices::virtio::{InterruptTransport, NetWindows, VirtioDevice};
        use std::sync::{Arc, Mutex};
        use vm_memory::{GuestAddress, GuestMemoryMmap};

        // ── 1. Guest memory ───────────────────────────────────────────────
        const MEM_SIZE: usize = 4 << 20;
        let mem: GuestMemoryMmap =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();

        // ── 2. Queue layout (TX = queue 1) ────────────────────────────────
        // desc[0] → virtio-net header (10 bytes, read-only)
        // desc[1] → Ethernet frame    (64 bytes, read-only)
        const DESC_TABLE: u64 = 0x0000;
        const AVAIL_RING: u64 = 0x0100;
        const USED_RING: u64 = 0x0200;
        const HDR_ADDR: u64 = 0x1000;
        const ETH_ADDR: u64 = 0x1100;

        const VIRTQ_DESC_F_NEXT: u16 = 0x1;

        let write_desc = |idx: usize, addr: u64, len: u32, flags: u16, next: u16| {
            let base = GuestAddress(DESC_TABLE + idx as u64 * 16);
            mem.write_slice(&addr.to_le_bytes(), base).unwrap();
            mem.write_slice(&len.to_le_bytes(), GuestAddress(base.0 + 8)).unwrap();
            mem.write_slice(&flags.to_le_bytes(), GuestAddress(base.0 + 12)).unwrap();
            mem.write_slice(&next.to_le_bytes(), GuestAddress(base.0 + 14)).unwrap();
        };
        // desc[0]: virtio-net header (10 bytes, read-only, NEXT→1)
        write_desc(0, HDR_ADDR, 10, VIRTQ_DESC_F_NEXT, 1);
        // desc[1]: Ethernet frame (64 bytes, read-only, no NEXT)
        write_desc(1, ETH_ADDR, 64, 0, 0);

        // Write a recognisable Ethernet frame.
        let mut eth = [0u8; 64];
        eth[..6].copy_from_slice(&[0xFF; 6]); // dst MAC = broadcast
        eth[6..12].copy_from_slice(&[0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE]); // src
        eth[12..14].copy_from_slice(&[0x08, 0x00]); // EtherType = IPv4
        mem.write_slice(&eth, GuestAddress(ETH_ADDR)).unwrap();

        // Avail ring for TX queue: idx=1, ring[0]=0 (head descriptor index)
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING)).unwrap();     // flags
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2)).unwrap(); // idx
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4)).unwrap(); // ring[0]

        // Used ring: idx=0 initially (device increments it after processing).
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING)).unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2)).unwrap();

        // ── 3. Create and configure the device ───────────────────────────
        let mac: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let mut net = NetWindows::new("net-smoke-tx", mac, None).expect("NetWindows::new failed");

        // Wire TX queue (index 1) to our descriptor table.
        {
            let q = &mut net.queues_mut()[1]; // TX_INDEX = 1
            q.size = 4;
            q.ready = true;
            q.desc_table = GuestAddress(DESC_TABLE);
            q.avail_ring = GuestAddress(AVAIL_RING);
            q.used_ring = GuestAddress(USED_RING);
        }

        // ── 4. Activate and kick the TX queue ────────────────────────────
        let dummy_irq: devices::legacy::IrqChip = DummyIrqChip::new().into();
        let transport = InterruptTransport::new(dummy_irq, "net-smoke".into()).unwrap();
        net.activate(mem.clone(), transport).unwrap();

        // Signal the TX queue event (queue_events[1]).
        net.queue_events()[1].try_clone().unwrap().write(1).unwrap();

        let mut evmgr = polly::event_manager::EventManager::new().unwrap();
        let net = Arc::new(Mutex::new(net));
        evmgr.add_subscriber(net.clone()).unwrap();
        // Pass 1: activate_evt → register queue events.
        // Pass 2: TX queue event → process_tx_queue().
        let _ = evmgr.run_with_timeout(200);
        let _ = evmgr.run_with_timeout(200);

        // ── 5. Verify the used ring advanced ─────────────────────────────
        // used ring idx is at USED_RING + 2.
        let mut used_idx = [0u8; 2];
        mem.read_slice(&mut used_idx, GuestAddress(USED_RING + 2)).unwrap();
        assert_eq!(
            u16::from_le_bytes(used_idx),
            1,
            "expected used ring idx=1 after TX processing"
        );
    }

    /// Verify `Console::new()` returns a device with the correct type and features.
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_console_init_smoke() {
        use devices::virtio::{Console, VirtioDevice};

        let console = Console::new(vec![]).expect("Console::new failed");
        // TYPE_CONSOLE = 3
        assert_eq!(console.device_type(), 3, "expected TYPE_CONSOLE=3");
        let features = console.avail_features();
        // VIRTIO_F_VERSION_1 (bit 32)
        assert_ne!(features & (1u64 << 32), 0, "VIRTIO_F_VERSION_1 not set");
    }

    /// Verify that `Console` processes a TX queue entry end-to-end:
    /// a descriptor chain with payload is consumed and the used ring index
    /// advances to 1.
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_console_tx_smoke() {
        use devices::legacy::DummyIrqChip;
        use devices::virtio::{Console, InterruptTransport, PortDescription, VirtioDevice, port_io};
        use std::sync::{Arc, Mutex};
        use vm_memory::{GuestAddress, GuestMemoryMmap};

        // ── 1. Guest memory ───────────────────────────────────────────────
        const MEM_SIZE: usize = 4 << 20;
        let mem: GuestMemoryMmap =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();

        // ── 2. Queue layout (Console 1-port: 4 queues; TX = queue 3) ─────
        // desc[0] → payload (read-only, 16 bytes)
        const DESC_TABLE: u64 = 0x0000;
        const AVAIL_RING: u64 = 0x0100;
        const USED_RING: u64 = 0x0200;
        const PAYLOAD_ADDR: u64 = 0x1000;
        const QUEUE_IDX: usize = 3; // port 0 TX

        // desc[0]: addr=PAYLOAD_ADDR, len=16, flags=0 (no NEXT, no WRITE = read-only)
        // virtio descriptor: addr(u64) + len(u32) + flags(u16) + next(u16) = 16 bytes
        let mut desc_bytes = [0u8; 16];
        desc_bytes[0..8].copy_from_slice(&PAYLOAD_ADDR.to_le_bytes());
        desc_bytes[8..12].copy_from_slice(&16u32.to_le_bytes());
        desc_bytes[12..14].copy_from_slice(&0u16.to_le_bytes());
        desc_bytes[14..16].copy_from_slice(&0u16.to_le_bytes());
        mem.write_slice(&desc_bytes, GuestAddress(DESC_TABLE)).unwrap();

        // avail ring: flags(u16)=0, idx(u16)=1, ring[0]=0
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING)).unwrap();
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2)).unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4)).unwrap();

        // used ring: flags(u16)=0, idx(u16)=0
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING)).unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2)).unwrap();

        // payload data
        mem.write_slice(b"Hello, virtconsole!", GuestAddress(PAYLOAD_ADDR)).unwrap();

        // ── 3. Build Console with one output-only port ────────────────────
        let output = port_io::output_to_raw_fd_dup(1).expect("output_to_raw_fd_dup failed");
        let term = port_io::term_fixed_size(80, 24);
        let port = PortDescription::console(None, Some(output), term);

        let mut console = Console::new(vec![port]).expect("Console::new failed");

        // ── 4. Wire up queue QUEUE_IDX directly (same pattern as blk/net tests) ──
        {
            let q = &mut console.queues_mut()[QUEUE_IDX];
            q.size = 32;
            q.ready = true;
            q.desc_table = GuestAddress(DESC_TABLE);
            q.avail_ring = GuestAddress(AVAIL_RING);
            q.used_ring = GuestAddress(USED_RING);
        }

        // ── 5. Activate + run EventManager ───────────────────────────────
        let mut evmgr = polly::event_manager::EventManager::new().unwrap();
        let console_arc = Arc::new(Mutex::new(console));
        evmgr.add_subscriber(console_arc.clone()).unwrap();

        let dummy_irq: devices::legacy::IrqChip = DummyIrqChip::new().into();
        let interrupt = InterruptTransport::new(dummy_irq, "con-smoke".into()).unwrap();
        console_arc
            .lock()
            .unwrap()
            .activate(mem.clone(), interrupt)
            .unwrap();

        // First run: processes activate_evt → registers queue events
        let _ = evmgr.run_with_timeout(200);

        // Signal queue 3 so the TX path fires
        console_arc.lock().unwrap().queue_events()[QUEUE_IDX]
            .write(1)
            .unwrap();
        let _ = evmgr.run_with_timeout(200);

        // ── 6. Verify: used ring idx should have advanced to 1 ────────────
        let mut used_idx = [0u8; 2];
        mem.read_slice(&mut used_idx, GuestAddress(USED_RING + 2)).unwrap();
        assert_eq!(
            u16::from_le_bytes(used_idx),
            1,
            "expected used ring idx=1 after console TX processing"
        );
    }

    /// Verify that `WindowsStdinInput` correctly reads from its ring buffer
    /// and signals the EventFd.
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_stdin_reader_smoke() {
        use crate::windows::stdin_reader::WindowsStdinInput;
        use std::io::Read;

        let mut reader = WindowsStdinInput::new().expect("WindowsStdinInput::new failed");

        // Buffer is initially empty — read should return 0 without blocking.
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).expect("read failed");
        assert_eq!(n, 0, "expected 0 bytes from empty stdin buffer");

        // The EventFd fd must be a valid synthetic fd (positive value).
        use devices::legacy::ReadableFd;
        let fd = reader.as_raw_fd();
        assert!(fd > 0, "EventFd synthetic fd should be > 0");
    }

    /// Verify `Vsock::new()` creates a device with the correct type, features,
    /// and CID config space.  Also checks Named Pipe port mapping conversion.
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_vsock_init_smoke() {
        use devices::virtio::{TsiFlags, VirtioDevice, Vsock};
        use std::collections::HashMap;
        use std::path::PathBuf;

        const GUEST_CID: u64 = 3;

        // No port maps — simplest creation.
        let vsock = Vsock::new(GUEST_CID, None, None, TsiFlags::empty())
            .expect("Vsock::new failed");

        // TYPE_VSOCK = 19
        assert_eq!(vsock.device_type(), 19, "expected TYPE_VSOCK=19");

        // VIRTIO_F_VERSION_1 (bit 32) must be set.
        let features = vsock.avail_features();
        assert_ne!(features & (1u64 << 32), 0, "VIRTIO_F_VERSION_1 not set");

        // Config space at offset 0 encodes the guest CID as little-endian u64.
        let mut cfg = [0u8; 8];
        vsock.read_config(0, &mut cfg);
        let cid_from_config = u64::from_le_bytes(cfg);
        assert_eq!(cid_from_config, GUEST_CID, "CID mismatch in config space");

        // Verify Named Pipe name conversion: PathBuf("myservice") → pipe name "myservice".
        let mut port_map: HashMap<u32, (PathBuf, bool)> = HashMap::new();
        port_map.insert(1234, (PathBuf::from("myservice"), false));
        let vsock2 = Vsock::new(GUEST_CID, None, Some(port_map), TsiFlags::empty())
            .expect("Vsock::new with port_map failed");
        // The device should accept the port map without error; cid is still correct.
        let mut cfg2 = [0u8; 8];
        vsock2.read_config(0, &mut cfg2);
        assert_eq!(u64::from_le_bytes(cfg2), GUEST_CID, "CID mismatch in vsock2");
    }

    /// Verify that `Vsock` processes a TX queue entry (a CONNECT packet) end-to-end:
    /// the descriptor chain is consumed and the used ring index advances to 1,
    /// even when no Named Pipe server is available (connect fails gracefully).
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_vsock_tx_smoke() {
        use devices::legacy::DummyIrqChip;
        use devices::virtio::{InterruptTransport, TsiFlags, VirtioDevice, Vsock};
        use polly::event_manager::EventManager;
        use std::sync::{Arc, Mutex};
        use vm_memory::{GuestAddress, GuestMemoryMmap};

        // ── 1. Guest memory ───────────────────────────────────────────────
        const MEM_SIZE: usize = 4 << 20;
        let mem: GuestMemoryMmap =
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();

        // ── 2. Queue layout (TX = queue 1) ────────────────────────────────
        // One descriptor: a 44-byte virtio-vsock header (CONNECT op, no data).
        const DESC_TABLE: u64 = 0x0000;
        const AVAIL_RING: u64 = 0x0100;
        const USED_RING: u64 = 0x0200;
        const HDR_ADDR: u64 = 0x1000;

        // virtio-vsock header (44 bytes): src_cid=3, dst_cid=2, src_port=5000,
        // dst_port=9999, len=0, type=1 (STREAM), op=1 (CONNECT), flags=0,
        // buf_alloc=0, fwd_cnt=0.
        let mut hdr = [0u8; 44];
        hdr[0..8].copy_from_slice(&3u64.to_le_bytes());     // src_cid
        hdr[8..16].copy_from_slice(&2u64.to_le_bytes());    // dst_cid (host)
        hdr[16..20].copy_from_slice(&5000u32.to_le_bytes()); // src_port
        hdr[20..24].copy_from_slice(&9999u32.to_le_bytes()); // dst_port
        hdr[24..28].copy_from_slice(&0u32.to_le_bytes());   // len
        hdr[28..30].copy_from_slice(&1u16.to_le_bytes());   // type = STREAM
        hdr[30..32].copy_from_slice(&1u16.to_le_bytes());   // op = CONNECT
        mem.write_slice(&hdr, GuestAddress(HDR_ADDR)).unwrap();

        // desc[0]: addr=HDR_ADDR, len=44, flags=0 (read-only), next=0
        let mut desc_bytes = [0u8; 16];
        desc_bytes[0..8].copy_from_slice(&HDR_ADDR.to_le_bytes());
        desc_bytes[8..12].copy_from_slice(&44u32.to_le_bytes());
        mem.write_slice(&desc_bytes, GuestAddress(DESC_TABLE)).unwrap();

        // Avail ring for TX queue: flags=0, idx=1, ring[0]=0
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING)).unwrap();
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2)).unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4)).unwrap();

        // Used ring: idx=0 initially.
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING)).unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2)).unwrap();

        // ── 3. Create and configure the device ───────────────────────────
        let vsock = Vsock::new(3, None, None, TsiFlags::empty())
            .expect("Vsock::new failed");
        let vsock = Arc::new(Mutex::new(vsock));

        // ── 4. Wire up EventManager and activate ─────────────────────────
        let mut evmgr = EventManager::new().unwrap();
        evmgr.add_subscriber(vsock.clone()).unwrap();

        let dummy_irq: devices::legacy::IrqChip = DummyIrqChip::new().into();
        let interrupt =
            InterruptTransport::new(dummy_irq, "vsock-test".into()).unwrap();

        {
            let mut dev = vsock.lock().unwrap();
            // Configure queue 1 (TX) with our layout.
            dev.queues_mut()[1].size = 256;
            dev.queues_mut()[1].ready = true;
            dev.queues_mut()[1].desc_table = GuestAddress(DESC_TABLE);
            dev.queues_mut()[1].avail_ring = GuestAddress(AVAIL_RING);
            dev.queues_mut()[1].used_ring = GuestAddress(USED_RING);

            dev.activate(mem.clone(), interrupt).unwrap();
        }

        // Pass 1: processes activate_evt → registers queue event fds.
        let _ = evmgr.run_with_timeout(200);

        // Signal TX queue event (queue index 1).
        {
            let dev = vsock.lock().unwrap();
            dev.queue_events()[1].write(1).unwrap();
        }

        // Pass 2: processes TX queue event → consumes the CONNECT packet.
        let _ = evmgr.run_with_timeout(200);

        // Used ring idx should advance to 1 (packet consumed).
        let mut used_idx = [0u8; 2];
        mem.read_slice(&mut used_idx, GuestAddress(USED_RING + 2)).unwrap();
        assert_eq!(
            u16::from_le_bytes(used_idx),
            1,
            "expected used ring idx=1 after vsock TX processing"
        );
    }

    // ── Real Linux kernel end-to-end boot test ─────────────────────────────

    /// End-to-end real Linux kernel boot test.
    ///
    /// Boots an x86_64 ELF vmlinux via WHPX, captures COM1 serial output,
    /// and asserts the Linux version banner ("Linux version") appears within
    /// 60 seconds.
    ///
    /// Prerequisites:
    ///   - WHPX/Hyper-V enabled on the host
    ///   - `TEST_VMLINUX_PATH` env var pointing to an x86_64 ELF vmlinux
    ///     (a raw `vmlinux` ELF, NOT a compressed bzImage)
    ///
    /// To obtain a suitable kernel, run:
    ///   tests/windows/download_test_kernel.ps1
    #[test]
    #[ignore = "Requires WHPX and TEST_VMLINUX_PATH env var pointing to an x86_64 ELF vmlinux"]
    fn test_whpx_real_kernel_e2e() {
        use std::sync::{Arc, Mutex};

        use devices::{Bus, BusDevice};
        use linux_loader::loader::{Elf, KernelLoader};

        // ── 1. Kernel path from env var — skip gracefully if not set ───────
        let vmlinux_path = match std::env::var("TEST_VMLINUX_PATH") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                eprintln!(
                    "[SKIP] TEST_VMLINUX_PATH not set; \
                     point it to an x86_64 ELF vmlinux to run this test.\n\
                     Run tests/windows/download_test_kernel.ps1 to fetch one."
                );
                return;
            }
        };

        // ── 2. Shared COM1 capture buffer ──────────────────────────────────
        // The vCPU thread writes captured bytes via the Bus; the main thread
        // polls the buffer for the Linux version banner.
        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct Com1Capture {
            buf: Arc<Mutex<Vec<u8>>>,
        }
        impl BusDevice for Com1Capture {
            // Capture characters written to the UART transmit register (offset 0).
            fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
                if offset == 0 {
                    self.buf.lock().unwrap().extend_from_slice(data);
                }
            }
            // Emulate UART LSR (offset 5): always report TX ready (THRE | TEMT).
            // Without this the kernel's earlycon busy-waits on bit 5 and stalls.
            fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
                if offset == 5 && !data.is_empty() {
                    data[0] = 0x60; // UART_LSR_THRE | UART_LSR_TEMT
                }
            }
        }

        // ── 3. Create 256 MB guest memory ─────────────────────────────────
        const MEM_SIZE: usize = 256 << 20;
        let (arch_mem_info, arch_mem_regions) =
            arch::arch_memory_regions(MEM_SIZE, None, 0, 0, None);
        let guest_mem = GuestMemoryMmap::from_ranges(&arch_mem_regions).unwrap();

        // ── 4. Load the kernel ELF ────────────────────────────────────────
        // linux_loader resolves the virtual→physical mapping and returns the
        // physical GPA entry point via kernel_load.
        let mut kernel_file = std::fs::File::open(&vmlinux_path)
            .unwrap_or_else(|e| panic!("Cannot open {:?}: {}", vmlinux_path, e));
        let load_result = Elf::load(&guest_mem, None, &mut kernel_file, None).expect(
            "ELF load failed — ensure TEST_VMLINUX_PATH is a raw ELF vmlinux, not a bzImage",
        );
        let kernel_entry = load_result.kernel_load;
        eprintln!("[e2e] Kernel entry GPA: 0x{:x}", kernel_entry.0);

        // ── 5. Write kernel command line ──────────────────────────────────
        // earlycon=uart8250,io,0x3f8 wires the very first printk — including
        // the "Linux version" banner — directly to the UART before the full
        // 8250 driver initialises, giving us immediate COM1 output.
        let cmdline =
            b"console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8 reboot=t panic=1 nokaslr\0";
        guest_mem
            .write_slice(cmdline, GuestAddress(arch::x86_64::layout::CMDLINE_START))
            .unwrap();

        // ── 6. Populate the Linux x86_64 zero page (boot_params @ 0x7000) ─
        arch::configure_system(
            &guest_mem,
            &arch_mem_info,
            GuestAddress(arch::x86_64::layout::CMDLINE_START),
            cmdline.len(),
            &None, // no initrd
            1,     // single vCPU
        )
        .unwrap();

        // ── 7. Create WHPX partition and map guest memory ─────────────────
        let mut vm = Vm::new(false, 1).unwrap();
        vm.memory_init(&guest_mem).unwrap();

        // ── 8. IO bus: COM1 capture device at ports 0x3F8–0x3FF ──────────
        let mut io_bus = Bus::new();
        io_bus
            .insert(
                Arc::new(Mutex::new(Com1Capture {
                    buf: captured.clone(),
                })),
                0x3F8,
                0x8,
            )
            .unwrap();

        // ── 9. Create vCPU ────────────────────────────────────────────────
        let exit_evt =
            utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            kernel_entry,
            io_bus,
            exit_evt,
        )
        .unwrap();

        // ── 10. Launch vCPU thread ────────────────────────────────────────
        // start_threaded() calls configure_x86_64() (RIP=kernel_entry,
        // RSI=0x7000 zero page) then drives the WHPX run loop.
        let handle = vcpu.start_threaded().unwrap();

        // ── 11. Poll until vCPU exits or 90 s deadline ───────────────────
        // Do NOT early-return on banner discovery: Vm must outlive the vCPU
        // thread to avoid WHvDeletePartition racing with WHvRunVirtualProcessor.
        // Instead, track the banner flag and keep looping until the thread
        // exits naturally (kernel panic) or we cancel it below.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut found_banner = false;
        let mut last_len = 0usize;
        let mut vcpu_exited = false;
        loop {
            // Non-blocking check for vCPU thread exit.
            if let Ok(resp) = handle.response_receiver().try_recv() {
                eprintln!("[e2e] vCPU thread exited: {:?}", resp);
                vcpu_exited = true;
                break;
            }

            // Stream newly captured bytes to stderr for live progress.
            let snapshot = captured.lock().unwrap().clone();
            if snapshot.len() > last_len {
                eprint!("{}", String::from_utf8_lossy(&snapshot[last_len..]));
                last_len = snapshot.len();
            }

            if !found_banner
                && String::from_utf8_lossy(&snapshot).contains("Linux version")
            {
                found_banner = true;
                eprintln!(
                    "\n[e2e] 'Linux version' found — waiting for vCPU to exit..."
                );
            }

            if std::time::Instant::now() >= deadline {
                eprintln!("[e2e] deadline reached");
                break;
            }

            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // ── 12. Cancel vCPU if it has not yet exited ──────────────────────
        // WHvCancelRunVirtualProcessor interrupts any in-flight
        // WHvRunVirtualProcessor, causing it to return WHvRunVpExitReasonCanceled
        // → VcpuExit::Shutdown → VcpuEmulation::Stopped → thread exits.
        // This ensures Vm::drop (WHvDeletePartition) does not race the thread.
        if !vcpu_exited {
            unsafe {
                let _ = windows::Win32::System::Hypervisor::WHvCancelRunVirtualProcessor(
                    vm.partition(),
                    0, // vCPU index
                    0, // flags (reserved, must be 0)
                );
            }
            let _ = handle
                .response_receiver()
                .recv_timeout(std::time::Duration::from_secs(5));
        }

        // ── 13. Assert the Linux version banner appeared ───────────────────
        let snapshot = captured.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&snapshot);
        eprintln!(
            "[e2e] Serial output ({} bytes total):\n{}",
            snapshot.len(),
            &text[..text.len().min(5000)]
        );
        assert!(
            found_banner,
            "[e2e] FAIL: 'Linux version' not found in serial output.\nGot:\n{}",
            &text[..text.len().min(2000)]
        );
        eprintln!("[e2e] PASS");
    }
}
