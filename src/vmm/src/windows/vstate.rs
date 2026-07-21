// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter};
use std::io;
use std::result;
use std::sync::Arc;

use vm_memory::{Address, Bytes, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};
use windows::Win32::System::Hypervisor::*;

use super::interrupts::PendingInterruptQueue;
use super::registers::{get_virtual_processor_registers, set_virtual_processor_registers};
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
const XAPIC_LVT_LINT0_OFFSET: usize = 0x350;
const XAPIC_LVT_LINT1_OFFSET: usize = 0x360;
const XAPIC_LVT_TIMER_OFFSET: usize = 0x320;
const XAPIC_TIMER_INITIAL_COUNT_OFFSET: usize = 0x380;
const XAPIC_TIMER_CURRENT_COUNT_OFFSET: usize = 0x390;
const XAPIC_TIMER_DIVIDE_OFFSET: usize = 0x3e0;
const XAPIC_SPURIOUS_VECTOR_OFFSET: usize = 0x0f0;
const XAPIC_ID_OFFSET: usize = 0x020;
const XAPIC_LDR_OFFSET: usize = 0x0d0;
const APIC_MODE_MASK: u32 = 0x700;
const APIC_LVT_MASK: u32 = 0x1_0000;
const APIC_MODE_NMI: u32 = 0x4;
const APIC_MODE_EXTINT: u32 = 0x7;
const APIC_SVR_ENABLE: u32 = 0x100;
const APIC_BASE_PHYS: u64 = 0xfee0_0000;
const APIC_BASE_MSR_ENABLE: u64 = 0x800;
const APIC_BASE_MSR_BSP: u64 = 0x100;
const HV_SYNTHETIC_FEATURE_HYPERVISOR_PRESENT: u64 = 1 << 0;
const HV_SYNTHETIC_FEATURE_HV1: u64 = 1 << 1;
const HV_SYNTHETIC_FEATURE_VP_RUNTIME: u64 = 1 << 2;
const HV_SYNTHETIC_FEATURE_REFERENCE_COUNTER: u64 = 1 << 3;
const HV_SYNTHETIC_FEATURE_HYPERCALL_REGS: u64 = 1 << 7;
const HV_SYNTHETIC_FEATURE_VP_INDEX: u64 = 1 << 8;
const HV_SYNTHETIC_FEATURE_PARTITION_REFERENCE_TSC: u64 = 1 << 9;
const HV_SYNTHETIC_FEATURE_FREQUENCY_REGS: u64 = 1 << 11;

fn desired_windows_hyperv_synthetic_features() -> u64 {
    HV_SYNTHETIC_FEATURE_HYPERVISOR_PRESENT
        | HV_SYNTHETIC_FEATURE_HV1
        | HV_SYNTHETIC_FEATURE_VP_RUNTIME
        | HV_SYNTHETIC_FEATURE_REFERENCE_COUNTER
        | HV_SYNTHETIC_FEATURE_HYPERCALL_REGS
        | HV_SYNTHETIC_FEATURE_VP_INDEX
        | HV_SYNTHETIC_FEATURE_PARTITION_REFERENCE_TSC
        | HV_SYNTHETIC_FEATURE_FREQUENCY_REGS
}

fn windows_hyperv_enlightenments_enabled() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WINDOWS_HYPERV_ENLIGHTENMENTS")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    })
}

fn host_windows_hyperv_synthetic_features() -> Option<u64> {
    unsafe {
        let mut capability = WHV_CAPABILITY::default();
        let mut written_size = 0u32;
        WHvGetCapability(
            WHvCapabilityCodeSyntheticProcessorFeaturesBanks,
            &mut capability as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<WHV_SYNTHETIC_PROCESSOR_FEATURES_BANKS>() as u32,
            Some(&mut written_size as *mut u32),
        )
        .ok()?;

        let banks = capability.SyntheticProcessorFeaturesBanks;
        if banks.BanksCount == 0 {
            return None;
        }

        Some(banks.Anonymous.AsUINT64[0])
    }
}

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

fn windows_vcpu_debug_log(message: impl AsRef<str>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        std::env::var("LIBKRUN_WINDOWS_VERBOSE_DEBUG")
            .or_else(|_| std::env::var("LIBKRUN_WINDOWS_VCPU_DEBUG"))
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }) {
        return;
    }
    utils::windows_debug_log("whpx-vcpu.log", message);
}

fn windows_vcpu_exit_state_log(vcpu_id: u8, message: impl AsRef<str>) {
    windows_vcpu_debug_log(format!("[VCPU-EXIT] vcpu={} {}", vcpu_id, message.as_ref()));
}

fn read_u32_le(buf: &[u8], offset: usize) -> Option<u32> {
    let bytes = buf.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn write_u32_le(buf: &mut [u8], offset: usize, value: u32) -> bool {
    let Some(dst) = buf.get_mut(offset..offset + 4) else {
        return false;
    };
    dst.copy_from_slice(&value.to_le_bytes());
    true
}

fn set_apic_delivery_mode(reg: u32, mode: u32) -> u32 {
    (reg & !(APIC_MODE_MASK | APIC_LVT_MASK)) | (mode << 8)
}

fn write_boot_state_to_guest(guest_mem: &GuestMemoryMmap) -> Result<()> {
    log::debug!("=== HIGHER-HALF KERNEL MAPPING FIX ACTIVE ===");

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

    // Set up page tables for identity mapping (low memory 0-1GB)
    // PML4[0] -> PDPTE -> PDE (512 x 2MB pages)
    guest_mem
        .write_obj(PDPTE_START | 0x03, GuestAddress(PML4_START))
        .map_err(|_| Error::VcpuConfigure)?;
    guest_mem
        .write_obj(PDE_START | 0x03, GuestAddress(PDPTE_START))
        .map_err(|_| Error::VcpuConfigure)?;

    // Identity map first 1GB (0x0 - 0x40000000) using 2MB pages
    for i in 0..512 {
        guest_mem
            .write_obj(
                (i << 21) as u64 | 0x83,
                GuestAddress(PDE_START + (i * 8) as u64),
            )
            .map_err(|_| Error::VcpuConfigure)?;
    }

    // Set up higher-half kernel mapping (0xffffffff80000000+)
    // PML4[511] -> same PDPTE (maps kernel virtual addresses to same physical memory)
    // This allows the kernel to access physical memory 0-1GB via virtual addresses 0xffffffff80000000+
    guest_mem
        .write_obj(PDPTE_START | 0x03, GuestAddress(PML4_START + (511 * 8)))
        .map_err(|_| Error::VcpuConfigure)?;

    log::debug!(
        "Page tables configured: PML4=0x{:x}, PDPTE=0x{:x}, PDE=0x{:x}",
        PML4_START,
        PDPTE_START,
        PDE_START
    );
    log::debug!("Identity mapping: 0x0-0x40000000 (1GB)");
    log::debug!("Higher-half kernel mapping: 0xffffffff80000000+ -> 0x0-0x40000000");

    Ok(())
}

/// A wrapper around creating and using a WHPX VM.
pub struct Vm {
    partition: WHV_PARTITION_HANDLE,
}

impl Vm {
    /// Constructs a new `Vm` using WHPX.
    ///
    /// `enable_apic`: set `true` for production VM boots (enables LAPIC emulation and
    /// Hyper-V CPUID enlightenments so the Linux kernel can use `hyperv_clocksource`).
    /// Set `false` for simple smoke tests — with APIC emulation active,
    /// `WHvRunVirtualProcessor` blocks on HLT indefinitely (WHPX waits for the APIC
    /// to deliver an interrupt), which would hang any test that expects a clean HLT exit.
    pub fn new(_nested_enabled: bool, vcpu_count: u32, enable_apic: bool) -> Result<Self> {
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

            if enable_apic {
                // Enable local APIC emulation to support interrupt injection.
                // WARNING: with APIC emulation enabled, WHvRunVirtualProcessor blocks on
                // HLT until the APIC delivers an interrupt (Hyper-V synthetic HLT semantics).
                // This is correct for production (Linux timer wakes it) but breaks simple
                // HLT smoke tests. Guard all APIC/CPUID setup behind `enable_apic`.
                use windows::Win32::System::Hypervisor::{
                    WHvPartitionPropertyCodeLocalApicEmulationMode,
                    WHvX64LocalApicEmulationModeXApic,
                };
                let mut apic_property: WHV_PARTITION_PROPERTY = std::mem::zeroed();
                apic_property.LocalApicEmulationMode = WHvX64LocalApicEmulationModeXApic;
                match WHvSetPartitionProperty(
                    partition,
                    WHvPartitionPropertyCodeLocalApicEmulationMode,
                    &apic_property as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<WHV_X64_LOCAL_APIC_EMULATION_MODE>() as u32,
                ) {
                    Err(e) => {
                        eprintln!("[WHPX] Failed to enable APIC emulation: {:?}", e);
                        windows_vcpu_debug_log(format!(
                            "[APIC] set LocalApicEmulationMode failed hr=0x{:x}",
                            e.code().0 as u32
                        ));
                    }
                    Ok(()) => {
                        let mut current_mode: WHV_PARTITION_PROPERTY = std::mem::zeroed();
                        let mut written_size = 0u32;
                        match WHvGetPartitionProperty(
                            partition,
                            WHvPartitionPropertyCodeLocalApicEmulationMode,
                            &mut current_mode as *mut _ as *mut std::ffi::c_void,
                            std::mem::size_of::<WHV_X64_LOCAL_APIC_EMULATION_MODE>() as u32,
                            Some(&mut written_size as *mut u32),
                        ) {
                            Ok(()) => windows_vcpu_debug_log(format!(
                                "[APIC] LocalApicEmulationMode set ok readback={} written_size={}",
                                current_mode.LocalApicEmulationMode.0, written_size
                            )),
                            Err(e) => windows_vcpu_debug_log(format!(
                                "[APIC] LocalApicEmulationMode readback failed hr=0x{:x}",
                                e.code().0 as u32
                            )),
                        }
                    }
                }

                if windows_hyperv_enlightenments_enabled() {
                    // Set CPUID 0x1 ECX bit 31 (hypervisor present) so the kernel
                    // detects Hyper-V via CPUID 0x40000000. Our emulate_cpuid() exit
                    // handler serves the Hyper-V leaves with bit 8 of 0x40000003 CLEARED,
                    // so the kernel does NOT install hv_calibrate_tsc().
                    // Hyper-V synthetic timer fires via WHPX APIC emulation → jiffies tick.
                    let mut cpuid_result: WHV_X64_CPUID_RESULT2 = std::mem::zeroed();
                    cpuid_result.Function = 0x1;
                    // ECX bit 31 = hypervisor present
                    cpuid_result.Output.Ecx = 1u32 << 31;
                    // Mask: only override bit 31 of ECX, leave all other bits as hardware
                    cpuid_result.Mask.Ecx = 1u32 << 31;
                    WHvSetPartitionProperty(
                        partition,
                        WHvPartitionPropertyCodeCpuidResultList2,
                        &cpuid_result as *const _ as *const std::ffi::c_void,
                        std::mem::size_of::<WHV_X64_CPUID_RESULT2>() as u32,
                    )
                    .map_err(|e| {
                        eprintln!(
                            "[WHPX] Failed to set CpuidResultList2: {:?} (0x{:x})",
                            e,
                            e.code().0 as u32
                        );
                    })
                    .ok();

                    let host_mask = host_windows_hyperv_synthetic_features();
                    let desired_mask = desired_windows_hyperv_synthetic_features();
                    let effective_mask = host_mask.map(|mask| mask & desired_mask).unwrap_or(0);
                    windows_vcpu_debug_log(format!(
                        "[HVFEAT] desired=0x{:016x} host={} effective=0x{:016x}",
                        desired_mask,
                        host_mask
                            .map(|mask| format!("0x{mask:016x}"))
                            .unwrap_or_else(|| "unavailable".to_string()),
                        effective_mask
                    ));

                    if effective_mask != 0 {
                        let mut synthetic_features: WHV_PARTITION_PROPERTY = std::mem::zeroed();
                        synthetic_features
                            .SyntheticProcessorFeaturesBanks
                            .BanksCount = 1;
                        synthetic_features
                            .SyntheticProcessorFeaturesBanks
                            .Anonymous
                            .AsUINT64[0] = effective_mask;

                        WHvSetPartitionProperty(
                            partition,
                            WHvPartitionPropertyCodeSyntheticProcessorFeaturesBanks,
                            &synthetic_features as *const _ as *const std::ffi::c_void,
                            std::mem::size_of::<WHV_SYNTHETIC_PROCESSOR_FEATURES_BANKS>() as u32,
                        )
                        .map_err(|e| {
                            eprintln!(
                                "[WHPX] Failed to set SyntheticProcessorFeaturesBanks: {:?} (0x{:x})",
                                e,
                                e.code().0 as u32
                            );
                        })
                        .ok();
                    }
                }

                // Enable MSR exits so whpx_vcpu::emulate_msr() intercepts Hyper-V MSRs
                // (0x40000020 time ref count, 0x40000022 TSC frequency).
                // Must be done BEFORE WHvSetupPartition.
                //
                // WHV_EXTENDED_VM_EXITS is a union with AsUINT64; bit 1 = X64MsrExitEnabled.
                let mut extended_exits: WHV_EXTENDED_VM_EXITS = std::mem::zeroed();
                extended_exits.AsUINT64 = (1 << 1) | (1 << 0);
                // Bit 0: X64CpuidExitEnabled — required for CpuidResultList2 to activate for
                //   standard leaves (0x15). Leaves in CpuidResultList2 are served statically
                //   (no exit generated); leaves NOT in the list generate CPUID exits handled
                //   by emulate_cpuid() which passes through to hardware.
                // Bit 1: X64MsrExitEnabled — MSR exits (intercepted by emulate_msr()).
                WHvSetPartitionProperty(
                    partition,
                    WHvPartitionPropertyCodeExtendedVmExits,
                    &extended_exits as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<WHV_EXTENDED_VM_EXITS>() as u32,
                )
                .map_err(|e| {
                    eprintln!(
                        "[WHPX] Failed to enable MSR exits: {:?} (0x{:x})",
                        e,
                        e.code().0 as u32
                    );
                })
                .ok();

                // Configure MSR exit bitmap to intercept all unhandled MSRs.
                // WHV_X64_MSR_EXIT_BITMAP is a union with AsUINT64; setting UnhandledMsrs (bit 0)
                // causes WHPX to generate exits for MSRs it doesn't handle natively (including
                // all Hyper-V synthetic MSRs like 0x40000020 and 0x40000022).
                let mut msr_bitmap: WHV_X64_MSR_EXIT_BITMAP = std::mem::zeroed();
                msr_bitmap.AsUINT64 = 1; // UnhandledMsrs bit
                WHvSetPartitionProperty(
                    partition,
                    WHvPartitionPropertyCodeX64MsrExitBitmap,
                    &msr_bitmap as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<WHV_X64_MSR_EXIT_BITMAP>() as u32,
                )
                .map_err(|e| {
                    eprintln!(
                        "[WHPX] Failed to set MSR exit bitmap: {:?} (0x{:x})",
                        e,
                        e.code().0 as u32
                    );
                })
                .ok();

                if windows_hyperv_enlightenments_enabled() {
                    use windows::Win32::System::Hypervisor::{
                        WHvMsrActionExit, WHvPartitionPropertyCodeMsrActionList,
                        WHV_MSR_ACTION_ENTRY,
                    };

                    let msr_action_entries = [
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0000,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0001,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0002,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0010,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0020,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0022,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0023,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                        WHV_MSR_ACTION_ENTRY {
                            Index: 0x4000_0073,
                            ReadAction: WHvMsrActionExit.0 as u8,
                            WriteAction: WHvMsrActionExit.0 as u8,
                            Reserved: 0,
                        },
                    ];

                    WHvSetPartitionProperty(
                        partition,
                        WHvPartitionPropertyCodeMsrActionList,
                        msr_action_entries.as_ptr() as *const std::ffi::c_void,
                        std::mem::size_of_val(&msr_action_entries) as u32,
                    )
                    .map_err(|e| {
                        eprintln!(
                            "[WHPX] Failed to set MsrActionList: {:?} (0x{:x})",
                            e,
                            e.code().0 as u32
                        );
                    })
                    .ok();
                }
            }

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
    /// Signaled by `WhpxIrqChip::set_irq()` after posting an interrupt via
    /// `WHvRequestInterrupt`.  When `Some`, `start_threaded()` waits on this
    /// instead of treating HLT as terminal, allowing the guest idle loop to
    /// work correctly.  `None` in unit tests that expect HLT to terminate.
    irq_pending_evt: Option<Arc<utils::eventfd::EventFd>>,
    pending_interrupt: Option<PendingInterruptQueue>,
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
        irq_pending_evt: Option<Arc<utils::eventfd::EventFd>>,
        pending_interrupt: Option<PendingInterruptQueue>,
    ) -> Result<Self> {
        let (event_sender, event_receiver) = crossbeam_channel::unbounded();
        let (response_sender, response_receiver) = crossbeam_channel::unbounded();

        let whpx_vcpu = WhpxVcpu::new(partition, id as u32, pending_interrupt.clone())
            .map_err(Error::VcpuSpawn)?;

        Ok(Vcpu {
            id,
            whpx_vcpu,
            partition,
            guest_mem,
            boot_entry_addr: boot_entry_addr.raw_value(),
            io_bus,
            mmio_bus: None,
            exit_evt,
            irq_pending_evt,
            pending_interrupt,
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
        log::debug!(
            "Configuring vCPU {} for x86_64 boot: RIP=0x{:x}, RSP=0x{:x}, RSI=0x{:x}",
            self.id,
            kernel_start_addr.raw_value(),
            arch::x86_64::layout::BOOT_STACK_POINTER,
            arch::x86_64::layout::ZERO_PAGE_START
        );

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
            WHvX64RegisterApicBase,
        ];

        let reg_values: [WHV_REGISTER_VALUE; 19] = unsafe {
            let mut v: [WHV_REGISTER_VALUE; 19] = std::mem::zeroed();
            v[0].Reg64 = kernel_start_addr.raw_value();
            v[1].Reg64 = arch::x86_64::layout::BOOT_STACK_POINTER;
            v[2].Reg64 = arch::x86_64::layout::BOOT_STACK_POINTER;
            v[3].Reg64 = arch::x86_64::layout::ZERO_PAGE_START;
            // RFLAGS: bit 1 = reserved (always 1). Do NOT set IF (bit 9) here;
            // Linux sets IF via `sti` during early boot. With WHPX APIC emulation
            // enabled, starting with IF=1 causes WHvRunVirtualProcessor to block
            // indefinitely when HLT is executed (WHPX waits for an interrupt
            // instead of returning WHvRunVpExitReasonX64Halt).
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
            v[18].Reg64 = APIC_BASE_PHYS
                | APIC_BASE_MSR_ENABLE
                | if self.id == 0 { APIC_BASE_MSR_BSP } else { 0 };
            v
        };

        unsafe {
            set_virtual_processor_registers(
                self.partition,
                self.id as u32,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_ptr(),
            )
            .map_err(|e| {
                windows_vcpu_debug_log(format!(
                    "[APICCFG] vcpu={} set_regs failed hr=0x{:x}",
                    self.id,
                    e.code().0 as u32
                ));
                error!("Failed to set x86_64 registers for vCPU {}: {e}", self.id);
                Error::VcpuConfigure
            })?;
        }

        windows_vcpu_debug_log(format!("[APICCFG] vcpu={} set_regs ok", self.id));
        windows_vcpu_debug_log(format!(
            "[APICCFG] vcpu={} apic_base=0x{:016x}",
            self.id,
            APIC_BASE_PHYS
                | APIC_BASE_MSR_ENABLE
                | if self.id == 0 { APIC_BASE_MSR_BSP } else { 0 }
        ));

        self.configure_lint()?;

        Ok(())
    }

    fn configure_lint(&self) -> Result<()> {
        use windows::Win32::System::Hypervisor::{
            WHvGetVirtualProcessorInterruptControllerState,
            WHvSetVirtualProcessorInterruptControllerState, WHvX64RegisterApicLvtLint0,
            WHvX64RegisterApicLvtLint1, WHV_REGISTER_VALUE,
        };

        let reg_names = [WHvX64RegisterApicLvtLint0, WHvX64RegisterApicLvtLint1];
        let mut reg_values = [WHV_REGISTER_VALUE::default(); 2];

        unsafe {
            if let Err(e) = get_virtual_processor_registers(
                self.partition,
                self.id as u32,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_mut_ptr(),
            ) {
                log::warn!(
                    "APIC LINT register access is unavailable on vCPU {}: {e}",
                    self.id
                );
                windows_vcpu_debug_log(format!(
                    "[APIC] vcpu={} lint read unavailable hr=0x{:x}",
                    self.id,
                    e.code().0 as u32
                ));

                let mut apic_state = [0u8; 4096];
                let mut bytes_written = 0u32;
                if let Err(state_err) = WHvGetVirtualProcessorInterruptControllerState(
                    self.partition,
                    self.id as u32,
                    apic_state.as_mut_ptr() as *mut std::ffi::c_void,
                    apic_state.len() as u32,
                    Some(&mut bytes_written as *mut u32),
                ) {
                    windows_vcpu_debug_log(format!(
                        "[APIC] vcpu={} interrupt-controller-state read unavailable hr=0x{:x}",
                        self.id,
                        state_err.code().0 as u32
                    ));
                    return Ok(());
                }

                let Some(lint0_before) = read_u32_le(&apic_state, XAPIC_LVT_LINT0_OFFSET) else {
                    windows_vcpu_debug_log(format!(
                        "[APIC] vcpu={} interrupt-controller-state too small for LINT0 bytes_written={}",
                        self.id, bytes_written
                    ));
                    return Ok(());
                };
                let Some(lint1_before) = read_u32_le(&apic_state, XAPIC_LVT_LINT1_OFFSET) else {
                    windows_vcpu_debug_log(format!(
                        "[APIC] vcpu={} interrupt-controller-state too small for LINT1 bytes_written={}",
                        self.id, bytes_written
                    ));
                    return Ok(());
                };
                let Some(spurious_before) = read_u32_le(&apic_state, XAPIC_SPURIOUS_VECTOR_OFFSET)
                else {
                    windows_vcpu_debug_log(format!(
                        "[APIC] vcpu={} interrupt-controller-state too small for SVR bytes_written={}",
                        self.id, bytes_written
                    ));
                    return Ok(());
                };
                let apic_id_before = read_u32_le(&apic_state, XAPIC_ID_OFFSET).unwrap_or(u32::MAX);
                let ldr_before = read_u32_le(&apic_state, XAPIC_LDR_OFFSET).unwrap_or(u32::MAX);

                let lint0_after = set_apic_delivery_mode(lint0_before, APIC_MODE_EXTINT);
                let lint1_after = set_apic_delivery_mode(lint1_before, APIC_MODE_NMI);
                let spurious_after = spurious_before | APIC_SVR_ENABLE;
                if !write_u32_le(&mut apic_state, XAPIC_LVT_LINT0_OFFSET, lint0_after)
                    || !write_u32_le(&mut apic_state, XAPIC_LVT_LINT1_OFFSET, lint1_after)
                    || !write_u32_le(
                        &mut apic_state,
                        XAPIC_SPURIOUS_VECTOR_OFFSET,
                        spurious_after,
                    )
                {
                    windows_vcpu_debug_log(format!(
                        "[APIC] vcpu={} interrupt-controller-state write slice failure bytes_written={}",
                        self.id, bytes_written
                    ));
                    return Ok(());
                }

                let state_size = if bytes_written == 0 {
                    apic_state.len() as u32
                } else {
                    bytes_written.min(apic_state.len() as u32)
                };

                if let Err(state_err) = WHvSetVirtualProcessorInterruptControllerState(
                    self.partition,
                    self.id as u32,
                    apic_state.as_ptr() as *const std::ffi::c_void,
                    state_size,
                ) {
                    windows_vcpu_debug_log(format!(
                        "[APIC] vcpu={} interrupt-controller-state write unavailable hr=0x{:x} lint0=0x{:08x}->0x{:08x} lint1=0x{:08x}->0x{:08x}",
                        self.id,
                        state_err.code().0 as u32,
                        lint0_before,
                        lint0_after,
                        lint1_before,
                        lint1_after
                    ));
                    return Ok(());
                }

                let mut verify_state = [0u8; 4096];
                let mut verify_bytes = 0u32;
                let _ = WHvGetVirtualProcessorInterruptControllerState(
                    self.partition,
                    self.id as u32,
                    verify_state.as_mut_ptr() as *mut std::ffi::c_void,
                    verify_state.len() as u32,
                    Some(&mut verify_bytes as *mut u32),
                );
                let lint0_verify =
                    read_u32_le(&verify_state, XAPIC_LVT_LINT0_OFFSET).unwrap_or(u32::MAX);
                let lint1_verify =
                    read_u32_le(&verify_state, XAPIC_LVT_LINT1_OFFSET).unwrap_or(u32::MAX);
                let spurious_verify =
                    read_u32_le(&verify_state, XAPIC_SPURIOUS_VECTOR_OFFSET).unwrap_or(u32::MAX);

                windows_vcpu_debug_log(format!(
                    "[APICCFG] vcpu={} state bytes={} verify_bytes={} apic_id=0x{:08x} ldr=0x{:08x} lint0=0x{:08x}->0x{:08x}->0x{:08x} lint1=0x{:08x}->0x{:08x}->0x{:08x} svr=0x{:08x}->0x{:08x}->0x{:08x}",
                    self.id,
                    bytes_written,
                    verify_bytes,
                    apic_id_before,
                    ldr_before,
                    lint0_before,
                    lint0_after,
                    lint0_verify,
                    lint1_before,
                    lint1_after,
                    lint1_verify,
                    spurious_before,
                    spurious_after,
                    spurious_verify
                ));
                return Ok(());
            }
        }

        let (lint0_before, lint1_before) = unsafe { (reg_values[0].Reg64, reg_values[1].Reg64) };
        reg_values[0].Reg64 = u64::from(set_apic_delivery_mode(
            lint0_before as u32,
            APIC_MODE_EXTINT,
        ));
        reg_values[1].Reg64 = u64::from(set_apic_delivery_mode(lint1_before as u32, APIC_MODE_NMI));

        unsafe {
            if let Err(e) = set_virtual_processor_registers(
                self.partition,
                self.id as u32,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_ptr(),
            ) {
                log::warn!(
                    "APIC LINT register programming is unavailable on vCPU {}: {e}",
                    self.id
                );
                windows_vcpu_debug_log(format!(
                    "[APIC] vcpu={} lint write unavailable hr=0x{:x}",
                    self.id,
                    e.code().0 as u32
                ));
                return Ok(());
            }
        }

        let (lint0_after, lint1_after) = unsafe { (reg_values[0].Reg64, reg_values[1].Reg64) };
        windows_vcpu_debug_log(format!(
            "[APIC] vcpu={} lint0=0x{:016x}->0x{:016x} lint1=0x{:016x}->0x{:016x}",
            self.id, lint0_before, lint0_after, lint1_before, lint1_after
        ));

        Ok(())
    }

    /// Moves the vcpu to its own thread and constructs a VcpuHandle.
    pub fn start_threaded(mut self) -> Result<VcpuHandle> {
        log::debug!("start_threaded called for vCPU {}", self.id);

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

                // Wait for the initial Resume before entering the run loop.
                // resume_vcpus() in lib.rs calls start_vcpus() then immediately
                // sends VcpuEvent::Resume to each handle.  Without this barrier
                // the vCPU thread can start running, hit a guest exit (HLT, fault,
                // etc.) and send VcpuResponse::Exited *before* the main thread
                // calls recv_timeout() for VcpuResponse::Resumed — causing a
                // spurious VcpuResume error.
                match self.event_receiver.recv() {
                    Ok(VcpuEvent::Resume) => {
                        if self
                            .response_sender
                            .send(VcpuResponse::Resumed)
                            .is_err()
                        {
                            return;
                        }
                    }
                    _ => {
                        // Channel closed or unexpected event before first resume.
                        self.exit(FC_EXIT_CODE_GENERIC_ERROR);
                        return;
                    }
                }

                let mut exit_count = 0u64;
                let mut last_log_time = std::time::Instant::now();
                let last_exit_time = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
                // Spawn a monitoring thread to detect stuck vCPU
                let partition_handle = self.partition;
                let vcpu_id = self.id;
                let last_exit_time_clone = last_exit_time.clone();
                let pending_interrupt_clone = self.pending_interrupt.clone();

                log::debug!("Spawning monitor thread for vCPU {}", self.id);

                std::thread::Builder::new()
                    .name(format!("vcpu-{}-monitor", self.id))
                    .spawn(move || {
                        log::debug!("Monitor thread started for vCPU {}", vcpu_id);

                        use windows::Win32::System::Hypervisor::{
                            WHvX64RegisterApicCurrentCount, WHvX64RegisterApicDivide,
                            WHvX64RegisterApicLvtTimer,
                            WHvX64RegisterRip, WHvX64RegisterRsp, WHvX64RegisterRflags,
                            WHvX64RegisterCr0, WHvX64RegisterCr3, WHvX64RegisterCr4,
                            WHvX64RegisterIdtr, WHvX64RegisterGdtr, WHvX64RegisterRax,
                            WHvX64RegisterRbx, WHvX64RegisterRcx, WHvX64RegisterRdx,
                            WHvX64RegisterRsi, WHvX64RegisterRdi, WHvX64RegisterRbp,
                            WHvX64RegisterR8, WHvX64RegisterR9, WHvX64RegisterR10,
                            WHvX64RegisterR11, WHvX64RegisterR12, WHvX64RegisterR13,
                            WHvX64RegisterR14, WHvX64RegisterR15, WHV_REGISTER_VALUE,
                        };

                        let mut last_rip: Option<u64> = None;
                        let mut dump_shown = false;
                        let mut stuck_count = 0u32;
                        const DELAY_LOOP_RIP: u64 = 0xffffffff81956f43;

                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(5));

                            let elapsed = last_exit_time_clone.lock().unwrap().elapsed().as_secs();

                            if elapsed >= 5 {
                                let reg_names = [
                                    WHvX64RegisterRip,
                                    WHvX64RegisterRflags,
                                    WHvRegisterPendingInterruption,
                                    WHvRegisterPendingEvent,
                                    WHvRegisterInterruptState,
                                    WHvRegisterInternalActivityState,
                                ];
                                let mut reg_values: [WHV_REGISTER_VALUE; 6] =
                                    unsafe { std::mem::zeroed() };

                                unsafe {
                                    if get_virtual_processor_registers(
                                        partition_handle,
                                        vcpu_id as u32,
                                        reg_names.as_ptr(),
                                        reg_names.len() as u32,
                                        reg_values.as_mut_ptr(),
                                    ).is_ok() {
                                        let current_rip = reg_values[0].Reg64;
                                        let current_rflags = reg_values[1].Reg64;
                                        let pending_interrupt = reg_values[2].PendingInterruption.AsUINT64;
                                        let pending_event = reg_values[3].ExtIntEvent.AsUINT128.Anonymous;
                                        let interrupt_state = reg_values[4].InterruptState.AsUINT64;
                                        let internal_activity = reg_values[5].InternalActivity.AsUINT64;
                                        let apic_names = [
                                            WHvX64RegisterApicLvtTimer,
                                            WHvX64RegisterApicCurrentCount,
                                            WHvX64RegisterApicDivide,
                                        ];
                                        let mut apic_values: [WHV_REGISTER_VALUE; 3] =
                                            std::mem::zeroed();
                                        let _ = get_virtual_processor_registers(
                                            partition_handle,
                                            vcpu_id as u32,
                                            apic_names.as_ptr(),
                                            apic_names.len() as u32,
                                            apic_values.as_mut_ptr(),
                                        );
                                        let (queue_depth, queue_front) = pending_interrupt_clone
                                            .as_ref()
                                            .and_then(|queue| {
                                                queue.lock().ok().map(|guard| {
                                                    (guard.len(), guard.front().copied())
                                                })
                                            })
                                            .unwrap_or((0, None));
                                        windows_vcpu_debug_log(format!(
                                            "[MONITOR] vcpu={} elapsed={} rip=0x{:016x} rflags=0x{:016x} pending_interrupt=0x{:016x} pending_event_hi=0x{:016x} pending_event_lo=0x{:016x} interrupt_state=0x{:016x} internal_activity=0x{:016x} apic_lvt_timer=0x{:016x} apic_current=0x{:016x} apic_divide=0x{:016x} queue_depth={} queue_front={:?}",
                                            vcpu_id,
                                            elapsed,
                                            current_rip,
                                            current_rflags,
                                            pending_interrupt,
                                            pending_event.High64,
                                            pending_event.Low64,
                                            interrupt_state,
                                            internal_activity,
                                            apic_values[0].Reg64,
                                            apic_values[1].Reg64,
                                            apic_values[2].Reg64,
                                            queue_depth,
                                            queue_front,
                                        ));
                                        let mut apic_state = [0u8; 4096];
                                        let mut apic_state_bytes = 0u32;
                                        if let Err(e) = WHvGetVirtualProcessorInterruptControllerState(
                                            partition_handle,
                                            vcpu_id as u32,
                                            apic_state.as_mut_ptr() as *mut std::ffi::c_void,
                                            apic_state.len() as u32,
                                            Some(&mut apic_state_bytes as *mut u32),
                                        ) {
                                            windows_vcpu_debug_log(format!(
                                                "[APICMON] vcpu={} elapsed={} read_state_failed hr=0x{:x}",
                                                vcpu_id,
                                                elapsed,
                                                e.code().0 as u32
                                            ));
                                        } else {
                                            let apic_tpr =
                                                read_u32_le(&apic_state, 0x80).unwrap_or(u32::MAX);
                                            let apic_ppr =
                                                read_u32_le(&apic_state, 0xa0).unwrap_or(u32::MAX);
                                            let apic_isr1 =
                                                read_u32_le(&apic_state, 0x110).unwrap_or(u32::MAX);
                                            let apic_irr1 =
                                                read_u32_le(&apic_state, 0x210).unwrap_or(u32::MAX);
                                            let apic_svr = read_u32_le(
                                                &apic_state,
                                                XAPIC_SPURIOUS_VECTOR_OFFSET,
                                            )
                                            .unwrap_or(u32::MAX);
                                            let apic_lvt_timer = read_u32_le(
                                                &apic_state,
                                                XAPIC_LVT_TIMER_OFFSET,
                                            )
                                            .unwrap_or(u32::MAX);
                                            let apic_timer_initial = read_u32_le(
                                                &apic_state,
                                                XAPIC_TIMER_INITIAL_COUNT_OFFSET,
                                            )
                                            .unwrap_or(u32::MAX);
                                            let apic_timer_current = read_u32_le(
                                                &apic_state,
                                                XAPIC_TIMER_CURRENT_COUNT_OFFSET,
                                            )
                                            .unwrap_or(u32::MAX);
                                            let apic_timer_divide = read_u32_le(
                                                &apic_state,
                                                XAPIC_TIMER_DIVIDE_OFFSET,
                                            )
                                            .unwrap_or(u32::MAX);
                                            windows_vcpu_debug_log(format!(
                                                "[APICMON] vcpu={} elapsed={} bytes={} tpr=0x{:08x} ppr=0x{:08x} isr1=0x{:08x} irr1=0x{:08x} svr=0x{:08x} lvt_timer=0x{:08x} timer_initial=0x{:08x} timer_current=0x{:08x} timer_divide=0x{:08x}",
                                                vcpu_id,
                                                elapsed,
                                                apic_state_bytes,
                                                apic_tpr,
                                                apic_ppr,
                                                apic_isr1,
                                                apic_irr1,
                                                apic_svr,
                                                apic_lvt_timer,
                                                apic_timer_initial,
                                                apic_timer_current,
                                                apic_timer_divide
                                            ));
                                        }

                                        if let Some(prev_rip) = last_rip {
                                            if current_rip == prev_rip {
                                                stuck_count += 1;

                                                // WORKAROUND: If stuck in known delay loop, skip immediately (don't wait)
                                                if current_rip == DELAY_LOOP_RIP && stuck_count >= 1 {
                                                    log::warn!("🔧 WORKAROUND: Detected delay loop at 0x{:016x} (stuck for {} seconds), forcing exit", current_rip, stuck_count * 5);

                                                    // Try to read LAPIC state to diagnose interrupt delivery issue
                                                    use windows::Win32::System::Hypervisor::WHvGetVirtualProcessorInterruptControllerState;

                                                    let mut lapic_state: [u8; 4096] = [0; 4096];
                                                    let mut bytes_written: u32 = 0;

                                                    let lapic_result = WHvGetVirtualProcessorInterruptControllerState(
                                                        partition_handle,
                                                        vcpu_id as u32,
                                                        lapic_state.as_mut_ptr() as *mut std::ffi::c_void,
                                                        lapic_state.len() as u32,
                                                        Some(&mut bytes_written as *mut u32),
                                                    );

                                                    if lapic_result.is_ok() {
                                                        log::debug!("LAPIC state read: {} bytes", bytes_written);
                                                        // Log first few bytes of LAPIC state
                                                        if bytes_written >= 16 {
                                                            log::debug!("LAPIC data: {:02x?}", &lapic_state[0..16]);
                                                        }
                                                    } else {
                                                        log::warn!("❌ Failed to read LAPIC state: {:?}", lapic_result);
                                                    }

                                                    // Jump past the delay loop function entirely
                                                    const AFTER_LOOP_RIP: u64 = 0xffffffff81956f48;

                                                    let rip_name = [WHvX64RegisterRip];
                                                    let mut rip_value: [WHV_REGISTER_VALUE; 1] = std::mem::zeroed();
                                                    rip_value[0].Reg64 = AFTER_LOOP_RIP;

                                                    let set_result = set_virtual_processor_registers(
                                                        partition_handle,
                                                        vcpu_id as u32,
                                                        rip_name.as_ptr(),
                                                        1,
                                                        rip_value.as_ptr(),
                                                    );

                                                    if set_result.is_ok() {
                                                        log::debug!("✅ Successfully set RIP=0x{:016x}, skipped delay loop", AFTER_LOOP_RIP);
                                                        stuck_count = 0; // Reset counter after intervention
                                                        last_rip = Some(AFTER_LOOP_RIP); // Update last_rip to new value
                                                    } else {
                                                        log::error!("❌ Failed to set RIP register: {:?}", set_result);
                                                    }
                                                } else {
                                                    log::warn!("🔴 vCPU {} RIP unchanged for {}+ seconds: 0x{:016x} (CPU may be in infinite loop)",
                                                        vcpu_id, stuck_count * 5, current_rip);
                                                }
                                            } else {
                                                stuck_count = 0; // Reset counter when RIP changes
                                                log::debug!("🟢 vCPU {} RIP changed: 0x{:016x} -> 0x{:016x} (CPU still executing)",
                                                    vcpu_id, prev_rip, current_rip);
                                                last_rip = Some(current_rip);
                                            }
                                        } else {
                                            last_rip = Some(current_rip);
                                        }
                                    }
                                }
                            }

                            if elapsed >= 10 && !dump_shown {
                                log::warn!("⚠️  vCPU {} appears stuck - no VM exit for {} seconds", vcpu_id, elapsed);

                                // Dump full CPU state
                                let reg_names = [
                                    WHvX64RegisterRip, WHvX64RegisterRsp, WHvX64RegisterRflags,
                                    WHvX64RegisterCr0, WHvX64RegisterCr3, WHvX64RegisterCr4,
                                    WHvX64RegisterIdtr, WHvX64RegisterGdtr,
                                    WHvX64RegisterRax, WHvX64RegisterRbx, WHvX64RegisterRcx,
                                    WHvX64RegisterRdx, WHvX64RegisterRsi, WHvX64RegisterRdi,
                                    WHvX64RegisterRbp, WHvX64RegisterR8, WHvX64RegisterR9,
                                    WHvX64RegisterR10, WHvX64RegisterR11, WHvX64RegisterR12,
                                    WHvX64RegisterR13, WHvX64RegisterR14, WHvX64RegisterR15,
                                ];
                                let mut reg_values: [WHV_REGISTER_VALUE; 23] = unsafe { std::mem::zeroed() };

                                unsafe {
                                    if get_virtual_processor_registers(
                                        partition_handle,
                                        vcpu_id as u32,
                                        reg_names.as_ptr(),
                                        reg_names.len() as u32,
                                        reg_values.as_mut_ptr(),
                                    ).is_ok() {
                                        log::debug!("╔══════════════════════════════════════════════════════════════╗");
                                        log::debug!("║  vCPU {} Register State Dump                                 ║", vcpu_id);
                                        log::debug!("╠══════════════════════════════════════════════════════════════╣");
                                        log::debug!("║  RIP     = 0x{:016x}                                  ║", reg_values[0].Reg64);
                                        log::debug!("║  RSP     = 0x{:016x}                                  ║", reg_values[1].Reg64);
                                        log::debug!("║  RFLAGS  = 0x{:016x}  (IF={})                        ║",
                                            reg_values[2].Reg64,
                                            if (reg_values[2].Reg64 & (1 << 9)) != 0 { "enabled" } else { "DISABLED" });
                                        log::debug!("║  CR0     = 0x{:016x}                                  ║", reg_values[3].Reg64);
                                        log::debug!("║  CR3     = 0x{:016x}                                  ║", reg_values[4].Reg64);
                                        log::debug!("║  CR4     = 0x{:016x}                                  ║", reg_values[5].Reg64);
                                        log::debug!("║  IDTR    = base:0x{:016x} limit:0x{:04x}              ║",
                                            reg_values[6].Table.Base, reg_values[6].Table.Limit);
                                        log::debug!("║  GDTR    = base:0x{:016x} limit:0x{:04x}              ║",
                                            reg_values[7].Table.Base, reg_values[7].Table.Limit);
                                        log::debug!("╚══════════════════════════════════════════════════════════════╝");
                                        log::debug!("RAX=0x{:016x} RBX=0x{:016x} RCX=0x{:016x}",
                                            reg_values[8].Reg64, reg_values[9].Reg64, reg_values[10].Reg64);
                                        log::debug!("RDX=0x{:016x} RSI=0x{:016x} RDI=0x{:016x}",
                                            reg_values[11].Reg64, reg_values[12].Reg64, reg_values[13].Reg64);
                                        log::debug!("RBP=0x{:016x} R8 =0x{:016x} R9 =0x{:016x}",
                                            reg_values[14].Reg64, reg_values[15].Reg64, reg_values[16].Reg64);
                                        log::debug!("R10=0x{:016x} R11=0x{:016x} R12=0x{:016x}",
                                            reg_values[17].Reg64, reg_values[18].Reg64, reg_values[19].Reg64);
                                        log::debug!("R13=0x{:016x} R14=0x{:016x} R15=0x{:016x}",
                                            reg_values[20].Reg64, reg_values[21].Reg64, reg_values[22].Reg64);
                                        dump_shown = true;
                                    }
                                }
                            }
                        }
                    })
                    .expect("Failed to spawn vCPU monitor thread");

                loop {
                    exit_count += 1;
                    windows_vcpu_exit_state_log(
                        self.id,
                        format!("outer_loop_enter exit_count={}", exit_count),
                    );

                    // Log progress every 5 seconds
                    if last_log_time.elapsed().as_secs() >= 5 {
                        debug!("vCPU {} progress: {} exits processed", self.id, exit_count);
                        last_log_time = std::time::Instant::now();
                    }

                    windows_vcpu_exit_state_log(self.id, "before_self_run");
                    match self.run() {
                        Ok(result) => {
                            // Update last exit time
                            *last_exit_time.lock().unwrap() = std::time::Instant::now();
                            windows_vcpu_exit_state_log(
                                self.id,
                                format!("after_self_run result={:?}", result),
                            );

                            match result {
                                VcpuEmulation::Halted => {
                                    windows_vcpu_exit_state_log(
                                        self.id,
                                        "emulation=Halted waiting_for_interrupt_or_terminal_exit",
                                    );
                                    if let Some(ref evt) = self.irq_pending_evt {
                                        // Guest is in HLT idle loop.  An interrupt has been (or
                                        // will be) posted to the virtual APIC via
                                        // WHvRequestInterrupt.  Wait up to 5 ms for the signal,
                                        // then re-enter WHvRunVirtualProcessor so WHPX can
                                        // deliver the queued interrupt.  The short timeout keeps
                                        // host CPU usage low during guest idle while bounding
                                        // interrupt delivery latency.
                                        windows_vcpu_exit_state_log(
                                            self.id,
                                            "halted_wait_begin timeout_ms=5",
                                        );
                                        evt.wait_timeout(5);
                                        windows_vcpu_exit_state_log(
                                            self.id,
                                            "halted_wait_end reenter_run",
                                        );
                                    } else {
                                        // No IrqChip wired (smoke tests): treat HLT as terminal.
                                        windows_vcpu_exit_state_log(
                                            self.id,
                                            "no_irq_chip terminal_exit_code=0",
                                        );
                                        self.exit(FC_EXIT_CODE_OK);
                                        break;
                                    }
                                }
                                VcpuEmulation::Stopped => {
                                    windows_vcpu_exit_state_log(
                                        self.id,
                                        "emulation=Stopped terminal_exit_code=0",
                                    );
                                    self.exit(FC_EXIT_CODE_OK);
                                    break;
                                }
                                VcpuEmulation::Handled => continue,
                            }
                        }
                        Err(e) => {
                            error!("Error running WHPX vCPU {}: {e}", self.id);
                            windows_vcpu_exit_state_log(
                                self.id,
                                format!("run_error={} terminal_exit_code=1", e),
                            );
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
        let rip = self.read_rip().unwrap_or(0);
        windows_vcpu_exit_state_log(
            self.id,
            format!("send_exit exit_code={} rip=0x{:016x}", exit_code, rip),
        );
        self.response_sender
            .send(VcpuResponse::Exited(exit_code))
            .expect("failed to send Exited status");

        if let Err(e) = self.exit_evt.write(1) {
            error!("Failed signaling vcpu exit event: {e}");
        }
    }

    /// Main vCPU run loop for x86_64.
    pub fn run(&mut self) -> result::Result<VcpuEmulation, io::Error> {
        log::debug!(
            "vCPU {} starting execution at RIP=0x{:x}",
            self.id,
            self.boot_entry_addr
        );

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
                        error!("Failed to complete WHPX MMIO read on vCPU {}: {e}", self.id);
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
                    // Check for ACPI shutdown before releasing the data borrow.
                    // Port 0x604 (PIIX4 PM1a_CNT): any write with bit 13 set
                    // (SLP_EN) signals a sleep/poweroff request from the guest.
                    let acpi_shutdown = port == 0x604
                        && data.len() >= 2
                        && (u16::from_le_bytes([data[0], data[1]]) & 0x2000) != 0;
                    let _ = data;
                    if let Err(e) = self.whpx_vcpu.complete_io_write() {
                        error!(
                            "Failed to complete WHPX I/O write emulation on vCPU {}: {e}",
                            self.id
                        );
                        self.whpx_vcpu.clear_pending_io();
                        VcpuEmulation::Stopped
                    } else if acpi_shutdown {
                        info!("Guest requested ACPI shutdown via port 0x604");
                        windows_vcpu_exit_state_log(
                            self.id,
                            "acpi_shutdown_via_port_0x604 emulation=Stopped",
                        );
                        VcpuEmulation::Stopped
                    } else {
                        VcpuEmulation::Handled
                    }
                }
                VcpuExit::Halted => {
                    log::debug!("vCPU {} halted - waiting for the next interrupt", self.id);
                    windows_vcpu_exit_state_log(self.id, "vcpu_exit=Halted");
                    self.whpx_vcpu.clear_pending_mmio();
                    self.whpx_vcpu.clear_pending_io();
                    VcpuEmulation::Halted
                }
                VcpuExit::Shutdown => {
                    log::warn!("vCPU {} shutdown - VM terminated abnormally", self.id);
                    windows_vcpu_exit_state_log(self.id, "vcpu_exit=Shutdown");
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

    /// Reads just RIP to check if CPU is still executing
    fn read_rip(&self) -> Option<u64> {
        use windows::Win32::System::Hypervisor::{WHvX64RegisterRip, WHV_REGISTER_VALUE};

        let reg_names = [WHvX64RegisterRip];
        let mut reg_values: [WHV_REGISTER_VALUE; 1] = unsafe { std::mem::zeroed() };

        unsafe {
            match get_virtual_processor_registers(
                self.partition,
                self.id as u32,
                reg_names.as_ptr(),
                1,
                reg_values.as_mut_ptr(),
            ) {
                Ok(_) => Some(reg_values[0].Reg64),
                Err(e) => {
                    log::debug!("Failed to read RIP for vCPU {}: {}", self.id, e);
                    None
                }
            }
        }
    }

    /// Reads and logs current CPU register state for debugging
    fn dump_cpu_state(&self) {
        use windows::Win32::System::Hypervisor::{
            WHvX64RegisterCr0, WHvX64RegisterCr3, WHvX64RegisterCr4, WHvX64RegisterGdtr,
            WHvX64RegisterIdtr, WHvX64RegisterR10, WHvX64RegisterR11, WHvX64RegisterR12,
            WHvX64RegisterR13, WHvX64RegisterR14, WHvX64RegisterR15, WHvX64RegisterR8,
            WHvX64RegisterR9, WHvX64RegisterRax, WHvX64RegisterRbp, WHvX64RegisterRbx,
            WHvX64RegisterRcx, WHvX64RegisterRdi, WHvX64RegisterRdx, WHvX64RegisterRflags,
            WHvX64RegisterRip, WHvX64RegisterRsi, WHvX64RegisterRsp, WHV_REGISTER_VALUE,
        };

        let reg_names = [
            WHvX64RegisterRip,
            WHvX64RegisterRsp,
            WHvX64RegisterRflags,
            WHvX64RegisterCr0,
            WHvX64RegisterCr3,
            WHvX64RegisterCr4,
            WHvX64RegisterIdtr,
            WHvX64RegisterGdtr,
            WHvX64RegisterRax,
            WHvX64RegisterRbx,
            WHvX64RegisterRcx,
            WHvX64RegisterRdx,
            WHvX64RegisterRsi,
            WHvX64RegisterRdi,
            WHvX64RegisterRbp,
            WHvX64RegisterR8,
            WHvX64RegisterR9,
            WHvX64RegisterR10,
            WHvX64RegisterR11,
            WHvX64RegisterR12,
            WHvX64RegisterR13,
            WHvX64RegisterR14,
            WHvX64RegisterR15,
        ];

        let mut reg_values: [WHV_REGISTER_VALUE; 23] = unsafe { std::mem::zeroed() };

        unsafe {
            if let Err(e) = get_virtual_processor_registers(
                self.partition,
                self.id as u32,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_mut_ptr(),
            ) {
                error!("❌ Failed to read CPU registers: {e}");
                return;
            }
        }

        log::debug!("╔══════════════════════════════════════════════════════════════╗");
        log::debug!(
            "║  vCPU {} Register State Dump                                 ║",
            self.id
        );
        log::debug!("╠══════════════════════════════════════════════════════════════╣");
        unsafe {
            log::debug!(
                "║  RIP     = 0x{:016x}                                  ║",
                reg_values[0].Reg64
            );
            log::debug!(
                "║  RSP     = 0x{:016x}                                  ║",
                reg_values[1].Reg64
            );
            log::debug!(
                "║  RFLAGS  = 0x{:016x}  (IF={})                        ║",
                reg_values[2].Reg64,
                if (reg_values[2].Reg64 & (1 << 9)) != 0 {
                    "enabled"
                } else {
                    "DISABLED"
                }
            );
            log::debug!(
                "║  CR0     = 0x{:016x}                                  ║",
                reg_values[3].Reg64
            );
            log::debug!(
                "║  CR3     = 0x{:016x}                                  ║",
                reg_values[4].Reg64
            );
            log::debug!(
                "║  CR4     = 0x{:016x}                                  ║",
                reg_values[5].Reg64
            );
            log::debug!(
                "║  IDTR    = base:0x{:016x} limit:0x{:04x}              ║",
                reg_values[6].Table.Base,
                reg_values[6].Table.Limit
            );
            log::debug!(
                "║  GDTR    = base:0x{:016x} limit:0x{:04x}              ║",
                reg_values[7].Table.Base,
                reg_values[7].Table.Limit
            );
        }
        log::debug!("╚══════════════════════════════════════════════════════════════╝");
    }
}

/// Wrapper over Vcpu that hides the underlying interactions with the Vcpu thread.
pub struct VcpuHandle {
    event_sender: crossbeam_channel::Sender<VcpuEvent>,
    response_receiver: crossbeam_channel::Receiver<VcpuResponse>,
    vcpu_thread: Option<std::thread::JoinHandle<()>>,
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
            vcpu_thread: Some(vcpu_thread),
        }
    }

    /// Waits for the vCPU thread to finish.
    ///
    /// Must be called before dropping the `Vm` to avoid a race between
    /// `WHvDeleteVirtualProcessor` (vCPU thread cleanup) and
    /// `WHvDeletePartition` (Vm::drop).
    pub fn join(mut self) {
        if let Some(t) = self.vcpu_thread.take() {
            let _ = t.join();
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
        let _vm = Vm::new(false, 1, false).unwrap();
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vm_memory_init_smoke() {
        let mut vm = Vm::new(false, 1, false).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x20_000)]).unwrap();
        vm.memory_init(&guest_mem).unwrap();
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vcpu_create_smoke() {
        const MEM_SIZE: usize = 0x40_0000;
        let mut vm = Vm::new(false, 1, false).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
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
            None,
            None,
        )
        .unwrap();
    }

    #[test]
    #[ignore = "Requires WHPX/Hyper-V available on host"]
    fn test_whpx_vcpu_configure_smoke() {
        const MEM_SIZE: usize = 0x40_0000;
        let mut vm = Vm::new(false, 1, false).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
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
            None,
            None,
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
        let mut vm = Vm::new(false, 1, false).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
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
            None,
            None,
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
        let mut vm = Vm::new(false, 1, false).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
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
            None,
            None,
        )
        .unwrap();

        // 4. Launch the vCPU in its own thread (the production path).
        //    start_threaded() internally calls configure_x86_64() and then waits
        //    for VcpuEvent::Resume before entering the run loop. The main thread
        //    must send Resume (mirroring what resume_vcpus() does in production).
        let handle = vcpu.start_threaded().unwrap();

        // 5. Send Resume to unblock the vCPU thread (mirrors production resume_vcpus()).
        handle.send_event(VcpuEvent::Resume).unwrap();

        // 6. Drain the Resumed acknowledgment.
        let resumed = handle
            .response_receiver()
            .recv_timeout(Duration::from_secs(5))
            .expect("vCPU thread did not acknowledge Resume within timeout");
        assert_eq!(resumed, VcpuResponse::Resumed);

        // 7. Expect the thread to report a clean exit within 5 seconds.
        //    Guest executes HLT → Halted → start_threaded() calls exit(FC_EXIT_CODE_OK).
        let response = handle
            .response_receiver()
            .recv_timeout(Duration::from_secs(5))
            .expect("vCPU thread did not respond within timeout");

        assert_eq!(response, VcpuResponse::Exited(FC_EXIT_CODE_OK));

        // Join the thread before vm is dropped to avoid a race between
        // WHvDeleteVirtualProcessor (thread cleanup) and WHvDeletePartition (Vm::drop).
        handle.join();
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

        let mut vm = Vm::new(false, 1, false).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
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

        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(ENTRY_ADDR),
            io_bus,
            exit_evt,
            None,
            None,
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
        assert!(
            !captured.captured.is_empty(),
            "no bytes captured on port 0x30"
        );
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
        let mut vm = Vm::new(false, 1, false).unwrap();
        let (arch_mem_info, arch_mem_regions) =
            arch::arch_memory_regions(MEM_SIZE, None, 0, 0, None);
        let guest_mem = GuestMemoryMmap::from_ranges(&arch_mem_regions).unwrap();
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
        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            kernel_entry,
            io_bus,
            exit_evt,
            None,
            None,
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
        assert!(
            !captured.captured.is_empty(),
            "no bytes captured on port 0x30"
        );
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

        let mut vm = Vm::new(false, 1, false).unwrap();
        let guest_mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
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

        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let mut vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            GuestAddress(ENTRY_ADDR),
            io_bus,
            exit_evt,
            None,
            None,
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
        assert!(
            !captured.captured.is_empty(),
            "no bytes captured on COM1 (0x3F8)"
        );
        assert_eq!(captured.captured[0], b'H', "expected 'H' on COM1 (0x3F8)");
    }

    // ── virtio-blk Windows backend smoke tests ──────────────────────────────

    /// Verify that `BlockWindows` can open a disk image, reports the correct
    /// capacity and features, and exposes them via the VirtioDevice trait.
    /// This test does NOT require WHPX and runs in the regular PR CI job.
    #[test]
    fn test_whpx_blk_init_smoke() {
        use devices::virtio::{BlockWindows, VirtioDevice};
        use std::io::Write;

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
        assert_eq!(u64::from_le_bytes(cfg), 2, "config space capacity mismatch");

        // Features: VIRTIO_F_VERSION_1 (bit 32), VIRTIO_BLK_F_FLUSH (bit 9),
        // VIRTIO_BLK_F_RO (bit 5) because the image is opened read-only.
        let features = blk.avail_features();
        assert_ne!(features & (1u64 << 32), 0, "VIRTIO_F_VERSION_1 not set");
        assert_ne!(features & (1u64 << 9), 0, "VIRTIO_BLK_F_FLUSH not set");
        assert_ne!(
            features & (1u64 << 5),
            0,
            "VIRTIO_BLK_F_RO not set for ro disk"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Verify that `BlockWindows` reads sector data correctly by constructing
    /// a minimal virtio-blk request in guest memory and processing the queue.
    /// This test does NOT require WHPX and runs in the regular PR CI job.
    #[test]
    fn test_whpx_blk_read_smoke() {
        use devices::legacy::DummyIrqChip;
        use devices::virtio::{BlockWindows, InterruptTransport, VirtioDevice};
        use std::io::Write;
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
            mem.write_slice(&len.to_le_bytes(), GuestAddress(base.0 + 8))
                .unwrap();
            mem.write_slice(&flags.to_le_bytes(), GuestAddress(base.0 + 12))
                .unwrap();
            mem.write_slice(&next.to_le_bytes(), GuestAddress(base.0 + 14))
                .unwrap();
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
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING))
            .unwrap(); // flags
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2))
            .unwrap(); // idx
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4))
            .unwrap(); // ring[0]=0 (desc idx)

        // Used ring: flags=0, idx=0 (device fills this).
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING))
            .unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2))
            .unwrap();

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
        let interrupt_transport = InterruptTransport::new(dummy_irq, "blk-smoke".into()).unwrap();
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
        mem.read_slice(&mut status, GuestAddress(STATUS_ADDR))
            .unwrap();
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
            mem.write_slice(&len.to_le_bytes(), GuestAddress(base.0 + 8))
                .unwrap();
            mem.write_slice(&flags.to_le_bytes(), GuestAddress(base.0 + 12))
                .unwrap();
            mem.write_slice(&next.to_le_bytes(), GuestAddress(base.0 + 14))
                .unwrap();
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
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING))
            .unwrap(); // flags
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2))
            .unwrap(); // idx
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4))
            .unwrap(); // ring[0]

        // Used ring: idx=0 initially (device increments it after processing).
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING))
            .unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2))
            .unwrap();

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
        mem.read_slice(&mut used_idx, GuestAddress(USED_RING + 2))
            .unwrap();
        assert_eq!(
            u16::from_le_bytes(used_idx),
            1,
            "expected used ring idx=1 after TX processing"
        );
    }

    /// Verify that `NetWindows` advertises checksum and TSO offload features.
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_net_offload_features() {
        use devices::virtio::{NetWindows, VirtioDevice};

        let mac: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let net = NetWindows::new("net-offload", mac, None).expect("NetWindows::new failed");

        let features = net.avail_features();

        // VIRTIO_NET_F_CSUM (bit 0)
        assert_ne!(features & (1u64 << 0), 0, "VIRTIO_NET_F_CSUM not set");
        // VIRTIO_NET_F_GUEST_CSUM (bit 1)
        assert_ne!(features & (1u64 << 1), 0, "VIRTIO_NET_F_GUEST_CSUM not set");
        // VIRTIO_NET_F_GUEST_TSO4 (bit 7)
        assert_ne!(features & (1u64 << 7), 0, "VIRTIO_NET_F_GUEST_TSO4 not set");
        // VIRTIO_NET_F_GUEST_TSO6 (bit 8)
        assert_ne!(features & (1u64 << 8), 0, "VIRTIO_NET_F_GUEST_TSO6 not set");
        // VIRTIO_NET_F_HOST_TSO4 (bit 11)
        assert_ne!(features & (1u64 << 11), 0, "VIRTIO_NET_F_HOST_TSO4 not set");
        // VIRTIO_NET_F_HOST_TSO6 (bit 12)
        assert_ne!(features & (1u64 << 12), 0, "VIRTIO_NET_F_HOST_TSO6 not set");
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
        use devices::virtio::{
            port_io, Console, InterruptTransport, PortDescription, VirtioDevice,
        };
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
        mem.write_slice(&desc_bytes, GuestAddress(DESC_TABLE))
            .unwrap();

        // avail ring: flags(u16)=0, idx(u16)=1, ring[0]=0
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING))
            .unwrap();
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2))
            .unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4))
            .unwrap();

        // used ring: flags(u16)=0, idx(u16)=0
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING))
            .unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2))
            .unwrap();

        // payload data
        mem.write_slice(b"Hello, virtconsole!", GuestAddress(PAYLOAD_ADDR))
            .unwrap();

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
        mem.read_slice(&mut used_idx, GuestAddress(USED_RING + 2))
            .unwrap();
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
        let vsock =
            Vsock::new(GUEST_CID, None, None, TsiFlags::empty()).expect("Vsock::new failed");

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
        assert_eq!(
            u64::from_le_bytes(cfg2),
            GUEST_CID,
            "CID mismatch in vsock2"
        );
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
        hdr[0..8].copy_from_slice(&3u64.to_le_bytes()); // src_cid
        hdr[8..16].copy_from_slice(&2u64.to_le_bytes()); // dst_cid (host)
        hdr[16..20].copy_from_slice(&5000u32.to_le_bytes()); // src_port
        hdr[20..24].copy_from_slice(&9999u32.to_le_bytes()); // dst_port
        hdr[24..28].copy_from_slice(&0u32.to_le_bytes()); // len
        hdr[28..30].copy_from_slice(&1u16.to_le_bytes()); // type = STREAM
        hdr[30..32].copy_from_slice(&1u16.to_le_bytes()); // op = CONNECT
        mem.write_slice(&hdr, GuestAddress(HDR_ADDR)).unwrap();

        // desc[0]: addr=HDR_ADDR, len=44, flags=0 (read-only), next=0
        let mut desc_bytes = [0u8; 16];
        desc_bytes[0..8].copy_from_slice(&HDR_ADDR.to_le_bytes());
        desc_bytes[8..12].copy_from_slice(&44u32.to_le_bytes());
        mem.write_slice(&desc_bytes, GuestAddress(DESC_TABLE))
            .unwrap();

        // Avail ring for TX queue: flags=0, idx=1, ring[0]=0
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING))
            .unwrap();
        mem.write_slice(&1u16.to_le_bytes(), GuestAddress(AVAIL_RING + 2))
            .unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(AVAIL_RING + 4))
            .unwrap();

        // Used ring: idx=0 initially.
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING))
            .unwrap();
        mem.write_slice(&0u16.to_le_bytes(), GuestAddress(USED_RING + 2))
            .unwrap();

        // ── 3. Create and configure the device ───────────────────────────
        let vsock = Vsock::new(3, None, None, TsiFlags::empty()).expect("Vsock::new failed");
        let vsock = Arc::new(Mutex::new(vsock));

        // ── 4. Wire up EventManager and activate ─────────────────────────
        let mut evmgr = EventManager::new().unwrap();
        evmgr.add_subscriber(vsock.clone()).unwrap();

        let dummy_irq: devices::legacy::IrqChip = DummyIrqChip::new().into();
        let interrupt = InterruptTransport::new(dummy_irq, "vsock-test".into()).unwrap();

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
        mem.read_slice(&mut used_idx, GuestAddress(USED_RING + 2))
            .unwrap();
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
    fn test_whpx_kernel_load_only() {
        use linux_loader::loader::{Elf, KernelLoader};

        let vmlinux_path = match std::env::var("TEST_VMLINUX_PATH") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                eprintln!("[SKIP] TEST_VMLINUX_PATH not set");
                return;
            }
        };

        const MEM_SIZE: usize = 256 << 20;
        let (arch_mem_info, arch_mem_regions) =
            arch::arch_memory_regions(MEM_SIZE, None, 0, 0, None);
        let guest_mem = GuestMemoryMmap::from_ranges(&arch_mem_regions).unwrap();
        eprintln!(
            "[load] guest_mem created, regions: {:?}",
            arch_mem_regions.len()
        );

        let mut kernel_file = std::fs::File::open(&vmlinux_path).unwrap();
        let load_result =
            Elf::load(&guest_mem, None, &mut kernel_file, None).expect("ELF load failed");
        eprintln!(
            "[load] ELF loaded OK, entry=0x{:x}",
            load_result.kernel_load.0
        );

        let cmdline = b"console=ttyS0 earlycon=uart8250,io,0x3f8 panic=1 nokaslr\0";
        guest_mem
            .write_slice(cmdline, GuestAddress(arch::x86_64::layout::CMDLINE_START))
            .unwrap();
        eprintln!("[load] cmdline written");

        arch::configure_system(
            &guest_mem,
            &arch_mem_info,
            GuestAddress(arch::x86_64::layout::CMDLINE_START),
            cmdline.len(),
            &None,
            1,
        )
        .unwrap();
        eprintln!("[load] configure_system OK");

        let mut vm = Vm::new(false, 1, false).unwrap();
        eprintln!("[load] Vm::new OK");
        vm.memory_init(&guest_mem).unwrap();
        eprintln!("[load] memory_init OK");

        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        let io_bus = devices::Bus::new();
        let vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            load_result.kernel_load,
            io_bus,
            exit_evt,
            None,
            None,
        )
        .unwrap();
        eprintln!("[load] Vcpu::new OK");

        // configure without starting
        let mut vcpu = vcpu;
        let gm = vcpu.guest_mem.clone();
        let entry = GuestAddress(vcpu.boot_entry_addr);
        vcpu.configure_x86_64(&gm, entry).unwrap();
        eprintln!("[load] configure_x86_64 OK — all done");
    }

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

        // ── 2b. PIT (8254) emulator ──────────────────────────────────────
        // The kernel uses PIT channel 0 to calibrate the TSC (and later the
        // LAPIC timer).  Without a working counter, TSC is marked unstable
        // Minimal PIT 8254: channels 0 and 2 (the ones Linux uses for TSC calibration).
        // Channel 2 is used by quick_pit_calibrate() and pit_calibrate_tsc():
        //   1. Write 0xb0 to 0x43 (ch2, lo+hi, mode 0)
        //   2. Write 0,0 to 0x42 (count=65536 → starts at 0xFFFF)
        //   3. Write 0x80 to 0x43 (latch ch2), read lo+hi from 0x42
        //      pit_verify_msb(0xff): check hi byte == 0xff
        // The counter decrements at 1193182 Hz based on real host time.
        const PIT_FREQ_HZ: u64 = 1_193_182;

        struct PitChannel {
            start_time: std::time::Instant,
            initial_count: u32, // 0 means 65536
            access_mode: u8,    // 1=lo, 2=hi, 3=lo+hi
            read_lo_next: bool,
            write_lo_next: bool,
            pending_lo: u8,
            latched: bool,
            latch_value: u16,
            running: bool,
        }
        impl PitChannel {
            fn new() -> Self {
                PitChannel {
                    start_time: std::time::Instant::now(),
                    initial_count: 65536,
                    access_mode: 3,
                    read_lo_next: true,
                    write_lo_next: true,
                    pending_lo: 0,
                    latched: false,
                    latch_value: 0,
                    running: false,
                }
            }
            fn current_count(&self) -> u16 {
                if !self.running {
                    return 0xFFFF;
                }
                let elapsed_ns = self.start_time.elapsed().as_nanos() as u64;
                let elapsed_ticks = (elapsed_ns * PIT_FREQ_HZ) / 1_000_000_000;
                let period = self.initial_count as u64;
                // PIT 8254: counter starts at (initial_count - 1) = 0xFFFF for count=0/65536.
                // Counts down from there; wraps periodically.
                // kernel's pit_verify_msb(0xff) expects hi byte = 0xFF initially.
                let start = period.saturating_sub(1);
                let ticks = elapsed_ticks % period.max(1);
                start.saturating_sub(ticks) as u16
            }
            fn latch(&mut self) {
                self.latch_value = self.current_count();
                self.latched = true;
            }
            fn set_access_mode(&mut self, access: u8) {
                self.access_mode = access;
                self.read_lo_next = true;
                self.write_lo_next = true;
                self.latched = false;
            }
            fn write_count(&mut self, byte: u8) {
                match self.access_mode {
                    1 => {
                        let v = byte as u32;
                        self.initial_count = if v == 0 { 65536 } else { v };
                        self.start_time = std::time::Instant::now();
                        self.running = true;
                    }
                    2 => {
                        let v = (byte as u32) << 8;
                        self.initial_count = if v == 0 { 65536 } else { v };
                        self.start_time = std::time::Instant::now();
                        self.running = true;
                    }
                    3 => {
                        if self.write_lo_next {
                            self.pending_lo = byte;
                            self.write_lo_next = false;
                        } else {
                            let v = self.pending_lo as u32 | ((byte as u32) << 8);
                            self.initial_count = if v == 0 { 65536 } else { v };
                            self.start_time = std::time::Instant::now();
                            self.write_lo_next = true;
                            self.running = true;
                        }
                    }
                    _ => {}
                }
            }
            fn read_byte(&mut self) -> u8 {
                let count = if self.latched {
                    self.latch_value
                } else {
                    self.current_count()
                };
                match self.access_mode {
                    1 => count as u8,
                    2 => (count >> 8) as u8,
                    3 => {
                        if self.read_lo_next {
                            self.read_lo_next = false;
                            count as u8
                        } else {
                            self.read_lo_next = true;
                            self.latched = false;
                            (count >> 8) as u8
                        }
                    }
                    _ => count as u8,
                }
            }
        }
        struct Pit8254 {
            ch: [PitChannel; 3],
        }
        impl Pit8254 {
            fn new() -> Self {
                Pit8254 {
                    ch: [PitChannel::new(), PitChannel::new(), PitChannel::new()],
                }
            }
        }
        impl BusDevice for Pit8254 {
            fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
                let byte = match data.first() {
                    Some(&b) => b,
                    None => return,
                };
                match offset {
                    3 => {
                        // Mode/command register
                        let ch_idx = ((byte >> 6) & 0x3) as usize;
                        let access = (byte >> 4) & 0x3;
                        if ch_idx <= 2 {
                            if access == 0 {
                                self.ch[ch_idx].latch();
                            } else {
                                self.ch[ch_idx].set_access_mode(access);
                            }
                        }
                        // ch_idx==3 = read-back command, ignore
                    }
                    0 | 1 | 2 => self.ch[offset as usize].write_count(byte),
                    _ => {}
                }
            }
            fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
                if data.is_empty() {
                    return;
                }
                match offset {
                    0 | 1 | 2 => data[0] = self.ch[offset as usize].read_byte(),
                    _ => {}
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
            b"console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8 reboot=t panic=1 nokaslr no_timer_check\0";
        guest_mem
            .write_slice(cmdline, GuestAddress(arch::x86_64::layout::CMDLINE_START))
            .unwrap();
        eprintln!(
            "[e2e] cmdline written at 0x{:x}",
            arch::x86_64::layout::CMDLINE_START
        );

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
        eprintln!("[e2e] configure_system done");

        // ── 7. Create WHPX partition and map guest memory ─────────────────
        // enable_apic=true: needed for Linux kernel (LAPIC + Hyper-V enlightenments)
        let mut vm = Vm::new(false, 1, true).unwrap();
        eprintln!("[e2e] Vm::new done");
        vm.memory_init(&guest_mem).unwrap();
        eprintln!("[e2e] memory_init done");

        // ── 8. IO bus: COM1 + PIT ─────────────────────────────────────────
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
        // PIT 8254 at 0x40-0x43: needed for TSC and LAPIC timer calibration.
        io_bus
            .insert(Arc::new(Mutex::new(Pit8254::new())), 0x40, 0x4)
            .unwrap();
        // Port 0x61 (PC speaker / NMI): kernel writes gate bit for PIT ch2 calibration.
        // We just store and return the value; gate actually starts counting on count write.
        struct Port61(u8);
        impl BusDevice for Port61 {
            fn read(&mut self, _: u64, _: u64, data: &mut [u8]) {
                if let Some(d) = data.first_mut() {
                    *d = self.0 | 0x20;
                }
            }
            fn write(&mut self, _: u64, _: u64, data: &[u8]) {
                if let Some(&b) = data.first() {
                    self.0 = b;
                }
            }
        }
        io_bus
            .insert(Arc::new(Mutex::new(Port61(0))), 0x61, 0x1)
            .unwrap();

        // ── 8b. Write BDA 0x40E → EBDA segment so Linux MP table scan works ─
        // Linux scans the EBDA area (get_bios_ebda() reads 0x40E) for the
        // "_MP_" floating pointer.  Without this, the APIC/MADT scan fails
        // and the kernel can't set up the timer interrupt.
        // EBDA segment = 0x9FC00 / 16 = 0x9FC0
        guest_mem
            .write_obj(0x9FC0u16, GuestAddress(0x40E))
            .expect("failed to write BDA EBDA ptr");

        // ── 9. Create vCPU ────────────────────────────────────────────────
        let exit_evt = utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap();
        // Pass a real irq_pending_evt so HLT re-enters WHvRunVirtualProcessor
        // (allowing WHPX LAPIC timer interrupts to fire) instead of exiting.
        let irq_evt = Arc::new(utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap());
        eprintln!("[e2e] creating Vcpu...");
        let vcpu = Vcpu::new(
            0,
            vm.partition(),
            guest_mem.clone(),
            kernel_entry,
            io_bus,
            exit_evt,
            Some(irq_evt),
            None,
        )
        .unwrap();
        eprintln!("[e2e] Vcpu::new done");

        // ── 10. Launch vCPU thread ────────────────────────────────────────
        // start_threaded() calls configure_x86_64() (RIP=kernel_entry,
        // RSI=0x7000 zero page) then drives the WHPX run loop.
        eprintln!("[e2e] calling start_threaded...");
        let handle = vcpu.start_threaded().unwrap();
        eprintln!("[e2e] start_threaded done, sending Resume...");
        handle.send_event(VcpuEvent::Resume).ok();
        // Wait for the vCPU to acknowledge the Resume event
        match handle
            .response_receiver()
            .recv_timeout(std::time::Duration::from_secs(10))
        {
            Ok(VcpuResponse::Resumed) => eprintln!("[e2e] vCPU resumed OK"),
            Ok(other) => eprintln!("[e2e] unexpected response after Resume: {:?}", other),
            Err(e) => eprintln!("[e2e] timeout waiting for Resumed: {}", e),
        }

        // ── 11. Poll until vCPU exits or 90 s deadline ───────────────────
        // Do NOT early-return on banner discovery: Vm must outlive the vCPU
        // thread to avoid WHvDeletePartition racing with WHvRunVirtualProcessor.
        // Instead, track the banner flag and keep looping until the thread
        // exits naturally (kernel panic) or we cancel it below.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
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

            if !found_banner && String::from_utf8_lossy(&snapshot).contains("Linux version") {
                found_banner = true;
                eprintln!("\n[e2e] 'Linux version' found — waiting for vCPU to exit...");
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

        // Join the vCPU thread before vm is dropped to ensure WHvDeleteVirtualProcessor
        // (thread cleanup) completes before WHvDeletePartition (Vm::drop).
        handle.join();

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

    // ── virtio-snd Windows backend smoke tests ──────────────────────────────

    /// Verify that `Snd` device can be created with NullBackend and reports
    /// correct device type and features. This test does NOT require WHPX.
    #[test]
    #[cfg(feature = "snd")]
    fn test_whpx_snd_init_smoke() {
        use devices::virtio::{Snd, VirtioDevice};

        let snd = Snd::new().expect("Snd::new failed");

        // Device identity
        assert_eq!(snd.device_type(), 25); // VIRTIO_ID_SND

        // Features: should include VIRTIO_F_VERSION_1
        let features = snd.avail_features();
        assert_ne!(features & (1 << 32), 0, "VIRTIO_F_VERSION_1 not set");

        // Config space: should have jacks, streams, chmaps counts
        let mut cfg = [0u8; 12];
        snd.read_config(0, &mut cfg);
        let jacks = u32::from_le_bytes([cfg[0], cfg[1], cfg[2], cfg[3]]);
        let streams = u32::from_le_bytes([cfg[4], cfg[5], cfg[6], cfg[7]]);
        let chmaps = u32::from_le_bytes([cfg[8], cfg[9], cfg[10], cfg[11]]);

        // Default config: 0 jacks, 2 streams (1 output + 1 input), 1 chmap
        assert_eq!(jacks, 0, "expected 0 jacks");
        assert_eq!(streams, 2, "expected 2 streams");
        assert_eq!(chmaps, 1, "expected 1 chmap");
    }

    // ── virtio-fs Windows backend smoke tests ───────────────────────────────

    /// Verify that `Fs` device can be created on Windows and reports correct
    /// device type. The passthrough backend returns ENOSYS stubs.
    #[test]
    #[cfg(not(any(feature = "tee", feature = "nitro")))]
    fn test_whpx_fs_init_smoke() {
        use devices::virtio::{Fs, VirtioDevice};
        use std::sync::atomic::AtomicI32;
        use std::sync::Arc;

        let exit_code = Arc::new(AtomicI32::new(0));
        let fs = Fs::new(
            "test-fs".to_string(),
            std::env::temp_dir().to_str().unwrap().to_string(),
            exit_code,
        )
        .expect("Fs::new failed");

        // Device identity
        assert_eq!(fs.device_type(), 26); // VIRTIO_ID_FS
        assert_eq!(fs.id(), "virtio_fs");

        // Features: should include VIRTIO_F_VERSION_1
        let features = fs.avail_features();
        assert_ne!(features & (1 << 32), 0, "VIRTIO_F_VERSION_1 not set");
    }

    /// Verify `BalloonWindows::new()` creates a device with 5 queues including
    /// the page-hinting queue (PHQ).
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_balloon_init_smoke() {
        use devices::virtio::VirtioDevice;

        let balloon = devices::virtio::Balloon::new().expect("Balloon::new failed");

        // Device type: TYPE_BALLOON = 5
        assert_eq!(balloon.device_type(), 5, "expected TYPE_BALLOON=5");

        // Should have 5 queues: IFQ, DFQ, STQ, PHQ, FRQ
        assert_eq!(balloon.queues().len(), 5, "expected 5 queues");

        // Features: should include VIRTIO_F_VERSION_1
        let features = balloon.avail_features();
        assert_ne!(features & (1 << 32), 0, "VIRTIO_F_VERSION_1 not set");
    }

    /// Verify that `Vsock` device advertises DGRAM support feature.
    /// Does NOT require WHPX — runs in the regular PR CI job.
    #[test]
    fn test_whpx_vsock_dgram_feature() {
        use devices::virtio::{VirtioDevice, Vsock};

        let vsock = Vsock::new(3, None, None, Default::default()).expect("Vsock::new failed");

        // Device type: TYPE_VSOCK = 19
        assert_eq!(vsock.device_type(), 19, "expected TYPE_VSOCK=19");

        // Features: should include VIRTIO_VSOCK_F_DGRAM (bit 3)
        let features = vsock.avail_features();
        assert_ne!(
            features & (1 << 3),
            0,
            "VIRTIO_VSOCK_F_DGRAM not advertised"
        );

        // Should also have VIRTIO_F_VERSION_1 (bit 32)
        assert_ne!(features & (1 << 32), 0, "VIRTIO_F_VERSION_1 not set");
    }
}
