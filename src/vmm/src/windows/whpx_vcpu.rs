// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! WHPX vCPU implementation for x86_64 architecture.
//!
//! This module provides the WhpxVcpu wrapper around Windows Hypervisor Platform
//! virtual processor APIs, handling VM exits and vCPU execution.
//!
//! # Architecture
//!
//! The vCPU run loop follows this flow:
//! 1. `WhpxVcpu::run()` calls `WHvRunVirtualProcessor` to execute guest code
//! 2. When a VM exit occurs, the exit context is parsed into a `VcpuExit` enum
//! 3. The `VcpuExit` is returned to the caller (typically `Vcpu::run()`)
//! 4. The caller (`Vcpu::run()`) handles the exit in-line
//! 5. Based on the `VcpuEmulation` result, execution continues or stops
//!
//! # Supported VM Exits
//!
//! The minimal set of VM exits currently supported:
//! - **MMIO Read/Write**: Memory-mapped I/O operations
//! - **IO Port Read/Write**: x86 port I/O operations
//! - **HLT**: CPU halt instruction
//! - **Shutdown**: VM shutdown request
//!
//! # Example
//!
//! ```ignore
//! # use windows::Win32::System::Hypervisor::WHV_PARTITION_HANDLE;
//! # use vmm::windows::whpx_vcpu::{WhpxVcpu, VcpuExit};
//! # fn example(partition: WHV_PARTITION_HANDLE) -> std::io::Result<()> {
//! let mut vcpu = WhpxVcpu::new(partition, 0)?;
//! loop {
//!     let exit = vcpu.run()?;
//!     match exit {
//!         VcpuExit::Halted => break,
//!         _ => { /* handle exit */ }
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use utils::time::timestamp_cycles;
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};
use windows::core::HRESULT;
use windows::Win32::System::Hypervisor::{
    WHvCreateVirtualProcessor, WHvDeleteVirtualProcessor, WHvEmulatorCreateEmulator,
    WHvEmulatorDestroyEmulator, WHvEmulatorTryIoEmulation,
    WHvGetVirtualProcessorInterruptControllerState, WHvMemoryAccessExecute, WHvMemoryAccessRead,
    WHvMemoryAccessWrite, WHvRequestInterrupt, WHvRunVirtualProcessor, WHvRunVpExitReasonCanceled,
    WHvRunVpExitReasonException, WHvRunVpExitReasonHypercall,
    WHvRunVpExitReasonInvalidVpRegisterValue, WHvRunVpExitReasonMemoryAccess,
    WHvRunVpExitReasonSynicSintDeliverable, WHvRunVpExitReasonUnrecoverableException,
    WHvRunVpExitReasonUnsupportedFeature, WHvRunVpExitReasonX64ApicEoi,
    WHvRunVpExitReasonX64ApicInitSipiTrap, WHvRunVpExitReasonX64ApicSmiTrap,
    WHvRunVpExitReasonX64ApicWriteTrap, WHvRunVpExitReasonX64Cpuid, WHvRunVpExitReasonX64Halt,
    WHvRunVpExitReasonX64InterruptWindow, WHvRunVpExitReasonX64IoPortAccess,
    WHvRunVpExitReasonX64MsrAccess, WHvRunVpExitReasonX64Rdtsc, WHvTranslateGva,
    WHvX64ApicWriteTypeDfr, WHvX64ApicWriteTypeLdr, WHvX64ApicWriteTypeLint0,
    WHvX64ApicWriteTypeLint1, WHvX64ApicWriteTypeSvr, WHvX64ExceptionTypeBreakpointTrap,
    WHvX64ExceptionTypeOverflowTrap, WHvX64InterruptDestinationModeLogical,
    WHvX64InterruptDestinationModePhysical, WHvX64InterruptTriggerModeEdge,
    WHvX64InterruptTypeFixed, WHvX64InterruptTypeInit, WHvX64InterruptTypeLocalInt1,
    WHvX64InterruptTypeLowestPriority, WHvX64InterruptTypeNmi, WHvX64InterruptTypeSipi,
    WHvX64PendingEventExtInt, WHvX64RegisterRax, WHvX64RegisterRbx, WHvX64RegisterRcx,
    WHvX64RegisterRdx, WHvX64RegisterRip, WHV_EMULATOR_CALLBACKS, WHV_EMULATOR_IO_ACCESS_INFO,
    WHV_EMULATOR_MEMORY_ACCESS_INFO, WHV_INTERRUPT_CONTROL, WHV_PARTITION_HANDLE,
    WHV_REGISTER_NAME, WHV_REGISTER_VALUE, WHV_RUN_VP_EXIT_CONTEXT, WHV_TRANSLATE_GVA_FLAGS,
    WHV_TRANSLATE_GVA_RESULT, WHV_TRANSLATE_GVA_RESULT_CODE,
    WHV_X64_DELIVERABILITY_NOTIFICATIONS_REGISTER, WHV_X64_PENDING_EXT_INT_EVENT,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::interrupts::{PendingInterrupt, PendingInterruptQueue};
use super::registers::{
    get_virtual_processor_registers as WHvGetVirtualProcessorRegisters,
    set_virtual_processor_registers as WHvSetVirtualProcessorRegisters,
};

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

fn windows_io_debug_enabled() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WINDOWS_VERBOSE_DEBUG")
            .or_else(|_| std::env::var("LIBKRUN_WINDOWS_IO_DEBUG"))
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

macro_rules! eprintln {
    ($($arg:tt)*) => {{
        if windows_io_debug_enabled() {
            std::eprintln!($($arg)*);
        }
    }};
}
const HV_X64_MSR_GUEST_OS_ID: u32 = 0x4000_0000;
const HV_X64_MSR_HYPERCALL: u32 = 0x4000_0001;
const HV_X64_MSR_VP_INDEX: u32 = 0x4000_0002;
const HV_X64_MSR_VP_RUNTIME: u32 = 0x4000_0010;
const HV_X64_MSR_TIME_REF_COUNT: u32 = 0x4000_0020;
const HV_X64_MSR_TSC_FREQUENCY: u32 = 0x4000_0022;
const HV_X64_MSR_APIC_FREQUENCY: u32 = 0x4000_0023;
const HV_X64_MSR_VP_ASSIST_PAGE: u32 = 0x4000_0073;
const HV_FEATURE_VP_RUNTIME_AVAILABLE: u64 = 1 << 0;
const HV_FEATURE_TIME_REF_COUNT_AVAILABLE: u64 = 1 << 1;
const HV_FEATURE_HYPERCALL_MSRS_AVAILABLE: u64 = 1 << 5;
const HV_FEATURE_VP_INDEX_AVAILABLE: u64 = 1 << 6;
const HV_FEATURE_FREQUENCY_MSRS_AVAILABLE: u64 = 1 << 11;
const HV_MISC_FEATURE_FREQUENCY_MSRS_AVAILABLE: u64 = 1 << 8;
const HV_CPUID_FEATURES_MINIMAL_GUEST_MASK: u32 =
    (1 << 0) | (1 << 1) | (1 << 5) | (1 << 6) | (1 << 8) | (1 << 9) | (1 << 11);

const XAPIC_ID_OFFSET: usize = 0x020;
const XAPIC_TPR_OFFSET: usize = 0x080;
const XAPIC_PPR_OFFSET: usize = 0x0A0;
const XAPIC_EOI_OFFSET: usize = 0x0B0;
const XAPIC_LDR_OFFSET: usize = 0x0D0;
const XAPIC_SVR_OFFSET: usize = 0x0F0;
const XAPIC_ISR1_OFFSET: usize = 0x110;
const XAPIC_IRR1_OFFSET: usize = 0x210;
const XAPIC_LVT_LINT0_OFFSET: usize = 0x350;
const XAPIC_LVT_LINT1_OFFSET: usize = 0x360;
const XAPIC_LVT_TIMER_OFFSET: usize = 0x320;
const XAPIC_TIMER_INITIAL_COUNT_OFFSET: usize = 0x380;
const XAPIC_TIMER_CURRENT_COUNT_OFFSET: usize = 0x390;
const XAPIC_TIMER_DIVIDE_OFFSET: usize = 0x3E0;

fn windows_skip_pic_fixed_ack() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_SKIP_PIC_FIXED_ACK")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    })
}

fn windows_skip_pic_extint_ack() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_SKIP_PIC_EXTINT_ACK")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    })
}

fn windows_trace_apic_state() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_TRACE_APIC_STATE")
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    })
}

fn windows_pending_event_halted_reenter() -> bool {
    true
}

fn windows_arm_interrupt_window_on_if_cleared() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_ARM_INTERRUPT_WINDOW")
            .ok()
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    })
}

#[derive(Debug, Clone, Copy)]
enum PicFixedInjectionMode {
    RequestInterrupt,
    PendingInterruption,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PicFixedBuilderDirectMode {
    PendingInterruption,
    PendingEventExtInt,
}

fn windows_pic_fixed_injection_mode() -> PicFixedInjectionMode {
    static VALUE: std::sync::OnceLock<PicFixedInjectionMode> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_FIXED_INJECT")
            .ok()
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "pending-interruption" | "pending-interrupt" | "register" => {
                    PicFixedInjectionMode::PendingInterruption
                }
                _ => PicFixedInjectionMode::RequestInterrupt,
            })
            .unwrap_or(PicFixedInjectionMode::RequestInterrupt)
    })
}

fn windows_pic_fixed_builder_direct_mode() -> PicFixedBuilderDirectMode {
    static VALUE: std::sync::OnceLock<PicFixedBuilderDirectMode> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_FIXED_BUILDER_DIRECT_MODE")
            .ok()
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "pending-event" | "pending-event-extint" | "event" | "extint" => {
                    PicFixedBuilderDirectMode::PendingEventExtInt
                }
                _ => PicFixedBuilderDirectMode::PendingInterruption,
            })
            .unwrap_or(PicFixedBuilderDirectMode::PendingInterruption)
    })
}

#[derive(Debug, Clone, Copy)]
enum PicFixedTarget {
    PhysicalZero,
    PhysicalApicId,
    PhysicalOne,
    PhysicalBroadcast,
    LogicalLdr,
    LogicalBroadcast,
}

#[derive(Debug, Clone, Copy)]
enum PicFixedRequestInterruptType {
    Fixed,
    LowestPriority,
    Nmi,
    LocalInt1,
}

fn windows_pic_fixed_target() -> PicFixedTarget {
    static VALUE: std::sync::OnceLock<PicFixedTarget> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_FIXED_TARGET")
            .ok()
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "physical-apic-id" | "apic-id" => PicFixedTarget::PhysicalApicId,
                "physical-1" | "one" => PicFixedTarget::PhysicalOne,
                "physical-broadcast" | "broadcast" => PicFixedTarget::PhysicalBroadcast,
                "logical-ldr" | "ldr" | "logical" => PicFixedTarget::LogicalLdr,
                "logical-broadcast" => PicFixedTarget::LogicalBroadcast,
                _ => PicFixedTarget::PhysicalZero,
            })
            .unwrap_or(PicFixedTarget::PhysicalZero)
    })
}

fn windows_pic_fixed_request_interrupt_type() -> PicFixedRequestInterruptType {
    static VALUE: std::sync::OnceLock<PicFixedRequestInterruptType> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_FIXED_REQUEST_TYPE")
            .ok()
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "lowest-priority" | "lowest" | "lp" => PicFixedRequestInterruptType::LowestPriority,
                "nmi" => PicFixedRequestInterruptType::Nmi,
                "local-int1" | "lint1" | "localint1" => PicFixedRequestInterruptType::LocalInt1,
                _ => PicFixedRequestInterruptType::Fixed,
            })
            .unwrap_or(PicFixedRequestInterruptType::Fixed)
    })
}

fn windows_pic_fixed_vector_override() -> Option<u8> {
    static VALUE: std::sync::OnceLock<Option<u8>> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        let raw = std::env::var("LIBKRUN_WHPX_PIC_FIXED_VECTOR_OVERRIDE").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(hex) = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
        {
            u8::from_str_radix(hex, 16).ok()
        } else {
            trimmed.parse::<u8>().ok()
        }
    })
}

fn make_whpx_interrupt_control_bitfield(
    interrupt_type: windows::Win32::System::Hypervisor::WHV_INTERRUPT_TYPE,
    destination_mode: windows::Win32::System::Hypervisor::WHV_INTERRUPT_DESTINATION_MODE,
    trigger_mode: windows::Win32::System::Hypervisor::WHV_INTERRUPT_TRIGGER_MODE,
) -> u64 {
    (interrupt_type.0 as u64) | ((destination_mode.0 as u64) << 8) | ((trigger_mode.0 as u64) << 12)
}

fn read_u32_le(buf: &[u8], offset: usize) -> Option<u32> {
    let bytes = buf.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn windows_io_debug_log(message: impl AsRef<str>) {
    if !windows_io_debug_enabled() {
        return;
    }
    use std::io::Write;

    for path in [
        r"C:\Users\18770\.a3s\libkrun-whpx-io-current.log",
        "tmp_whpx_io.log",
    ] {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", message.as_ref());
        }
    }
}

fn measured_tsc_hz() -> u64 {
    static TSC_HZ: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *TSC_HZ.get_or_init(|| {
        let t0 = std::time::Instant::now();
        let c0 = unsafe { std::arch::x86_64::_rdtsc() };
        std::thread::sleep(std::time::Duration::from_millis(10));
        let c1 = unsafe { std::arch::x86_64::_rdtsc() };
        let elapsed = t0.elapsed().as_secs_f64();
        ((c1.wrapping_sub(c0)) as f64 / elapsed)
            .round()
            .clamp(100_000_000.0, 6_000_000_000.0) as u64
    })
}

fn performance_counter_100ns() -> u64 {
    let mut counter = 0i64;
    let mut frequency = 0i64;
    unsafe {
        QueryPerformanceCounter(&mut counter).ok().unwrap_or(());
        QueryPerformanceFrequency(&mut frequency).ok().unwrap_or(());
    }

    if counter <= 0 || frequency <= 0 {
        return 0;
    }

    ((counter as u128) * 10_000_000u128 / (frequency as u128)) as u64
}

fn host_hyperv_cpuid(function: u32) -> (u64, u64, u64, u64) {
    let result = unsafe { std::arch::x86_64::__cpuid_count(function, 0) };
    (
        result.eax as u64,
        result.ebx as u64,
        result.ecx as u64,
        result.edx as u64,
    )
}

fn guest_hyperv_cpuid(function: u32) -> (u64, u64, u64, u64) {
    let (rax, rbx, rcx, rdx) = host_hyperv_cpuid(function);

    match function {
        0x4000_0000 => (0x4000_0006, rbx, rcx, rdx),
        0x4000_0003 => (
            (rax as u32 & HV_CPUID_FEATURES_MINIMAL_GUEST_MASK) as u64,
            0,
            0,
            0,
        ),
        0x4000_0004 => (0x20, 0xff, 0, 0),
        0x4000_0005 | 0x4000_0006 => (0, 0, 0, 0),
        _ => (rax, rbx, rcx, rdx),
    }
}

fn zero_guest_page(guest_mem: &GuestMemoryMmap, gpa: u64) {
    let zero_page = [0u8; 4096];
    let _ = guest_mem.write_slice(&zero_page, GuestAddress(gpa));
}

fn windows_exit_debug_log(kind: &str, rip: u64, extra: impl AsRef<str>) {
    windows_io_debug_log(format!(
        "[EXIT] kind={} rip=0x{:x} {}",
        kind,
        rip,
        extra.as_ref()
    ));
}

fn should_log_mmio_gpa(gpa: u64) -> bool {
    matches!(
        gpa,
        0xd000_0000..=0xd0ff_ffff | 0xfec0_0000..=0xfee0_0fff
    )
}

fn should_always_log_io_port(port: u16) -> bool {
    matches!(
        port,
        0x20 | 0x21 | 0xa0 | 0xa1 | 0x40 | 0x41 | 0x42 | 0x43 | 0x61 | 0xcf8 | 0xcfc
    )
}

fn describe_apic_write_type(
    kind: windows::Win32::System::Hypervisor::WHV_X64_APIC_WRITE_TYPE,
) -> &'static str {
    match kind {
        kind if kind == WHvX64ApicWriteTypeLdr => "ldr",
        kind if kind == WHvX64ApicWriteTypeDfr => "dfr",
        kind if kind == WHvX64ApicWriteTypeSvr => "svr",
        kind if kind == WHvX64ApicWriteTypeLint0 => "lint0",
        kind if kind == WHvX64ApicWriteTypeLint1 => "lint1",
        _ => "other",
    }
}

/// Represents a VM exit from the WHPX virtual CPU.
///
/// The lifetime parameter `'a` ensures that borrowed data (MMIO/IO port buffers)
/// cannot outlive the exit context.
#[derive(Debug)]
#[cfg(target_os = "windows")]
pub enum VcpuExit<'a> {
    /// MMIO read operation.
    /// Contains the physical address and a mutable buffer to fill with data.
    /// The buffer size determines how many bytes to read (typically 1, 2, 4, or 8).
    MmioRead(u64, &'a mut [u8]),

    /// MMIO write operation.
    /// Contains the physical address and the data to write.
    /// The buffer size determines how many bytes to write (typically 1, 2, 4, or 8).
    MmioWrite(u64, &'a [u8]),

    /// IO port read operation.
    /// Contains the port number and a mutable buffer to fill with data.
    /// The buffer size determines how many bytes to read (typically 1, 2, or 4).
    IoPortRead(u16, &'a mut [u8]),

    /// IO port write operation.
    /// Contains the port number and the data to write.
    /// The buffer size determines how many bytes to write (typically 1, 2, or 4).
    IoPortWrite(u16, &'a [u8]),

    /// CPU executed HLT instruction.
    Halted,

    /// VM shutdown requested.
    Shutdown,
}

/// Result of emulating a VM exit.
///
/// Indicates how the VMM should proceed after handling a VM exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(target_os = "windows")]
pub enum VcpuEmulation {
    /// The exit was handled successfully, continue execution.
    Handled,

    /// The VM should stop execution.
    Stopped,

    /// The CPU is halted.
    Halted,
}

/// Represents a WHPX virtual CPU.
///
/// # Ownership
/// The `partition` handle must remain valid for the lifetime of this vCPU.
/// The caller is responsible for ensuring the partition is not destroyed
/// while this vCPU exists.
#[cfg(target_os = "windows")]
pub struct WhpxVcpu {
    /// Handle to the WHPX partition this vCPU belongs to.
    partition: WHV_PARTITION_HANDLE,
    /// Index of this vCPU within the partition.
    index: u32,
    /// WHPX software emulator handle for InstructionByteCount=0 exits.
    emulator: *mut c_void,
    /// Buffer for MMIO/IO port data transfer.
    data_buffer: [u8; 8],
    pending_io_read: Option<PendingIoRead>,
    pending_io_write: Option<PendingIoWrite>,
    pending_mmio_read: Option<PendingMmioRead>,
    pending_mmio_write: Option<PendingMmioWrite>,
    pending_interrupt: Option<PendingInterruptQueue>,
    guest_mem: *const GuestMemoryMmap,
    hyperv_guest_os_id: AtomicU64,
    hyperv_hypercall: AtomicU64,
    hyperv_vp_assist_page: AtomicU64,
}

// SAFETY: WhpxVcpu holds a raw emulator handle (*mut c_void) that is only
// accessed from the thread running WhpxVcpu::run(). WHV_PARTITION_HANDLE is
// an isize and safe to send across threads.
unsafe impl Send for WhpxVcpu {}

#[derive(Debug, Clone, Copy)]
struct PendingIoRead {
    port: u16,
    size: usize,
    next_rip: u64,
}

#[derive(Debug, Clone, Copy)]
struct PendingIoWrite {
    port: u16,
    next_rip: u64,
}

#[derive(Debug, Clone, Copy)]
struct PendingMmioRead {
    gpa: u64,
    size: usize,
    next_rip: u64,
    reg_index: u8,
    high8: bool,
    write_full: bool,
    sign_extend: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingMmioWrite {
    gpa: u64,
    next_rip: u64,
}

#[derive(Debug, Clone, Copy)]
enum MmioAccessKind {
    Noop,
    ReadReg { reg_index: u8, high8: bool },
    ReadRegZeroExtend { reg_index: u8 },
    ReadRegSignExtend { reg_index: u8 },
    WriteReg { reg_index: u8, high8: bool },
    WriteImm { value: u64 },
}

#[derive(Debug, Clone, Copy)]
struct DecodedMmioAccess {
    kind: MmioAccessKind,
    next_rip: u64,
    size: usize,
}

#[derive(Debug, Clone, Copy)]
struct ApicDestination {
    apic_id_raw: u32,
    ldr_raw: u32,
    destination_mode: u64,
    destination: u32,
    target: PicFixedTarget,
}

// ----------- WHPX hardware emulator (WHvEmulator) support --------------------
//
// When WHPX sets InstructionByteCount=0 on an IO port exit, the partition is in
// "software emulation mode".  In this mode WHvSetVirtualProcessorRegisters(RIP)
// is silently ignored and WHPX computes a corrupt next-RIP.
//
// WHvEmulatorTryIoEmulation is the correct remedy: it fetches the instruction
// bytes from guest memory via the TranslateGva + Memory callbacks, decodes the
// instruction, dispatches IO via the IoPort callback, and advances RIP through
// the SetRegisters callback (which WHPX does respect inside the emulator).

#[repr(C)]
struct EmulatorContext {
    partition: WHV_PARTITION_HANDLE,
    vp_index: u32,
    vcpu_id: u64,
    io_bus: *const devices::Bus,
    guest_mem: *const GuestMemoryMmap,
}

unsafe extern "system" fn emulator_io_port_cb(
    context: *const c_void,
    ioaccess: *mut WHV_EMULATOR_IO_ACCESS_INFO,
) -> HRESULT {
    let ctx = &*(context as *const EmulatorContext);
    let io = &mut *ioaccess;
    let port = io.Port;
    let size = (io.AccessSize as usize).min(4);
    let bus = &*ctx.io_bus;
    if io.Direction == 1 {
        // Write: data flows guest → device.
        let data_bytes = io.Data.to_le_bytes();
        bus.write(ctx.vcpu_id, port as u64, &data_bytes[..size]);
    } else {
        // Read: data flows device → guest.
        let mut buf = [0_u8; 4];
        bus.read(ctx.vcpu_id, port as u64, &mut buf[..size]);
        io.Data = u32::from_le_bytes(buf);
    }
    HRESULT(0) // S_OK — unregistered ports silently pass
}

unsafe extern "system" fn emulator_memory_cb(
    context: *const c_void,
    memoryaccess: *mut WHV_EMULATOR_MEMORY_ACCESS_INFO,
) -> HRESULT {
    let ctx = &*(context as *const EmulatorContext);
    let mem = &mut *memoryaccess;
    let size = mem.AccessSize as usize;
    let addr = GuestAddress(mem.GpaAddress);
    let guest_mem = &*ctx.guest_mem;
    if mem.Direction == 0 {
        if guest_mem.read_slice(&mut mem.Data[..size], addr).is_ok() {
            HRESULT(0)
        } else {
            HRESULT(0x80004005_u32 as i32) // E_FAIL
        }
    } else if guest_mem.write_slice(&mem.Data[..size], addr).is_ok() {
        HRESULT(0)
    } else {
        HRESULT(0x80004005_u32 as i32) // E_FAIL
    }
}

unsafe extern "system" fn emulator_get_registers_cb(
    context: *const c_void,
    registernames: *const WHV_REGISTER_NAME,
    registercount: u32,
    registervalues: *mut WHV_REGISTER_VALUE,
) -> HRESULT {
    let ctx = &*(context as *const EmulatorContext);
    match WHvGetVirtualProcessorRegisters(
        ctx.partition,
        ctx.vp_index,
        registernames,
        registercount,
        registervalues,
    ) {
        Ok(()) => HRESULT(0),
        Err(e) => e.code(),
    }
}

unsafe extern "system" fn emulator_set_registers_cb(
    context: *const c_void,
    registernames: *const WHV_REGISTER_NAME,
    registercount: u32,
    registervalues: *const WHV_REGISTER_VALUE,
) -> HRESULT {
    let ctx = &*(context as *const EmulatorContext);
    match WHvSetVirtualProcessorRegisters(
        ctx.partition,
        ctx.vp_index,
        registernames,
        registercount,
        registervalues,
    ) {
        Ok(()) => HRESULT(0),
        Err(e) => e.code(),
    }
}

unsafe extern "system" fn emulator_translate_gva_cb(
    context: *const c_void,
    gva: u64,
    translateflags: WHV_TRANSLATE_GVA_FLAGS,
    translationresult: *mut WHV_TRANSLATE_GVA_RESULT_CODE,
    gpa: *mut u64,
) -> HRESULT {
    let ctx = &*(context as *const EmulatorContext);
    let mut result: WHV_TRANSLATE_GVA_RESULT = std::mem::zeroed();
    match WHvTranslateGva(
        ctx.partition,
        ctx.vp_index,
        gva,
        translateflags,
        &mut result,
        gpa,
    ) {
        Ok(()) => {
            *translationresult = result.ResultCode;
            HRESULT(0)
        }
        Err(e) => e.code(),
    }
}

impl WhpxVcpu {
    fn guest_mem_ref(&self) -> Option<&GuestMemoryMmap> {
        if self.guest_mem.is_null() {
            None
        } else {
            Some(unsafe { &*self.guest_mem })
        }
    }

    fn arm_interrupt_window_notification(
        &self,
        rip: u64,
        source: &str,
        ptr: usize,
        depth_before: usize,
        pending: PendingInterrupt,
    ) -> io::Result<()> {
        use windows::Win32::System::Hypervisor::{
            WHvX64RegisterDeliverabilityNotifications, WHV_REGISTER_VALUE,
        };

        let vector = match pending {
            PendingInterrupt::PicExtInt { vector, .. }
            | PendingInterrupt::PicFixed { vector, .. } => vector,
        };
        let priority = u64::from(vector >> 4) & 0xf;
        let reg_name = [WHvX64RegisterDeliverabilityNotifications];
        let reg_value = [WHV_REGISTER_VALUE {
            DeliverabilityNotifications: WHV_X64_DELIVERABILITY_NOTIFICATIONS_REGISTER {
                AsUINT64: (1 << 1) | (priority << 2),
            },
        }];

        unsafe {
            WHvSetVirtualProcessorRegisters(
                self.partition,
                self.index,
                reg_name.as_ptr(),
                1,
                reg_value.as_ptr(),
            )
            .map_err(|e| {
                io::Error::other(format!(
                    "Failed to arm WHPX interrupt window at rip=0x{rip:x}: {e}"
                ))
            })?;
        }

        let mut readback = [WHV_REGISTER_VALUE::default(); 1];
        let readback_bits = unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                reg_name.as_ptr(),
                1,
                readback.as_mut_ptr(),
            )
            .ok()
            .map(|_| readback[0].DeliverabilityNotifications.AsUINT64)
            .unwrap_or(u64::MAX)
        };
        windows_exit_debug_log(
            "interrupt_window_arm",
            rip,
            format!(
                "source={} ptr=0x{:x} depth={} pending={:?} vector=0x{:02x} readback=0x{:016x}",
                source, ptr, depth_before, pending, vector, readback_bits
            ),
        );
        Ok(())
    }

    fn current_apic_destination(&self) -> Option<ApicDestination> {
        let mut apic_state = [0u8; 4096];
        let mut bytes_written = 0u32;
        unsafe {
            WHvGetVirtualProcessorInterruptControllerState(
                self.partition,
                self.index,
                apic_state.as_mut_ptr() as *mut c_void,
                apic_state.len() as u32,
                Some(&mut bytes_written as *mut u32),
            )
            .ok()?;
        }

        let apic_id_raw = read_u32_le(&apic_state, XAPIC_ID_OFFSET)?;
        let ldr_raw = read_u32_le(&apic_state, XAPIC_LDR_OFFSET)?;
        let apic_id = (apic_id_raw >> 24) & 0xff;
        let ldr = (ldr_raw >> 24) & 0xff;

        let target = windows_pic_fixed_target();
        let (destination_mode, destination) = match target {
            PicFixedTarget::PhysicalZero => (WHvX64InterruptDestinationModePhysical.0 as u64, 0),
            PicFixedTarget::PhysicalApicId => {
                (WHvX64InterruptDestinationModePhysical.0 as u64, apic_id)
            }
            PicFixedTarget::PhysicalOne => (WHvX64InterruptDestinationModePhysical.0 as u64, 1),
            PicFixedTarget::PhysicalBroadcast => {
                (WHvX64InterruptDestinationModePhysical.0 as u64, 0xff)
            }
            PicFixedTarget::LogicalLdr => (WHvX64InterruptDestinationModeLogical.0 as u64, ldr),
            PicFixedTarget::LogicalBroadcast => {
                (WHvX64InterruptDestinationModeLogical.0 as u64, 0xff)
            }
        };

        Some(ApicDestination {
            apic_id_raw,
            ldr_raw,
            destination_mode,
            destination,
            target,
        })
    }

    fn trace_interrupt_controller_state(&self, label: &str, rip: u64) {
        if !windows_trace_apic_state() {
            return;
        }

        let mut apic_state = [0u8; 4096];
        let mut bytes_written = 0u32;
        let result = unsafe {
            WHvGetVirtualProcessorInterruptControllerState(
                self.partition,
                self.index,
                apic_state.as_mut_ptr() as *mut c_void,
                apic_state.len() as u32,
                Some(&mut bytes_written as *mut u32),
            )
        };

        match result {
            Ok(()) => windows_exit_debug_log(
                "apic_state",
                rip,
                format!(
                    "label={} bytes={} apic_id=0x{:08x} tpr=0x{:08x} ppr=0x{:08x} eoi=0x{:08x} ldr=0x{:08x} svr=0x{:08x} isr1=0x{:08x} irr1=0x{:08x} lint0=0x{:08x} lint1=0x{:08x} lvt_timer=0x{:08x} timer_initial=0x{:08x} timer_current=0x{:08x} timer_divide=0x{:08x}",
                    label,
                    bytes_written,
                    read_u32_le(&apic_state, XAPIC_ID_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_TPR_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_PPR_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_EOI_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_LDR_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_SVR_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_ISR1_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_IRR1_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_LVT_LINT0_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_LVT_LINT1_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_LVT_TIMER_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_TIMER_INITIAL_COUNT_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_TIMER_CURRENT_COUNT_OFFSET).unwrap_or(u32::MAX),
                    read_u32_le(&apic_state, XAPIC_TIMER_DIVIDE_OFFSET).unwrap_or(u32::MAX),
                ),
            ),
            Err(e) => windows_exit_debug_log(
                "apic_state_failed",
                rip,
                format!("label={} hr=0x{:x}", label, e.code().0 as u32),
            ),
        }
    }

    fn pending_interrupt_depth(&self) -> usize {
        self.pending_interrupt
            .as_ref()
            .and_then(|queue| queue.lock().ok().map(|guard| guard.len()))
            .unwrap_or(0)
    }

    fn is_legacy_prefix(byte: u8) -> bool {
        matches!(
            byte,
            0x66 // operand-size override
                | 0x67 // address-size override
                | 0xF0 // lock
                | 0xF2 // repne/repnz
                | 0xF3 // rep/repe/repz
                | 0x2E // CS segment override
                | 0x36 // SS segment override
                | 0x3E // DS segment override
                | 0x26 // ES segment override
                | 0x64 // FS segment override
                | 0x65 // GS segment override
        )
    }

    fn advance_rip(&self, next_rip: u64) -> io::Result<()> {
        let names = [WHvX64RegisterRip];
        let values = unsafe {
            let mut v = [std::mem::zeroed::<WHV_REGISTER_VALUE>(); 1];
            v[0].Reg64 = next_rip;
            v
        };
        self.set_registers(&names, &values)?;
        self.service_pending_interrupt_after_completion("post-mmio-read")
    }

    /// Decodes the byte length of an x86 I/O port instruction from its raw bytes.
    ///
    /// WHPX on some Windows builds sets `InstructionByteCount = 0` for I/O port
    /// exits instead of the actual instruction length.  When that happens the
    /// caller must fall back to opcode-level decoding.
    ///
    /// Handles prefix bytes (REX 0x40–0x4F, legacy 0x66/0x67/0xF2/0xF3/0x26 …)
    /// followed by the I/O opcode:
    ///   * `E4`/`E5`/`E6`/`E7` (IN/OUT imm8) → opcode + 1-byte immediate = 2 bytes
    ///   * `EC`/`ED`/`EE`/`EF`/`6C`/`6D`/`6E`/`6F` (IN/OUT DX, INS/OUTS) → 1 byte
    fn decode_io_instr_len(instr_bytes: &[u8; 16]) -> u64 {
        let mut skip = 0usize;
        while skip < 15 {
            match instr_bytes[skip] {
                // Legacy prefixes: segment overrides, operand/address size, REP variants
                0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2
                | 0xF3
                // REX prefixes (64-bit mode)
                | 0x40..=0x4F => skip += 1,
                _ => break,
            }
        }
        let extra: usize = match instr_bytes[skip] {
            // IN/OUT with an immediate byte port operand (2-byte instruction)
            0xE4..=0xE7 => 2,
            // IN/OUT via DX, INS, OUTS (1-byte opcode after any prefixes)
            _ => 1,
        };
        (skip + extra) as u64
    }

    fn allow_string_io_fallback(port: u16) -> bool {
        // Legacy debug/console port ranges where dropping string I/O side effects
        // is acceptable during early boot and diagnostics.
        (0x3F8..=0x3FF).contains(&port) // COM1
            || (0x2F8..=0x2FF).contains(&port) // COM2
            || (0x3E8..=0x3EF).contains(&port) // COM3
            || (0x2E8..=0x2EF).contains(&port) // COM4
            || matches!(port, 0x80 | 0xE9 | 0x402)
    }

    fn gpr_name(index: u8) -> io::Result<WHV_REGISTER_NAME> {
        if index <= 15 {
            Ok(WHV_REGISTER_NAME(index as i32))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid x86_64 GPR index: {}", index),
            ))
        }
    }

    fn get_register_u64(&self, reg_index: u8) -> io::Result<u64> {
        let name = Self::gpr_name(reg_index)?;
        let mut value = [WHV_REGISTER_VALUE::default()];
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                &name,
                1,
                value.as_mut_ptr(),
            )
            .map_err(|e| {
                io::Error::other(format!("Failed to get vCPU register {}: {}", reg_index, e))
            })?;
            Ok(value[0].Reg64)
        }
    }

    fn reg_bits(value: u64, size: usize, high8: bool) -> io::Result<u64> {
        let out = match size {
            1 if high8 => (value >> 8) & 0xff,
            1 => value & 0xff,
            2 => value & 0xffff,
            4 => value & 0xffff_ffff,
            8 => value,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unsupported operand size {}", size),
                ));
            }
        };
        Ok(out)
    }

    fn merge_reg_bits(current: u64, size: usize, high8: bool, value: u64) -> io::Result<u64> {
        let merged = match size {
            1 if high8 => (current & !(0xff << 8)) | ((value & 0xff) << 8),
            1 => (current & !0xff) | (value & 0xff),
            2 => (current & !0xffff) | (value & 0xffff),
            4 => value & 0xffff_ffff,
            8 => value,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unsupported operand size {}", size),
                ));
            }
        };
        Ok(merged)
    }

    fn skip_modrm_address(bytes: &[u8], mut idx: usize, modrm: u8) -> io::Result<usize> {
        let mod_bits = (modrm >> 6) & 0x3;
        let rm = modrm & 0x7;

        if mod_bits == 0x3 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Register-only ModRM is not MMIO",
            ));
        }

        if rm == 0x4 {
            let sib = *bytes.get(idx).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "Malformed ModRM/SIB encoding")
            })?;
            idx += 1;
            let base = sib & 0x7;
            if mod_bits == 0x0 && base == 0x5 {
                idx = idx.checked_add(4).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Address decode overflow")
                })?;
            }
        }

        match mod_bits {
            0x0 if rm == 0x5 => {
                idx = idx.checked_add(4).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Address decode overflow")
                })?;
            }
            0x1 => {
                idx = idx.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Address decode overflow")
                })?;
            }
            0x2 => {
                idx = idx.checked_add(4).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Address decode overflow")
                })?;
            }
            _ => {}
        }

        Ok(idx)
    }

    fn operand_size_from_prefixes(rex: u8, operand_size_override: bool) -> usize {
        if (rex & 0x08) != 0 {
            8
        } else if operand_size_override {
            2
        } else {
            4
        }
    }

    fn decode_mmio_access(
        rip: u64,
        instruction_bytes: &[u8],
        is_write: bool,
    ) -> io::Result<DecodedMmioAccess> {
        let mut idx = 0;
        let mut rex: u8 = 0;
        let mut operand_size_override = false;

        while let Some(&b) = instruction_bytes.get(idx) {
            if Self::is_legacy_prefix(b) {
                if b == 0x66 {
                    operand_size_override = true;
                }
                idx += 1;
                continue;
            }
            if (0x40..=0x4f).contains(&b) {
                rex = b;
                idx += 1;
                continue;
            }
            break;
        }

        let opcode = *instruction_bytes.get(idx).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Missing opcode in MMIO instruction",
            )
        })?;
        idx += 1;

        if opcode == 0x0f {
            let opcode2 = *instruction_bytes.get(idx).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Missing second opcode byte in MMIO instruction",
                )
            })?;
            idx += 1;

            let modrm = *instruction_bytes.get(idx).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Missing ModRM in MMIO instruction",
                )
            })?;
            idx += 1;

            let reg_base = (modrm >> 3) & 0x7;
            let rex_r = (rex >> 2) & 1;
            let reg_index = reg_base + (rex_r << 3);
            let end_idx = Self::skip_modrm_address(instruction_bytes, idx, modrm)?;
            let next_rip =
                rip.wrapping_add(end_idx.try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len")
                })?);

            let (kind, size) = match opcode2 {
                // Prefetch variants: memory-touching hints with no architectural side effects.
                0x0d | 0x18 | 0x1f => (MmioAccessKind::Noop, 1),
                0xb6 if !is_write => (MmioAccessKind::ReadRegZeroExtend { reg_index }, 1),
                0xb7 if !is_write => (MmioAccessKind::ReadRegZeroExtend { reg_index }, 2),
                0xbe if !is_write => (MmioAccessKind::ReadRegSignExtend { reg_index }, 1),
                0xbf if !is_write => (MmioAccessKind::ReadRegSignExtend { reg_index }, 2),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "Unsupported MMIO instruction opcode 0x0f 0x{opcode2:02x} (is_write={is_write})"
                        ),
                    ));
                }
            };

            return Ok(DecodedMmioAccess {
                kind,
                next_rip,
                size,
            });
        }

        // moffs forms: mov AL/AX/EAX/RAX, moffs and mov moffs, AL/AX/EAX/RAX.
        if matches!(opcode, 0xa0..=0xa3) {
            let next_rip =
                rip.wrapping_add(instruction_bytes.len().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len")
                })?);

            let size = match opcode {
                0xa0 | 0xa2 => 1,
                0xa1 | 0xa3 => Self::operand_size_from_prefixes(rex, operand_size_override),
                _ => unreachable!(),
            };

            let kind = match opcode {
                0xa0 | 0xa1 if !is_write => MmioAccessKind::ReadReg {
                    reg_index: 0,
                    high8: false,
                },
                0xa2 | 0xa3 if is_write => MmioAccessKind::WriteReg {
                    reg_index: 0,
                    high8: false,
                },
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "Unsupported MMIO moffs opcode 0x{opcode:02x} (is_write={is_write})"
                        ),
                    ));
                }
            };

            return Ok(DecodedMmioAccess {
                kind,
                next_rip,
                size,
            });
        }

        // Reject unsupported opcodes before attempting to read the ModRM byte.
        // Opcodes not in this list have no ModRM and are not MMIO instructions we handle.
        if !matches!(opcode, 0x8a | 0x8b | 0x88 | 0x89 | 0x63 | 0xc6 | 0xc7) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Unsupported MMIO instruction opcode 0x{opcode:02x} (is_write={is_write})"),
            ));
        }

        let modrm = *instruction_bytes.get(idx).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Missing ModRM in MMIO instruction",
            )
        })?;
        idx += 1;

        let reg_base = (modrm >> 3) & 0x7;
        let rex_r = (rex >> 2) & 1;
        let reg_extended = reg_base + (rex_r << 3);

        let operand_size = match opcode {
            0x8a | 0x88 | 0xc6 => 1,
            0x63 => 4,
            0x8b | 0x89 | 0xc7 => Self::operand_size_from_prefixes(rex, operand_size_override),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "Unsupported MMIO instruction opcode 0x{opcode:02x} (is_write={is_write})"
                    ),
                ));
            }
        };

        let kind = match opcode {
            0x8a | 0x8b if !is_write => {
                let high8 = operand_size == 1 && rex == 0 && (4..=7).contains(&reg_base);
                let reg_index = if high8 { reg_base - 4 } else { reg_extended };
                MmioAccessKind::ReadReg { reg_index, high8 }
            }
            0x63 if !is_write => MmioAccessKind::ReadRegSignExtend {
                reg_index: reg_extended,
            },
            0x88 | 0x89 if is_write => {
                let high8 = operand_size == 1 && rex == 0 && (4..=7).contains(&reg_base);
                let reg_index = if high8 { reg_base - 4 } else { reg_extended };
                MmioAccessKind::WriteReg { reg_index, high8 }
            }
            0xc6 if is_write => {
                if reg_base != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Unsupported C6 ModRM extension",
                    ));
                }
                let imm_idx = Self::skip_modrm_address(instruction_bytes, idx, modrm)?;
                let imm = *instruction_bytes.get(imm_idx).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Missing imm8 in MMIO write")
                })? as u64;
                MmioAccessKind::WriteImm { value: imm }
            }
            0xc7 if is_write => {
                if reg_base != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Unsupported C7 ModRM extension",
                    ));
                }
                let imm_idx = Self::skip_modrm_address(instruction_bytes, idx, modrm)?;
                let imm_len = if operand_size == 2 { 2 } else { 4 };
                let imm_slice = instruction_bytes
                    .get(imm_idx..imm_idx + imm_len)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "Missing immediate in C7")
                    })?;
                let imm = if imm_len == 2 {
                    u16::from_le_bytes([imm_slice[0], imm_slice[1]]) as u64
                } else {
                    let raw = u32::from_le_bytes([
                        imm_slice[0],
                        imm_slice[1],
                        imm_slice[2],
                        imm_slice[3],
                    ]);
                    if operand_size == 8 {
                        (raw as i32 as i64) as u64
                    } else {
                        raw as u64
                    }
                };
                MmioAccessKind::WriteImm { value: imm }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "Unsupported MMIO instruction opcode 0x{opcode:02x} (is_write={is_write})"
                    ),
                ));
            }
        };

        let end_idx = match opcode {
            0xc6 => Self::skip_modrm_address(instruction_bytes, idx, modrm)?
                .checked_add(1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len"))?,
            0xc7 => Self::skip_modrm_address(instruction_bytes, idx, modrm)?
                .checked_add(if operand_size == 2 { 2 } else { 4 })
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len"))?,
            _ => Self::skip_modrm_address(instruction_bytes, idx, modrm)?,
        };
        let next_rip = rip.wrapping_add(
            end_idx
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len"))?,
        );

        Ok(DecodedMmioAccess {
            kind,
            next_rip,
            size: operand_size,
        })
    }

    fn set_registers(
        &self,
        names: &[WHV_REGISTER_NAME],
        values: &[WHV_REGISTER_VALUE],
    ) -> io::Result<()> {
        unsafe {
            WHvSetVirtualProcessorRegisters(
                self.partition,
                self.index,
                names.as_ptr(),
                names.len() as u32,
                values.as_ptr(),
            )
            .map_err(|e| io::Error::other(format!("Failed to set vCPU registers: {}", e)))
        }
    }

    fn log_invalid_vp_register_state(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) {
        use windows::Win32::System::Hypervisor::{
            WHvRegisterInterruptState, WHvRegisterPendingEvent, WHvRegisterPendingInterruption,
            WHvX64RegisterApicBase, WHvX64RegisterApicLvtLint0, WHvX64RegisterApicLvtLint1,
            WHvX64RegisterApicSpurious, WHvX64RegisterApicTpr, WHvX64RegisterCr8,
            WHvX64RegisterDeliverabilityNotifications, WHvX64RegisterRflags, WHvX64RegisterRip,
        };

        let core_reg_names = [
            WHvX64RegisterRip,
            WHvX64RegisterRflags,
            WHvRegisterInterruptState,
            WHvRegisterPendingInterruption,
            WHvRegisterPendingEvent,
            WHvX64RegisterDeliverabilityNotifications,
        ];
        let mut core_reg_values = [WHV_REGISTER_VALUE::default(); 6];

        unsafe {
            match WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                core_reg_names.as_ptr(),
                core_reg_names.len() as u32,
                core_reg_values.as_mut_ptr(),
            ) {
                Ok(()) => {
                    let pending_event = core_reg_values[4].ExtIntEvent.AsUINT128.Anonymous;
                    let execution_state = exit_context.VpContext.ExecutionState.AsUINT16;
                    let vp_bitfield = exit_context.VpContext._bitfield;
                    windows_exit_debug_log(
                        "invalid_vp_register",
                        exit_context.VpContext.Rip,
                        format!(
                            "vp_exec=0x{:04x} instr_len={} exit_cr8=0x{:x} exit_rflags=0x{:016x} rip=0x{:016x} rflags=0x{:016x} interrupt_state=0x{:016x} pending_interrupt=0x{:016x} pending_event_hi=0x{:016x} pending_event_lo=0x{:016x} deliverability=0x{:016x}",
                            execution_state,
                            vp_bitfield & 0x0f,
                            (vp_bitfield >> 4) & 0x0f,
                            exit_context.VpContext.Rflags,
                            core_reg_values[0].Reg64,
                            core_reg_values[1].Reg64,
                            core_reg_values[2].InterruptState.AsUINT64,
                            core_reg_values[3].PendingInterruption.AsUINT64,
                            pending_event.High64,
                            pending_event.Low64,
                            core_reg_values[5].DeliverabilityNotifications.AsUINT64,
                        ),
                    );
                }
                Err(e) => windows_exit_debug_log(
                    "invalid_vp_register",
                    exit_context.VpContext.Rip,
                    format!("core_state_read_failed hr=0x{:x}", e.code().0 as u32),
                ),
            }

            for (name, reg_name) in [
                ("apic_base", WHvX64RegisterApicBase),
                ("cr8", WHvX64RegisterCr8),
                ("apic_tpr", WHvX64RegisterApicTpr),
                ("lint0", WHvX64RegisterApicLvtLint0),
                ("lint1", WHvX64RegisterApicLvtLint1),
                ("apic_spurious", WHvX64RegisterApicSpurious),
            ] {
                let mut value = [WHV_REGISTER_VALUE::default(); 1];
                match WHvGetVirtualProcessorRegisters(
                    self.partition,
                    self.index,
                    [reg_name].as_ptr(),
                    1,
                    value.as_mut_ptr(),
                ) {
                    Ok(()) => windows_exit_debug_log(
                        "invalid_vp_register",
                        exit_context.VpContext.Rip,
                        format!("{}=0x{:016x}", name, value[0].Reg64),
                    ),
                    Err(e) => windows_exit_debug_log(
                        "invalid_vp_register",
                        exit_context.VpContext.Rip,
                        format!("{}_read_failed hr=0x{:x}", name, e.code().0 as u32),
                    ),
                }
            }
        }
    }

    fn inject_pending_interrupt(
        &self,
        rip: u64,
        can_inject_now: bool,
        source: &str,
    ) -> io::Result<bool> {
        use windows::Win32::System::Hypervisor::{
            WHvRegisterInternalActivityState, WHvRegisterInterruptState, WHvRegisterPendingEvent,
            WHvRegisterPendingInterruption, WHvX64RegisterDeliverabilityNotifications,
            WHV_REGISTER_VALUE,
        };

        let Some(queue) = self.pending_interrupt.as_ref() else {
            windows_exit_debug_log(
                "interrupt_probe",
                rip,
                format!("source={} queue=none can_inject={}", source, can_inject_now),
            );
            return Ok(false);
        };

        let ptr = Arc::as_ptr(queue) as usize;
        let (depth_before, front_before) = {
            let guard = queue.lock().unwrap();
            (guard.len(), guard.front().copied())
        };
        windows_exit_debug_log(
            "interrupt_probe",
            rip,
            format!(
                "source={} ptr=0x{:x} can_inject={} depth={} front={:?}",
                source, ptr, can_inject_now, depth_before, front_before
            ),
        );

        let allow_pending_pic_fixed_while_if_cleared =
            matches!(front_before, Some(PendingInterrupt::PicFixed { .. }))
                && matches!(
                    windows_pic_fixed_injection_mode(),
                    PicFixedInjectionMode::PendingInterruption
                );

        if !can_inject_now && !allow_pending_pic_fixed_while_if_cleared {
            if depth_before != 0 {
                let mut arm_attempted = false;
                let mut arm_result = "skipped";
                if windows_arm_interrupt_window_on_if_cleared() {
                    arm_attempted = true;
                    if let Some(pending) = front_before {
                        match self.arm_interrupt_window_notification(
                            rip,
                            source,
                            ptr,
                            depth_before,
                            pending,
                        ) {
                            Ok(()) => arm_result = "ok",
                            Err(e) => {
                                arm_result = "failed";
                                windows_exit_debug_log(
                                    "interrupt_window_arm_failed",
                                    rip,
                                    format!(
                                        "source={} ptr=0x{:x} depth={} front={:?} err={}",
                                        source, ptr, depth_before, front_before, e
                                    ),
                                );
                            }
                        }
                    } else {
                        arm_result = "no-front";
                    }
                }
                windows_exit_debug_log(
                    "interrupt_deferred",
                    rip,
                    format!(
                        "source={} reason=if-cleared ptr=0x{:x} depth={} front={:?} arm_attempted={} arm_result={}",
                        source,
                        ptr,
                        depth_before,
                        front_before,
                        arm_attempted,
                        arm_result
                    ),
                );
            }
            return Ok(false);
        }

        if allow_pending_pic_fixed_while_if_cleared {
            windows_exit_debug_log(
                "interrupt_if_override",
                rip,
                format!(
                    "source={} ptr=0x{:x} depth={} front={:?}",
                    source, ptr, depth_before, front_before
                ),
            );
        }

        // Builder-side PicFixed direct injection paths only queue when they were skipped
        // or failed, so a queued PicFixed must still be consumed here by the vCPU thread.

        let reg_names = [
            WHvRegisterPendingInterruption,
            WHvRegisterPendingEvent,
            WHvRegisterInterruptState,
            WHvX64RegisterDeliverabilityNotifications,
            WHvRegisterInternalActivityState,
        ];
        let mut reg_values = [WHV_REGISTER_VALUE::default(); 5];
        let reg_state = unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_mut_ptr(),
            )
        };
        let (
            pending_interrupt_reg,
            pending_event,
            interrupt_state,
            deliverability,
            internal_activity,
        ) = unsafe {
            (
                reg_values[0].PendingInterruption.AsUINT64,
                reg_values[1].ExtIntEvent.AsUINT128.Anonymous,
                reg_values[2].InterruptState.AsUINT64,
                reg_values[3].DeliverabilityNotifications.AsUINT64,
                reg_values[4].InternalActivity.AsUINT64,
            )
        };

        let pending_interrupt_busy = (pending_interrupt_reg & 1) != 0;
        let pending_event_busy = (pending_event.Low64 & 1) != 0;

        match reg_state {
            Ok(()) => {
                if pending_interrupt_busy || pending_event_busy {
                    windows_exit_debug_log(
                        "interrupt_deferred",
                        rip,
                        format!(
                            "source={} reason=slot-busy ptr=0x{:x} depth={} front={:?} pending_interrupt=0x{:016x} pending_interrupt_busy={} pending_event_hi=0x{:016x} pending_event_lo=0x{:016x} pending_event_busy={} interrupt_state=0x{:016x} deliverability=0x{:016x} internal_activity=0x{:016x}",
                            source,
                            ptr,
                            depth_before,
                            front_before,
                            pending_interrupt_reg,
                            pending_interrupt_busy,
                            pending_event.High64,
                            pending_event.Low64,
                            pending_event_busy,
                            interrupt_state,
                            deliverability,
                            internal_activity,
                        ),
                    );
                    return Ok(false);
                }
            }
            Err(e) => windows_exit_debug_log(
                "interrupt_probe_state_read_failed",
                rip,
                format!(
                    "source={} ptr=0x{:x} hr=0x{:x} depth={} front={:?}",
                    source,
                    ptr,
                    e.code().0 as u32,
                    depth_before,
                    front_before
                ),
            ),
        }

        let (pending, depth_after, front_after) = {
            let mut guard = queue.lock().unwrap();
            let pending = guard.pop_front();
            let depth_after = guard.len();
            let front_after = guard.front().copied();
            (pending, depth_after, front_after)
        };

        let Some(pending) = pending else {
            windows_exit_debug_log(
                "interrupt_empty_after_lock",
                rip,
                format!("source={} ptr=0x{:x}", source, ptr),
            );
            return Ok(false);
        };

        windows_exit_debug_log(
            "interrupt_pop",
            rip,
            format!(
                "source={} ptr=0x{:x} pending={:?} depth_after={} front_after={:?}",
                source, ptr, pending, depth_after, front_after
            ),
        );

        match pending {
            PendingInterrupt::PicExtInt { irq, vector } => {
                self.trace_interrupt_controller_state("pic-extint-before", rip);
                let mut ext_int_event = WHV_X64_PENDING_EXT_INT_EVENT::default();
                unsafe {
                    ext_int_event.Anonymous._bitfield =
                        1 | ((WHvX64PendingEventExtInt.0 as u64) << 1) | (u64::from(vector) << 8);
                    ext_int_event.Anonymous.Reserved2 = 0;
                }
                let reg_name = [WHvRegisterPendingEvent];
                let reg_value = [WHV_REGISTER_VALUE {
                    Reg128: unsafe { ext_int_event.AsUINT128 },
                }];

                let request_result = unsafe {
                    WHvSetVirtualProcessorRegisters(
                        self.partition,
                        self.index,
                        reg_name.as_ptr(),
                        1,
                        reg_value.as_ptr(),
                    )
                };

                match request_result {
                    Ok(()) => {
                        let skipped_ack = windows_skip_pic_extint_ack();
                        if !skipped_ack {
                            devices::legacy::windows_pic_stub::acknowledge_irq(irq);
                        }
                        self.trace_interrupt_controller_state("pic-extint-after", rip);
                        windows_exit_debug_log(
                            "interrupt_window_inject",
                            rip,
                            format!(
                                "source={} kind=pic-pending-event irq={} vector=0x{:02x} ptr=0x{:x} depth_after={} skipped_ack={}",
                                source,
                                irq,
                                vector,
                                ptr,
                                depth_after,
                                skipped_ack
                            ),
                        );
                        Ok(true)
                    }
                    Err(request_err) => {
                        {
                            let mut guard = queue.lock().unwrap();
                            guard.push_front(PendingInterrupt::PicExtInt { irq, vector });
                            windows_exit_debug_log(
                                "interrupt_requeue",
                                rip,
                                format!(
                                    "source={} ptr=0x{:x} irq={} vector=0x{:02x} depth={} front={:?}",
                                    source,
                                    ptr,
                                    irq,
                                    vector,
                                    guard.len(),
                                    guard.front().copied()
                                ),
                            );
                        }

                        let mut post_values = [WHV_REGISTER_VALUE::default(); 5];
                        let post_state = unsafe {
                            WHvGetVirtualProcessorRegisters(
                                self.partition,
                                self.index,
                                reg_names.as_ptr(),
                                reg_names.len() as u32,
                                post_values.as_mut_ptr(),
                            )
                        };
                        match post_state {
                            Ok(()) => {
                                let (
                                    post_pending_interrupt,
                                    post_pending_event,
                                    post_interrupt_state,
                                    post_deliverability,
                                    post_internal_activity,
                                ) = unsafe {
                                    (
                                        post_values[0].PendingInterruption.AsUINT64,
                                        post_values[1].ExtIntEvent.AsUINT128.Anonymous,
                                        post_values[2].InterruptState.AsUINT64,
                                        post_values[3].DeliverabilityNotifications.AsUINT64,
                                        post_values[4].InternalActivity.AsUINT64,
                                    )
                                };
                                windows_exit_debug_log(
                                    "interrupt_inject_failed",
                                    rip,
                                    format!(
                                        "source={} ptr=0x{:x} irq={} vector=0x{:02x} hr=0x{:x} pending_interrupt=0x{:016x} pending_interrupt_busy={} pending_event_hi=0x{:016x} pending_event_lo=0x{:016x} pending_event_busy={} interrupt_state=0x{:016x} deliverability=0x{:016x} internal_activity=0x{:016x}",
                                        source,
                                        ptr,
                                        irq,
                                        vector,
                                        request_err.code().0 as u32,
                                        post_pending_interrupt,
                                        (post_pending_interrupt & 1) != 0,
                                        post_pending_event.High64,
                                        post_pending_event.Low64,
                                        (post_pending_event.Low64 & 1) != 0,
                                        post_interrupt_state,
                                        post_deliverability,
                                        post_internal_activity,
                                    ),
                                );
                                if request_err.code().0 as u32 == 0xc0350005
                                    && (((post_pending_interrupt & 1) != 0)
                                        || ((post_pending_event.Low64 & 1) != 0))
                                {
                                    windows_exit_debug_log(
                                        "interrupt_deferred",
                                        rip,
                                        format!(
                                            "source={} reason=slot-busy-after-error ptr=0x{:x} irq={} vector=0x{:02x}",
                                            source, ptr, irq, vector
                                        ),
                                    );
                                    Ok(false)
                                } else {
                                    Err(io::Error::other(format!(
                                        "Failed to inject queued PIC ExtINT at rip=0x{rip:x}: hr=0x{:x}",
                                        request_err.code().0 as u32
                                    )))
                                }
                            }
                            Err(post_err) => Err(io::Error::other(format!(
                                "Failed to inject queued PIC ExtINT at rip=0x{rip:x}: hr=0x{:x} (state readback hr=0x{:x})",
                                request_err.code().0 as u32,
                                post_err.code().0 as u32
                            ))),
                        }
                    }
                }
            }
            PendingInterrupt::PicFixed { irq, vector } => {
                let injection_mode = windows_pic_fixed_injection_mode();
                let effective_vector = windows_pic_fixed_vector_override().unwrap_or(vector);
                let apic_destination = self.current_apic_destination();
                let (destination_mode, destination, apic_id_raw, ldr_raw, target) =
                    apic_destination
                        .map(|state| {
                            (
                                state.destination_mode,
                                state.destination,
                                state.apic_id_raw,
                                state.ldr_raw,
                                state.target,
                            )
                        })
                        .unwrap_or((
                            WHvX64InterruptDestinationModePhysical.0 as u64,
                            0,
                            u32::MAX,
                            u32::MAX,
                            PicFixedTarget::PhysicalZero,
                        ));
                self.trace_interrupt_controller_state("pic-fixed-before", rip);
                let request_interrupt_type = match windows_pic_fixed_request_interrupt_type() {
                    PicFixedRequestInterruptType::Fixed => WHvX64InterruptTypeFixed,
                    PicFixedRequestInterruptType::LowestPriority => {
                        WHvX64InterruptTypeLowestPriority
                    }
                    PicFixedRequestInterruptType::Nmi => WHvX64InterruptTypeNmi,
                    PicFixedRequestInterruptType::LocalInt1 => WHvX64InterruptTypeLocalInt1,
                };
                let interrupt = WHV_INTERRUPT_CONTROL {
                    _bitfield: make_whpx_interrupt_control_bitfield(
                        request_interrupt_type,
                        windows::Win32::System::Hypervisor::WHV_INTERRUPT_DESTINATION_MODE(
                            destination_mode as i32,
                        ),
                        WHvX64InterruptTriggerModeEdge,
                    ),
                    Destination: destination,
                    Vector: u32::from(effective_vector),
                };
                windows_exit_debug_log(
                    "pic_fixed_route",
                    rip,
                    format!(
                        "source={} target={:?} request_type={:?} apic_id_raw=0x{:08x} ldr_raw=0x{:08x} dest_mode=0x{:x} dest=0x{:x} vector=0x{:02x} effective_vector=0x{:02x} bitfield=0x{:016x}",
                        source,
                        target,
                        windows_pic_fixed_request_interrupt_type(),
                        apic_id_raw,
                        ldr_raw,
                        destination_mode,
                        destination,
                        vector,
                        effective_vector,
                        interrupt._bitfield,
                    ),
                );

                let request_result = unsafe {
                    match injection_mode {
                        PicFixedInjectionMode::RequestInterrupt => WHvRequestInterrupt(
                            self.partition,
                            &interrupt,
                            std::mem::size_of::<WHV_INTERRUPT_CONTROL>() as u32,
                        ),
                        PicFixedInjectionMode::PendingInterruption
                            if matches!(
                                windows_pic_fixed_builder_direct_mode(),
                                PicFixedBuilderDirectMode::PendingEventExtInt
                            ) =>
                        {
                            let mut ext_int_event = WHV_X64_PENDING_EXT_INT_EVENT::default();
                            ext_int_event.Anonymous._bitfield = 1
                                | ((WHvX64PendingEventExtInt.0 as u64) << 1)
                                | (u64::from(vector) << 8);
                            ext_int_event.Anonymous.Reserved2 = 0;
                            let reg_name = [WHvRegisterPendingEvent];
                            let reg_value = [WHV_REGISTER_VALUE {
                                Reg128: ext_int_event.AsUINT128,
                            }];
                            WHvSetVirtualProcessorRegisters(
                                self.partition,
                                self.index,
                                reg_name.as_ptr(),
                                1,
                                reg_value.as_ptr(),
                            )
                        }
                        PicFixedInjectionMode::PendingInterruption => {
                            let reg_name = [WHvRegisterPendingInterruption];
                            let mut reg_value = [WHV_REGISTER_VALUE::default(); 1];
                            reg_value[0].PendingInterruption.AsUINT64 =
                                1 | (u64::from(vector) << 16);
                            WHvSetVirtualProcessorRegisters(
                                self.partition,
                                self.index,
                                reg_name.as_ptr(),
                                1,
                                reg_value.as_ptr(),
                            )
                        }
                    }
                };

                match request_result {
                    Ok(()) => {
                        let mut post_pending = [WHV_REGISTER_VALUE::default(); 1];
                        let post_pending_state = unsafe {
                            WHvGetVirtualProcessorRegisters(
                                self.partition,
                                self.index,
                                [WHvRegisterPendingInterruption].as_ptr(),
                                1,
                                post_pending.as_mut_ptr(),
                            )
                        }
                        .ok()
                        .map(|_| unsafe { post_pending[0].PendingInterruption.AsUINT64 });
                        let skipped_ack = windows_skip_pic_fixed_ack();
                        if !skipped_ack {
                            devices::legacy::windows_pic_stub::acknowledge_irq(irq);
                        }
                        self.trace_interrupt_controller_state("pic-fixed-after", rip);
                        windows_exit_debug_log(
                            "interrupt_window_inject",
                            rip,
                            format!(
                                "source={} kind=pic-fixed irq={} vector=0x{:02x} effective_vector=0x{:02x} ptr=0x{:x} depth_after={} inject_mode={:?} request_type={:?} target={:?} dest_mode=0x{:x} dest=0x{:x} post_pending=0x{:016x} skipped_ack={}",
                                source,
                                irq,
                                vector,
                                effective_vector,
                                ptr,
                                depth_after,
                                injection_mode,
                                windows_pic_fixed_request_interrupt_type(),
                                target,
                                destination_mode,
                                interrupt.Destination,
                                post_pending_state.unwrap_or(u64::MAX),
                                skipped_ack
                            ),
                        );
                        Ok(true)
                    }
                    Err(request_err) => {
                        let mut guard = queue.lock().unwrap();
                        guard.push_front(PendingInterrupt::PicFixed { irq, vector });
                        windows_exit_debug_log(
                            "interrupt_requeue",
                            rip,
                            format!(
                                "source={} kind=pic-fixed ptr=0x{:x} irq={} vector=0x{:02x} effective_vector=0x{:02x} inject_mode={:?} request_type={:?} target={:?} dest_mode=0x{:x} dest=0x{:x} hr=0x{:x} depth={} front={:?}",
                                source,
                                ptr,
                                irq,
                                vector,
                                effective_vector,
                                injection_mode,
                                windows_pic_fixed_request_interrupt_type(),
                                target,
                                destination_mode,
                                interrupt.Destination,
                                request_err.code().0 as u32,
                                guard.len(),
                                guard.front().copied()
                            ),
                        );
                        Err(io::Error::other(format!(
                            "Failed to inject queued PIC fixed interrupt at rip=0x{rip:x}: hr=0x{:x}",
                            request_err.code().0 as u32
                        )))
                    }
                }
            }
        }
    }

    fn pending_event_busy_and_halted(&self) -> io::Result<bool> {
        use windows::Win32::System::Hypervisor::{
            WHvRegisterInternalActivityState, WHvRegisterPendingEvent, WHV_REGISTER_NAME,
            WHV_REGISTER_VALUE,
        };

        let reg_names: [WHV_REGISTER_NAME; 2] =
            [WHvRegisterPendingEvent, WHvRegisterInternalActivityState];
        let mut reg_values = [WHV_REGISTER_VALUE::default(); 2];
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_mut_ptr(),
            )
            .map_err(|e| {
                windows_exit_debug_log(
                    "pending_event_halted_probe_failed",
                    0,
                    format!("vcpu={} hr=0x{:x}", self.index, e.code().0 as u32),
                );
                io::Error::other(format!(
                    "Failed to read pending event / activity state for vCPU {}: {e}",
                    self.index
                ))
            })?;
        }

        let pending_event_busy =
            unsafe { (reg_values[0].ExtIntEvent.AsUINT128.Anonymous.Low64 & 1) != 0 };
        let internal_activity = unsafe { reg_values[1].InternalActivity.AsUINT64 };
        windows_exit_debug_log(
            "pending_event_halted_probe",
            0,
            format!(
                "vcpu={} pending_event_busy={} internal_activity=0x{:x}",
                self.index, pending_event_busy, internal_activity
            ),
        );
        Ok(pending_event_busy && (internal_activity & 0x2) != 0)
    }

    fn service_pending_interrupt_after_completion(&self, source: &str) -> io::Result<()> {
        use windows::Win32::System::Hypervisor::{
            WHvX64RegisterRflags, WHvX64RegisterRip, WHV_REGISTER_VALUE,
        };

        let Some(queue) = self.pending_interrupt.as_ref() else {
            return Ok(());
        };

        let queue_snapshot = {
            let guard = queue.lock().unwrap();
            if guard.is_empty() {
                return Ok(());
            }
            (guard.len(), guard.front().copied())
        };

        let log_post_completion =
            !source.starts_with("post-io") && !source.starts_with("post-mmio");

        if queue_snapshot.0 == 0 {
            return Ok(());
        }

        let reg_names = [WHvX64RegisterRip, WHvX64RegisterRflags];
        let mut reg_values = [WHV_REGISTER_VALUE::default(); 2];
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_mut_ptr(),
            )
            .map_err(|e| {
                io::Error::other(format!(
                    "Failed to read vCPU state for deferred interrupt delivery after {source}: {e}"
                ))
            })?;
        }

        let rip = unsafe { reg_values[0].Reg64 };
        let rflags = unsafe { reg_values[1].Reg64 };
        let can_inject_now = (rflags & (1 << 9)) != 0;

        if log_post_completion {
            windows_exit_debug_log(
                "interrupt_post_completion",
                rip,
                format!(
                    "source={} can_inject={} rflags=0x{:016x} depth={} front={:?}",
                    source, can_inject_now, rflags, queue_snapshot.0, queue_snapshot.1
                ),
            );
        }

        if !can_inject_now {
            return Ok(());
        }

        let _ = self.inject_pending_interrupt(rip, true, source)?;
        Ok(())
    }

    fn emulate_cpuid(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) -> io::Result<()> {
        let cpuid = unsafe { exit_context.Anonymous.CpuidAccess };
        let function = cpuid.Rax as u32;
        let next_rip = exit_context.VpContext.Rip.wrapping_add(2);

        // Hyper-V enlightenments: intercept hypervisor CPUID leaves (0x40000000-0x4000000F)
        // These tell the Linux kernel it's running under Hyper-V, enabling:
        //   - hyperv_clocksource (skips slow PIT-based TSC calibration)
        //   - Hyper-V synthetic timers
        //   - Other Hyper-V optimizations
        let (rax, rbx, rcx, rdx) = match function {
            // Leaf 0x0: maximum standard CPUID input value.
            // Bump to >= 0x15 so native_calibrate_tsc() queries leaf 0x15
            // (served from CpuidResultList2 with the measured TSC frequency).
            0x0 => {
                let hw_eax = cpuid.DefaultResultRax as u32;
                let eax = hw_eax.max(0x15) as u64;
                eprintln!("[CPUID 0x0] hw_max=0x{:x} → reporting 0x{:x}", hw_eax, eax);
                (
                    eax,
                    cpuid.DefaultResultRbx,
                    cpuid.DefaultResultRcx,
                    cpuid.DefaultResultRdx,
                )
            }
            // Leaf 0x1: when Hyper-V enlightenments are disabled, explicitly
            // clear the "hypervisor present" bit so Linux stays on the native
            // x86 path instead of probing an incomplete Hyper-V ABI.
            0x1 if !windows_hyperv_enlightenments_enabled() => (
                cpuid.DefaultResultRax,
                cpuid.DefaultResultRbx,
                cpuid.DefaultResultRcx & !(1u64 << 31),
                cpuid.DefaultResultRdx,
            ),
            // Leaf 0x40000000+: optional Hyper-V enlightenments.
            // Keep this disabled for now; exposing an incomplete Hyper-V surface
            // (vendor leaves without the full MSR/hypercall contract) sends the
            // guest into a partially paravirtualized path that regressed boot.
            0x40000000..=0x40000006 if !windows_hyperv_enlightenments_enabled() => {
                (0u64, 0u64, 0u64, 0u64)
            }
            0x40000000..=0x40000006 if windows_hyperv_enlightenments_enabled() => {
                guest_hyperv_cpuid(function)
            }
            // Leaf 0x15: Time Stamp Counter and Nominal Core Crystal Clock.
            // native_calibrate_tsc() reads this early in boot to get TSC khz.
            // If ECX=0 (hardware doesn't advertise crystal freq), calibration falls
            // through to PIT which fails in this VM. Intercept it and return a
            // measured frequency so the kernel can calibrate without PIT.
            // EAX=1 (denominator), EBX=1 (numerator), ECX=tsc_hz → TSC_khz=ECX/1000.
            0x15 => {
                let tsc_hz = measured_tsc_hz() as u32;
                eprintln!(
                    "[CPUID 0x15] TSC_HZ measured: {} Hz ({} MHz)",
                    tsc_hz,
                    tsc_hz / 1_000_000
                );
                (1u64, 1u64, tsc_hz as u64, 0u64) // EAX=denom=1, EBX=num=1, ECX=crystal_hz
            }
            // All other leaves: pass through WHPX default
            _ => (
                cpuid.DefaultResultRax,
                cpuid.DefaultResultRbx,
                cpuid.DefaultResultRcx,
                cpuid.DefaultResultRdx,
            ),
        };

        if windows_hyperv_enlightenments_enabled()
            && function >= 0x40000000
            && function <= 0x40000006
        {
            windows_io_debug_log(format!(
                "[CPUID] 0x{:08x}: eax=0x{:x} ebx=0x{:x} ecx=0x{:x} edx=0x{:x}",
                function, rax, rbx, rcx, rdx
            ));
            eprintln!(
                "[CPUID] 0x{:08x}: eax=0x{:x} ebx=0x{:x} ecx=0x{:x} edx=0x{:x}",
                function, rax, rbx, rcx, rdx
            );
        }

        let names = [
            WHvX64RegisterRax,
            WHvX64RegisterRbx,
            WHvX64RegisterRcx,
            WHvX64RegisterRdx,
            WHvX64RegisterRip,
        ];
        let values = [
            WHV_REGISTER_VALUE { Reg64: rax },
            WHV_REGISTER_VALUE { Reg64: rbx },
            WHV_REGISTER_VALUE { Reg64: rcx },
            WHV_REGISTER_VALUE { Reg64: rdx },
            WHV_REGISTER_VALUE { Reg64: next_rip },
        ];

        self.set_registers(&names, &values)
    }

    fn emulate_msr(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) -> io::Result<()> {
        let msr = unsafe { exit_context.Anonymous.MsrAccess };
        let is_write = unsafe { msr.AccessInfo.AsUINT32 } & 1 != 0;
        let next_rip = exit_context.VpContext.Rip.wrapping_add(2);

        if is_write {
            let write_value = (msr.Rdx << 32) | (msr.Rax & 0xffff_ffff);

            // Log writes to Hyper-V MSRs for debugging
            if msr.MsrNumber >= 0x40000000 {
                windows_io_debug_log(format!(
                    "[MSR-WR] 0x{:08x}=0x{:016x}",
                    msr.MsrNumber, write_value
                ));
                log::debug!("MSR WRMSR 0x{:08x} = 0x{:016x}", msr.MsrNumber, write_value);
            }

            match msr.MsrNumber {
                HV_X64_MSR_GUEST_OS_ID => {
                    self.hyperv_guest_os_id
                        .store(write_value, Ordering::Relaxed);
                }
                HV_X64_MSR_HYPERCALL => {
                    self.hyperv_hypercall.store(write_value, Ordering::Relaxed);
                }
                HV_X64_MSR_VP_ASSIST_PAGE => {
                    self.hyperv_vp_assist_page
                        .store(write_value, Ordering::Relaxed);
                    if write_value & 1 != 0 {
                        if let Some(guest_mem) = self.guest_mem_ref() {
                            let gpa = write_value & !0xfff;
                            zero_guest_page(guest_mem, gpa);
                            windows_io_debug_log(format!(
                                "[MSR-WR] vp_assist_page_zeroed gpa=0x{:016x}",
                                gpa
                            ));
                        }
                    }
                }
                _ => {}
            }

            let names = [WHvX64RegisterRip];
            let values = [WHV_REGISTER_VALUE { Reg64: next_rip }];
            self.set_registers(&names, &values)
        } else {
            let read_value: u64 = match msr.MsrNumber {
                // IA32_TSC (0x10): return a monotonic host value.
                0x10 => timestamp_cycles(),
                HV_X64_MSR_GUEST_OS_ID => self.hyperv_guest_os_id.load(Ordering::Relaxed),
                HV_X64_MSR_HYPERCALL => self.hyperv_hypercall.load(Ordering::Relaxed),
                HV_X64_MSR_VP_ASSIST_PAGE => self.hyperv_vp_assist_page.load(Ordering::Relaxed),
                HV_X64_MSR_VP_INDEX => self.index as u64,
                // HV_X64_MSR_TIME_REF_COUNT (0x40000020):
                // 100ns-resolution reference time counter (Hyper-V TLFS).
                // Convert TSC cycles to 100ns units using a rough estimate.
                // This allows the kernel's hyperv_clocksource to function.
                HV_X64_MSR_TIME_REF_COUNT => {
                    // 100ns intervals since boot. Use host timestamp / ~10MHz estimate.
                    // Actual TSC freq varies; kernel will calibrate against this.
                    let tsc = performance_counter_100ns();
                    // Assume ~3GHz TSC: 3e9 cycles/s = 3e7 100ns/s
                    // → divide by 30 to get rough 100ns count
                    tsc
                }
                // HV_X64_MSR_VP_RUNTIME (0x40000010): vCPU runtime in 100ns.
                HV_X64_MSR_VP_RUNTIME => performance_counter_100ns(),
                // HV_X64_MSR_TSC_FREQUENCY (0x40000022):
                // TSC frequency in Hz. With HV_ACCESS_FREQUENCY_MSRS (CPUID 0x40000003 bit 8),
                // the kernel reads this MSR to calibrate TSC directly, skipping PIT.
                // QueryPerformanceFrequency returns the actual TSC frequency on modern Windows.
                HV_X64_MSR_TSC_FREQUENCY => {
                    let result = measured_tsc_hz();
                    eprintln!(
                        "[MSR] 0x40000022 (TSC_FREQUENCY) → {} Hz ({} MHz)",
                        result,
                        result / 1_000_000
                    );
                    windows_io_debug_log(format!("[MSR-RD] 0x40000022=0x{:016x}", result));
                    result
                }
                HV_X64_MSR_APIC_FREQUENCY => {
                    let result = 100_000_000u64;
                    windows_io_debug_log(format!("[MSR-RD] 0x40000023=0x{:016x}", result));
                    result
                }
                // Default to zero for currently unsupported MSRs.
                _ => {
                    if msr.MsrNumber >= 0x40000000 {
                        windows_io_debug_log(format!(
                            "[MSR-RD] 0x{:08x}=0x0000000000000000",
                            msr.MsrNumber
                        ));
                        log::debug!(
                            "MSR RDMSR 0x{:08x} → 0 (unsupported Hyper-V MSR)",
                            msr.MsrNumber
                        );
                    }
                    0
                }
            };
            let names = [WHvX64RegisterRax, WHvX64RegisterRdx, WHvX64RegisterRip];
            let values = [
                WHV_REGISTER_VALUE {
                    Reg64: read_value & 0xffff_ffff,
                },
                WHV_REGISTER_VALUE {
                    Reg64: read_value >> 32,
                },
                WHV_REGISTER_VALUE { Reg64: next_rip },
            ];
            self.set_registers(&names, &values)
        }
    }

    fn emulate_rdtsc(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) -> io::Result<()> {
        let tsc = timestamp_cycles();
        let next_rip = exit_context.VpContext.Rip.wrapping_add(2);

        let names = [WHvX64RegisterRax, WHvX64RegisterRdx, WHvX64RegisterRip];
        let values = [
            WHV_REGISTER_VALUE {
                Reg64: tsc & 0xffff_ffff,
            },
            WHV_REGISTER_VALUE { Reg64: tsc >> 32 },
            WHV_REGISTER_VALUE { Reg64: next_rip },
        ];
        self.set_registers(&names, &values)?;
        self.service_pending_interrupt_after_completion("post-io-read")
    }

    fn emulate_exception(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) -> io::Result<bool> {
        let vp_exception = unsafe { exit_context.Anonymous.VpException };
        let exception_type = vp_exception.ExceptionType as i32;

        if exception_type == WHvX64ExceptionTypeBreakpointTrap.0
            || exception_type == WHvX64ExceptionTypeOverflowTrap.0
        {
            let next_rip = exit_context
                .VpContext
                .Rip
                .wrapping_add(vp_exception.InstructionByteCount as u64);
            self.advance_rip(next_rip)?;
            return Ok(true);
        }

        Ok(false)
    }

    /// Creates a new WHPX virtual CPU.
    ///
    /// # Arguments
    /// * `partition` - Handle to the WHPX partition
    /// * `index` - Index of the vCPU to create
    ///
    /// # Errors
    /// Returns an error if vCPU creation fails.
    pub fn new(
        partition: WHV_PARTITION_HANDLE,
        index: u32,
        pending_interrupt: Option<PendingInterruptQueue>,
    ) -> io::Result<Self> {
        // SAFETY: We assume the caller has provided a valid partition handle.
        // The partition must remain valid for the lifetime of this vCPU (documented in struct).
        // The third parameter (0) represents flags, with 0 meaning default behavior.
        unsafe {
            WHvCreateVirtualProcessor(partition, index, 0 /* flags: default behavior */)
                .map_err(|e| io::Error::other(format!("Failed to create vCPU: {}", e)))?;
        }

        // Create the WHPX software emulator used to handle IO exits where
        // InstructionByteCount=0 (software-emulation mode).
        let callbacks = WHV_EMULATOR_CALLBACKS {
            Size: std::mem::size_of::<WHV_EMULATOR_CALLBACKS>() as u32,
            Reserved: 0,
            WHvEmulatorIoPortCallback: Some(emulator_io_port_cb),
            WHvEmulatorMemoryCallback: Some(emulator_memory_cb),
            WHvEmulatorGetVirtualProcessorRegisters: Some(emulator_get_registers_cb),
            WHvEmulatorSetVirtualProcessorRegisters: Some(emulator_set_registers_cb),
            WHvEmulatorTranslateGvaPage: Some(emulator_translate_gva_cb),
        };
        let mut emulator: *mut c_void = std::ptr::null_mut();
        unsafe {
            WHvEmulatorCreateEmulator(&callbacks, &mut emulator)
                .map_err(|e| io::Error::other(format!("Failed to create WHPX emulator: {e}")))?;
        }

        Ok(Self {
            partition,
            index,
            emulator,
            data_buffer: [0; 8],
            pending_io_read: None,
            pending_io_write: None,
            pending_mmio_read: None,
            pending_mmio_write: None,
            pending_interrupt,
            guest_mem: std::ptr::null(),
            hyperv_guest_os_id: AtomicU64::new(0),
            hyperv_hypercall: AtomicU64::new(0),
            hyperv_vp_assist_page: AtomicU64::new(0),
        })
    }

    pub fn complete_mmio_read(&mut self, data: &[u8]) -> io::Result<()> {
        let pending = self.pending_mmio_read.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "No pending WHPX MMIO read exit",
            )
        })?;

        if data.len() < pending.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "MMIO read buffer too small: have {}, need {}",
                    data.len(),
                    pending.size
                ),
            ));
        }

        let mut value = 0_u64;
        for (idx, byte) in data.iter().take(pending.size).enumerate() {
            value |= (*byte as u64) << (idx * 8);
        }

        if should_log_mmio_gpa(pending.gpa) {
            windows_io_debug_log(format!(
                "[MMIOREAD-COMPLETE] gpa=0x{:08x} size={} reg={} high8={} write_full={} sign_extend={} data={:02x?} value=0x{:x} next_rip=0x{:x}",
                pending.gpa,
                pending.size,
                pending.reg_index,
                pending.high8,
                pending.write_full,
                pending.sign_extend,
                &data[..pending.size],
                value,
                pending.next_rip
            ));
        }

        let merged = if pending.write_full {
            if pending.sign_extend {
                match pending.size {
                    1 => (value as u8 as i8 as i64) as u64,
                    2 => (value as u16 as i16 as i64) as u64,
                    4 => (value as u32 as i32 as i64) as u64,
                    8 => value,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("Unsupported MMIO sign-extend size {}", pending.size),
                        ));
                    }
                }
            } else {
                value
            }
        } else {
            let current = self.get_register_u64(pending.reg_index)?;
            Self::merge_reg_bits(current, pending.size, pending.high8, value)?
        };

        if should_log_mmio_gpa(pending.gpa) {
            windows_io_debug_log(format!(
                "[MMIOREAD-WRITEBACK] gpa=0x{:08x} reg={} merged=0x{:x} next_rip=0x{:x}",
                pending.gpa, pending.reg_index, merged, pending.next_rip
            ));
        }

        let names = [Self::gpr_name(pending.reg_index)?, WHvX64RegisterRip];
        let values = unsafe {
            let mut v = [std::mem::zeroed::<WHV_REGISTER_VALUE>(); 2];
            v[0].Reg64 = merged;
            v[1].Reg64 = pending.next_rip;
            v
        };
        self.set_registers(&names, &values)?;
        self.service_pending_interrupt_after_completion("post-mmio-read")
    }

    pub fn complete_mmio_write(&mut self) -> io::Result<()> {
        let pending = self.pending_mmio_write.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "No pending WHPX MMIO write exit",
            )
        })?;

        if should_log_mmio_gpa(pending.gpa) {
            windows_io_debug_log(format!(
                "[MMIOWRITE-COMPLETE] gpa=0x{:08x} next_rip=0x{:x}",
                pending.gpa, pending.next_rip
            ));
        }

        let names = [WHvX64RegisterRip];
        let values = unsafe {
            let mut v = [std::mem::zeroed::<WHV_REGISTER_VALUE>(); 1];
            v[0].Reg64 = pending.next_rip;
            v
        };
        self.set_registers(&names, &values)?;
        self.service_pending_interrupt_after_completion("post-mmio-write")
    }

    pub fn complete_io_read(&mut self, data: &[u8]) -> io::Result<()> {
        let pending = self.pending_io_read.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "No pending WHPX I/O read exit")
        })?;

        if data.len() < pending.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "I/O read buffer too small: have {}, need {}",
                    data.len(),
                    pending.size
                ),
            ));
        }

        let mut value = 0_u64;
        for (idx, byte) in data.iter().take(pending.size).enumerate() {
            value |= (*byte as u64) << (idx * 8);
        }

        if should_always_log_io_port(pending.port) {
            windows_io_debug_log(format!(
                "[IOREAD] port=0x{:04x} size={} data={:02x?}",
                pending.port,
                pending.size,
                &data[..pending.size]
            ));
        }

        let current_rax = self.get_register_u64(0)?;
        let merged_rax = Self::merge_reg_bits(current_rax, pending.size, false, value)?;

        let names = [WHvX64RegisterRax, WHvX64RegisterRip];
        let values = unsafe {
            let mut v = [std::mem::zeroed::<WHV_REGISTER_VALUE>(); 2];
            v[0].Reg64 = merged_rax;
            v[1].Reg64 = pending.next_rip;
            v
        };
        self.set_registers(&names, &values)?;
        self.service_pending_interrupt_after_completion("post-io-read")
    }

    pub fn complete_io_write(&mut self) -> io::Result<()> {
        let pending = self.pending_io_write.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "No pending WHPX I/O write exit",
            )
        })?;

        if should_always_log_io_port(pending.port) {
            windows_io_debug_log(format!("[IOWRITE-COMPLETE] port=0x{:04x}", pending.port));
        }

        let names = [WHvX64RegisterRip];
        let values = unsafe {
            let mut v = [std::mem::zeroed::<WHV_REGISTER_VALUE>(); 1];
            v[0].Reg64 = pending.next_rip;
            v
        };
        self.set_registers(&names, &values)?;
        self.service_pending_interrupt_after_completion("post-io-write")
    }

    pub fn clear_pending_io(&mut self) {
        self.pending_io_read = None;
        self.pending_io_write = None;
    }

    pub fn clear_pending_mmio(&mut self) {
        self.pending_mmio_read = None;
        self.pending_mmio_write = None;
    }

    fn emulate_apic_init_sipi(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) -> io::Result<()> {
        let apic = unsafe { exit_context.Anonymous.ApicInitSipi };
        let icr = apic.ApicIcr;
        let vector = (icr & 0xff) as u32;
        let delivery_mode = ((icr >> 8) & 0x7) as u8;
        let destination_mode = if ((icr >> 11) & 0x1) != 0 {
            WHvX64InterruptDestinationModeLogical
        } else {
            WHvX64InterruptDestinationModePhysical
        };
        let destination = ((icr >> 56) & 0xff) as u32;

        let interrupt_type = match delivery_mode {
            0b101 => WHvX64InterruptTypeInit,
            0b110 => WHvX64InterruptTypeSipi,
            other => {
                windows_exit_debug_log(
                    "apic_init_sipi_unknown",
                    exit_context.VpContext.Rip,
                    format!("icr=0x{icr:016x} delivery_mode={other}"),
                );
                return Ok(());
            }
        };

        windows_exit_debug_log(
            "apic_init_sipi",
            exit_context.VpContext.Rip,
            format!(
                "icr=0x{icr:016x} delivery_mode={} dest_mode={} dest=0x{:x} vector=0x{:x}",
                delivery_mode, destination_mode.0, destination, vector
            ),
        );

        let interrupt = WHV_INTERRUPT_CONTROL {
            _bitfield: make_whpx_interrupt_control_bitfield(
                interrupt_type,
                destination_mode,
                WHvX64InterruptTriggerModeEdge,
            ),
            Destination: destination,
            Vector: vector,
        };

        unsafe {
            WHvRequestInterrupt(
                self.partition,
                &interrupt,
                std::mem::size_of::<WHV_INTERRUPT_CONTROL>() as u32,
            )
            .map_err(|e| {
                io::Error::other(format!(
                    "Failed to emulate APIC INIT/SIPI icr=0x{icr:016x} vector=0x{vector:x} dest=0x{destination:x}: {e}"
                ))
            })?;
        }

        Ok(())
    }

    /// Gets the current RIP (instruction pointer) value.
    pub fn get_rip(&self) -> io::Result<u64> {
        let names = [WHvX64RegisterRip];
        let mut values = [WHV_REGISTER_VALUE::default()];

        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                self.index,
                names.as_ptr(),
                names.len() as u32,
                values.as_mut_ptr(),
            )
            .map_err(|e| io::Error::other(format!("Failed to get RIP: {}", e)))?;
        }

        Ok(unsafe { values[0].Reg64 })
    }

    /// Runs the virtual CPU until a VM exit occurs.
    ///
    /// # Returns
    /// Returns a `VcpuExit` describing why the vCPU stopped executing.
    ///
    /// # Errors
    /// Returns an error if running the vCPU fails.
    pub fn run(
        &mut self,
        io_bus: *const devices::Bus,
        guest_mem: *const GuestMemoryMmap,
        vcpu_id: u64,
    ) -> io::Result<VcpuExit<'_>> {
        static mut RUN_COUNT: u64 = 0;
        static RUN_SEQ: AtomicU64 = AtomicU64::new(0);
        self.guest_mem = guest_mem;
        loop {
            let run_seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
            let queue_snapshot = self.pending_interrupt.as_ref().map(|queue| {
                let guard = queue.lock().unwrap();
                (
                    Arc::as_ptr(queue) as usize,
                    guard.len(),
                    guard.front().copied(),
                )
            });
            if let Some((ptr, depth, front)) = queue_snapshot {
                if depth != 0 {
                    windows_exit_debug_log(
                        "run_enter",
                        0,
                        format!(
                            "seq={} ptr=0x{:x} depth={} front={:?}",
                            run_seq, ptr, depth, front
                        ),
                    );
                }
            }
            let mut exit_context = WHV_RUN_VP_EXIT_CONTEXT::default();

            // SAFETY: WHvRunVirtualProcessor is safe to call with valid partition and vCPU handles.
            // The exit_context is a valid mutable reference that will be filled by the API.
            unsafe {
                RUN_COUNT += 1;
                if RUN_COUNT % 1000 == 0 {
                    log::debug!("WHvRunVirtualProcessor called {} times", RUN_COUNT);
                }

                WHvRunVirtualProcessor(
                    self.partition,
                    self.index,
                    (&mut exit_context as *mut WHV_RUN_VP_EXIT_CONTEXT).cast(),
                    std::mem::size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32,
                )
                .map_err(|e| io::Error::other(format!("Failed to run vCPU: {}", e)))?;
            }

            if let Some((ptr, depth, front)) = queue_snapshot {
                if depth != 0 {
                    windows_exit_debug_log(
                        "run_exit",
                        exit_context.VpContext.Rip,
                        format!(
                            "seq={} reason={:?} ptr=0x{:x} depth_before={} front_before={:?}",
                            run_seq, exit_context.ExitReason, ptr, depth, front
                        ),
                    );
                }
            }

            // Log exit reason for debugging
            log::trace!(
                "WHPX exit: reason={:?}, RIP={:#x}",
                exit_context.ExitReason,
                exit_context.VpContext.Rip
            );

            // Debug: track exit reason counts
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static TOTAL: AtomicU64 = AtomicU64::new(0);
                static CPUID_EXITS: AtomicU64 = AtomicU64::new(0);
                static MSR_EXITS: AtomicU64 = AtomicU64::new(0);
                static IO_EXITS: AtomicU64 = AtomicU64::new(0);
                let t = TOTAL.fetch_add(1, Ordering::Relaxed);
                if exit_context.ExitReason == WHvRunVpExitReasonX64Cpuid {
                    let c = CPUID_EXITS.fetch_add(1, Ordering::Relaxed);
                    if c < 10 {
                        eprintln!(
                            "[EXIT] CPUID exit #{} at RIP=0x{:x}",
                            c, exit_context.VpContext.Rip
                        );
                    }
                }
                if exit_context.ExitReason == WHvRunVpExitReasonX64MsrAccess {
                    MSR_EXITS.fetch_add(1, Ordering::Relaxed);
                }
                if exit_context.ExitReason == WHvRunVpExitReasonX64IoPortAccess {
                    IO_EXITS.fetch_add(1, Ordering::Relaxed);
                }
                if t == 10000 {
                    eprintln!(
                        "[EXIT] Stats after 10000 exits: cpuid={} msr={} io={}",
                        CPUID_EXITS.load(Ordering::Relaxed),
                        MSR_EXITS.load(Ordering::Relaxed),
                        IO_EXITS.load(Ordering::Relaxed)
                    );
                }
            }

            // Parse the exit reason.
            match exit_context.ExitReason {
                reason if reason == WHvRunVpExitReasonMemoryAccess => {
                    let memory_access = unsafe { exit_context.Anonymous.MemoryAccess };
                    let gpa = memory_access.Gpa;
                    let access_info = unsafe { memory_access.AccessInfo.AsUINT32 };
                    let access_type = (access_info & 0x3) as i32;
                    let access_size = 0usize;

                    // Track all MemoryAccess exits to understand kernel behavior
                    let rip = exit_context.VpContext.Rip;
                    let access_type_str = match access_type {
                        0 => "Read",
                        1 => "Write",
                        2 => "Execute",
                        _ => "Unknown",
                    };

                    static mut TOTAL_EXITS: u64 = 0;
                    static mut LAST_RIP: u64 = 0;
                    static mut SAME_RIP_COUNT: u64 = 0;

                    unsafe {
                        TOTAL_EXITS += 1;

                        // Track if we're stuck at the same RIP
                        if rip == LAST_RIP {
                            SAME_RIP_COUNT += 1;
                            // Log if stuck at same address for 100+ exits
                            if SAME_RIP_COUNT == 100 {
                                debug!(
                                    "STUCK: RIP={:#x} repeated 100 times, GPA={:#x}, Type={}, Size={}",
                                    rip, gpa, access_type_str, access_size
                                );
                            } else if SAME_RIP_COUNT % 1000 == 0 {
                                debug!("STUCK: RIP={:#x} repeated {} times", rip, SAME_RIP_COUNT);
                            }
                        } else {
                            // RIP changed, log if previous was stuck
                            if SAME_RIP_COUNT >= 100 {
                                debug!(
                                    "Unstuck from RIP={:#x} after {} exits, now at {:#x}",
                                    LAST_RIP, SAME_RIP_COUNT, rip
                                );
                            }
                            LAST_RIP = rip;
                            SAME_RIP_COUNT = 1;
                        }

                        // Log first 20 exits and every 1000th exit for overview
                        if TOTAL_EXITS <= 20 || TOTAL_EXITS % 1000 == 0 {
                            debug!(
                                "Exit #{}: RIP={:#x}, GPA={:#x}, Type={}, Size={}",
                                TOTAL_EXITS, rip, gpa, access_type_str, access_size
                            );
                        }
                    }
                    if should_log_mmio_gpa(gpa) {
                        use std::sync::atomic::{AtomicU64, Ordering};
                        static MMIO_DEBUG_COUNT: AtomicU64 = AtomicU64::new(0);
                        let n = MMIO_DEBUG_COUNT.fetch_add(1, Ordering::Relaxed);
                        if n < 512 || n % 256 == 0 {
                            windows_io_debug_log(format!(
                                "[MMIOEXIT] #{} gpa=0x{:08x} rip=0x{:x} type={} size={}",
                                n, gpa, rip, access_type_str, access_size
                            ));
                        }
                    }
                    if access_size > self.data_buffer.len() {
                        warn!(
                            "Unsupported WHPX MMIO access size {} at gpa=0x{gpa:x}",
                            access_size
                        );
                        return Ok(VcpuExit::Shutdown);
                    }
                    let instruction_len = memory_access.InstructionByteCount as usize;

                    // Log instruction bytes for debugging
                    if instruction_len == 0 {
                        warn!(
                            "WHPX MMIO access with zero instruction bytes at gpa=0x{gpa:x}, RIP=0x{:x}, access_type={access_type}, size={access_size}",
                            exit_context.VpContext.Rip
                        );
                    } else {
                        debug!(
                            "WHPX MMIO access: gpa=0x{gpa:x}, RIP=0x{:x}, instruction_len={instruction_len}, bytes={:02x?}",
                            exit_context.VpContext.Rip,
                            &memory_access.InstructionBytes[..instruction_len.min(16)]
                        );
                    }

                    // Get instruction bytes - either from WHPX or by fetching from guest memory
                    let mut instruction_buffer = [0u8; 15];
                    let mut skip_bytes = 0;
                    let instruction_bytes = if instruction_len > 0
                        && memory_access.InstructionBytes[0] != 0
                    {
                        // WHPX provided valid instruction bytes
                        memory_access
                            .InstructionBytes
                            .get(..instruction_len)
                            .ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "Invalid WHPX MMIO instruction length",
                                )
                            })?
                    } else {
                        // WHPX failed to provide instruction bytes - fetch manually from guest memory
                        let rip = exit_context.VpContext.Rip;
                        debug!(
                            "WHPX returned invalid instruction bytes at RIP=0x{:x}, fetching from guest memory",
                            rip
                        );

                        // Translate virtual RIP to physical address
                        // For higher-half kernel addresses (0xffffffff80000000+), we can directly
                        // calculate the physical address since we know the mapping
                        let gpa = if rip >= 0xffffffff80000000 {
                            // Higher-half kernel mapping: 0xffffffff80000000+ -> 0x0+
                            rip - 0xffffffff80000000
                        } else {
                            // For other addresses, try WHPX translation
                            let mut translate_result: WHV_TRANSLATE_GVA_RESULT =
                                unsafe { std::mem::zeroed() };
                            let mut translated_gpa: u64 = 0;
                            let translate_flags = WHV_TRANSLATE_GVA_FLAGS(0); // Read access

                            match unsafe {
                                WHvTranslateGva(
                                    self.partition,
                                    self.index,
                                    rip,
                                    translate_flags,
                                    &mut translate_result,
                                    &mut translated_gpa,
                                )
                            } {
                                Ok(_) if translate_result.ResultCode.0 == 0 => translated_gpa,
                                _ => {
                                    warn!("Failed to translate RIP 0x{:x}, skipping MMIO", rip);
                                    self.advance_rip(rip.wrapping_add(2))?;
                                    continue;
                                }
                            }
                        };

                        debug!("Translated RIP 0x{:x} to GPA 0x{:x}", rip, gpa);

                        // Read instruction bytes from guest memory
                        let guest_mem_ref = unsafe { &*guest_mem };
                        let fetch_len = instruction_buffer.len();
                        if let Err(e) = guest_mem_ref
                            .read_slice(&mut instruction_buffer[..fetch_len], GuestAddress(gpa))
                        {
                            warn!(
                                "Failed to read instruction bytes from GPA 0x{:x}: {}, skipping MMIO",
                                gpa, e
                            );
                            self.advance_rip(rip.wrapping_add(2))?;
                            continue;
                        }

                        debug!(
                            "Fetched instruction bytes from GPA 0x{:x} (RIP 0x{:x}): {:02x?}",
                            gpa,
                            rip,
                            &instruction_buffer[..fetch_len.min(16)]
                        );

                        // Skip leading zero bytes if present (alignment padding)
                        while skip_bytes < fetch_len && instruction_buffer[skip_bytes] == 0 {
                            skip_bytes += 1;
                        }

                        if skip_bytes > 0 && skip_bytes < fetch_len {
                            debug!(
                                "Skipping {} leading zero bytes, instruction starts at offset {}",
                                skip_bytes, skip_bytes
                            );
                        }

                        &instruction_buffer[skip_bytes..fetch_len]
                    };

                    // Adjust RIP to account for skipped bytes (for decode purposes only)
                    // Note: We still use the original RIP for decode_mmio_access because
                    // it needs to calculate the correct next_rip based on the actual instruction location
                    let decode_rip = exit_context.VpContext.Rip;

                    match access_type {
                        x if x == WHvMemoryAccessRead.0 => {
                            let decoded = match Self::decode_mmio_access(
                                decode_rip,
                                instruction_bytes,
                                false,
                            ) {
                                Ok(decoded) => {
                                    debug!(
                                        "MMIO read decoded: kind={:?}, next_rip=0x{:x}, decode_rip=0x{:x}",
                                        decoded.kind, decoded.next_rip, decode_rip
                                    );
                                    decoded
                                }
                                Err(e) => {
                                    warn!(
                                        "WHPX MMIO read decode failed (gpa=0x{gpa:x}, size={access_size}): {e}"
                                    );
                                    return Ok(VcpuExit::Shutdown);
                                }
                            };
                            let access_size = decoded.size;
                            if access_size > self.data_buffer.len() {
                                warn!(
                                    "Unsupported decoded WHPX MMIO read size {} at gpa=0x{gpa:x}",
                                    access_size
                                );
                                return Ok(VcpuExit::Shutdown);
                            }
                            let (reg_index, high8, write_full, sign_extend) = match decoded.kind {
                                MmioAccessKind::Noop => {
                                    self.advance_rip(decoded.next_rip)?;
                                    continue;
                                }
                                MmioAccessKind::ReadReg { reg_index, high8 } => {
                                    (reg_index, high8, false, false)
                                }
                                MmioAccessKind::ReadRegZeroExtend { reg_index } => {
                                    (reg_index, false, true, false)
                                }
                                MmioAccessKind::ReadRegSignExtend { reg_index } => {
                                    (reg_index, false, true, true)
                                }
                                _ => {
                                    warn!(
                                        "Unexpected MMIO read decode kind (gpa=0x{gpa:x}, size={access_size})"
                                    );
                                    return Ok(VcpuExit::Shutdown);
                                }
                            };
                            if should_log_mmio_gpa(gpa) {
                                windows_io_debug_log(format!(
                                    "[MMIOREAD-DECODE] gpa=0x{:08x} rip=0x{:x} size={} next_rip=0x{:x} kind={:?} bytes={:02x?}",
                                    gpa,
                                    decode_rip,
                                    access_size,
                                    decoded.next_rip,
                                    decoded.kind,
                                    &instruction_bytes[..instruction_bytes.len().min(16)]
                                ));
                            }
                            self.pending_mmio_read = Some(PendingMmioRead {
                                gpa,
                                size: access_size,
                                next_rip: decoded.next_rip,
                                reg_index,
                                high8,
                                write_full,
                                sign_extend,
                            });
                            self.pending_mmio_write = None;
                            return Ok(VcpuExit::MmioRead(
                                gpa,
                                &mut self.data_buffer[..access_size],
                            ));
                        }
                        x if x == WHvMemoryAccessWrite.0 => {
                            let decoded = match Self::decode_mmio_access(
                                decode_rip,
                                instruction_bytes,
                                true,
                            ) {
                                Ok(decoded) => {
                                    debug!(
                                        "MMIO write decoded: kind={:?}, next_rip=0x{:x}, decode_rip=0x{:x}",
                                        decoded.kind, decoded.next_rip, decode_rip
                                    );
                                    decoded
                                }
                                Err(e) => {
                                    warn!(
                                        "WHPX MMIO write decode failed (gpa=0x{gpa:x}, size={access_size}): {e}"
                                    );
                                    return Ok(VcpuExit::Shutdown);
                                }
                            };
                            let access_size = decoded.size;
                            if access_size > self.data_buffer.len() {
                                warn!(
                                    "Unsupported decoded WHPX MMIO write size {} at gpa=0x{gpa:x}",
                                    access_size
                                );
                                return Ok(VcpuExit::Shutdown);
                            }
                            let write_value = match decoded.kind {
                                MmioAccessKind::Noop => {
                                    self.advance_rip(decoded.next_rip)?;
                                    continue;
                                }
                                MmioAccessKind::WriteReg { reg_index, high8 } => {
                                    let reg = self.get_register_u64(reg_index)?;
                                    Self::reg_bits(reg, access_size, high8)?
                                }
                                MmioAccessKind::WriteImm { value } => {
                                    Self::reg_bits(value, access_size, false)?
                                }
                                _ => {
                                    warn!(
                                        "Unexpected MMIO write decode kind (gpa=0x{gpa:x}, size={access_size})"
                                    );
                                    return Ok(VcpuExit::Shutdown);
                                }
                            };
                            if should_log_mmio_gpa(gpa) {
                                windows_io_debug_log(format!(
                                    "[MMIOWRITE-DECODE] gpa=0x{:08x} rip=0x{:x} size={} next_rip=0x{:x} kind={:?} bytes={:02x?}",
                                    gpa,
                                    decode_rip,
                                    access_size,
                                    decoded.next_rip,
                                    decoded.kind,
                                    &instruction_bytes[..instruction_bytes.len().min(16)]
                                ));
                            }

                            for i in 0..access_size {
                                self.data_buffer[i] = ((write_value >> (i * 8)) & 0xff) as u8;
                            }

                            self.pending_mmio_write = Some(PendingMmioWrite {
                                gpa,
                                next_rip: decoded.next_rip,
                            });
                            self.pending_mmio_read = None;
                            return Ok(VcpuExit::MmioWrite(gpa, &self.data_buffer[..access_size]));
                        }
                        x if x == WHvMemoryAccessExecute.0 => {
                            // WHPX software emulation (InstructionByteCount=0 on a prior I/O
                            // exit) can land execution at an unmapped GPA. Manual RIP advancement
                            // via WHvSetVirtualProcessorRegisters is silently ignored in this mode;
                            // the proper fix requires WHvEmulatorTryIoEmulation. Stop the vCPU
                            // rather than looping endlessly on the same Execute exit.
                            warn!(
                                "WHPX Execute MemoryAccess at gpa=0x{gpa:x} (software emulation \
                                 mode): stopping vCPU"
                            );
                            return Ok(VcpuExit::Shutdown);
                        }
                        _ => {
                            warn!(
                                "Unsupported WHPX memory access type {} at gpa=0x{gpa:x}",
                                access_type
                            );
                            return Ok(VcpuExit::Shutdown);
                        }
                    }
                }
                reason if reason == WHvRunVpExitReasonX64IoPortAccess => {
                    let io_port = unsafe { exit_context.Anonymous.IoPortAccess };
                    let port = io_port.PortNumber;
                    let io_access_bits = unsafe { io_port.AccessInfo.AsUINT32 };
                    let size = (((io_access_bits >> 1) & 0x7) as usize).max(1);
                    if size > self.data_buffer.len() {
                        warn!(
                            "Unsupported WHPX I/O access size {} on port 0x{port:04x}",
                            size
                        );
                        return Ok(VcpuExit::Shutdown);
                    }
                    let is_write = (io_access_bits & 1) != 0;
                    let string_op = (io_access_bits & (1 << 4)) != 0;
                    let rep_prefix = (io_access_bits & (1 << 5)) != 0;
                    let rip = exit_context.VpContext.Rip;

                    {
                        use std::sync::atomic::{AtomicU64, Ordering};
                        static IO_DEBUG_COUNT: AtomicU64 = AtomicU64::new(0);
                        let n = IO_DEBUG_COUNT.fetch_add(1, Ordering::Relaxed);
                        let pending_depth = self.pending_interrupt_depth();
                        if n < 128
                            || n % 1024 == 0
                            || should_always_log_io_port(port)
                            || pending_depth != 0
                        {
                            windows_io_debug_log(format!(
                                "[IOEXIT] #{} port=0x{:04x} write={} size={} string_op={} rep_prefix={} instr_len={} rip=0x{:x} pending_depth={}",
                                n,
                                port,
                                is_write,
                                size,
                                string_op,
                                rep_prefix,
                                io_port.InstructionByteCount,
                                rip,
                                pending_depth
                            ));
                        }
                    }

                    // When InstructionByteCount=0 and this is a simple (non-string,
                    // non-rep) port IO, delegate to WHvEmulatorTryIoEmulation.
                    // This is the only correct path: calling WHvSetVirtualProcessorRegisters(RIP)
                    // manually is silently ignored by WHPX in software-emulation mode.
                    if io_port.InstructionByteCount == 0 && !string_op && !rep_prefix {
                        let mut ctx = EmulatorContext {
                            partition: self.partition,
                            vp_index: self.index,
                            vcpu_id,
                            io_bus,
                            guest_mem,
                        };
                        let status = unsafe {
                            WHvEmulatorTryIoEmulation(
                                self.emulator as *const c_void,
                                &mut ctx as *mut EmulatorContext as *const c_void,
                                &exit_context.VpContext,
                                &io_port,
                            )
                        }
                        .map_err(|e| {
                            io::Error::other(format!(
                                "WHvEmulatorTryIoEmulation failed on port 0x{port:04x}: {e}"
                            ))
                        })?;
                        if unsafe { status.AsUINT32 } & 1 != 0 {
                            continue; // EmulationSuccessful — RIP advanced by emulator
                        }
                        warn!(
                            "WHPX IO emulation unsuccessful on port 0x{port:04x} \
                             (status={:#010x}): stopping vCPU",
                            unsafe { status.AsUINT32 }
                        );
                        return Ok(VcpuExit::Shutdown);
                    }

                    // WHPX on some Windows builds returns InstructionByteCount=0.
                    // Fall back to opcode-level decoding in that case.
                    let instr_len = if io_port.InstructionByteCount > 0 {
                        io_port.InstructionByteCount as u64
                    } else {
                        Self::decode_io_instr_len(&io_port.InstructionBytes)
                    };
                    let next_rip = rip.wrapping_add(instr_len);

                    if string_op || rep_prefix {
                        // Best-effort compatibility path for debug/legacy serial ports.
                        if Self::allow_string_io_fallback(port) {
                            if rep_prefix {
                                // Treat REP string I/O as fully consumed to avoid re-executing
                                // the same instruction in tight debug output loops.
                                let names = [WHvX64RegisterRip, WHvX64RegisterRcx];
                                let values = unsafe {
                                    let mut v = [std::mem::zeroed::<WHV_REGISTER_VALUE>(); 2];
                                    v[0].Reg64 = next_rip;
                                    v[1].Reg64 = 0;
                                    v
                                };
                                self.set_registers(&names, &values)?;
                            } else {
                                self.advance_rip(next_rip)?;
                            }
                            continue;
                        }

                        warn!(
                            "Unsupported WHPX I/O string op on port 0x{port:04x} (string_op={string_op}, rep_prefix={rep_prefix})"
                        );
                        return Ok(VcpuExit::Shutdown);
                    }

                    if is_write {
                        let rax = io_port.Rax;
                        for i in 0..size {
                            self.data_buffer[i] = ((rax >> (i * 8)) & 0xff) as u8;
                        }
                        // Log serial port writes
                        if port >= 0x3f8 && port <= 0x3ff {
                            log::debug!(
                                "[IO] Serial write to port {:#x}, size={}, data={:02x?}",
                                port,
                                size,
                                &self.data_buffer[..size]
                            );
                        }
                        if should_always_log_io_port(port) {
                            windows_io_debug_log(format!(
                                "[IOWRITE] port=0x{:04x} size={} data={:02x?}",
                                port,
                                size,
                                &self.data_buffer[..size]
                            ));
                        }
                        self.pending_io_write = Some(PendingIoWrite { port, next_rip });
                        return Ok(VcpuExit::IoPortWrite(port, &self.data_buffer[..size]));
                    } else {
                        // Log serial port reads
                        if port >= 0x3f8 && port <= 0x3ff {
                            log::trace!("[IO] Serial read from port {:#x}, size={}", port, size);
                        }
                        self.pending_io_read = Some(PendingIoRead {
                            port,
                            size,
                            next_rip,
                        });
                        return Ok(VcpuExit::IoPortRead(port, &mut self.data_buffer[..size]));
                    }
                }
                reason if reason == WHvRunVpExitReasonX64Cpuid => {
                    windows_exit_debug_log("post_cpuid_branch", exit_context.VpContext.Rip, "");
                    self.emulate_cpuid(&exit_context)?;
                    let can_inject_now = (exit_context.VpContext.Rflags & (1 << 9)) != 0;
                    let _ = self.inject_pending_interrupt(
                        exit_context.VpContext.Rip,
                        can_inject_now,
                        "post-cpuid",
                    )?;
                    windows_exit_debug_log(
                        "post_cpuid_after_service",
                        exit_context.VpContext.Rip,
                        "",
                    );
                }
                reason if reason == WHvRunVpExitReasonX64MsrAccess => {
                    self.emulate_msr(&exit_context)?;
                    let can_inject_now = (exit_context.VpContext.Rflags & (1 << 9)) != 0;
                    let _ = self.inject_pending_interrupt(
                        exit_context.VpContext.Rip,
                        can_inject_now,
                        "post-msr",
                    )?;
                }
                reason if reason == WHvRunVpExitReasonX64Rdtsc => {
                    self.emulate_rdtsc(&exit_context)?;
                    let can_inject_now = (exit_context.VpContext.Rflags & (1 << 9)) != 0;
                    let _ = self.inject_pending_interrupt(
                        exit_context.VpContext.Rip,
                        can_inject_now,
                        "post-rdtsc",
                    )?;
                }
                reason if reason == WHvRunVpExitReasonX64InterruptWindow => {
                    // Interrupt window opened - guest is ready to receive interrupts
                    windows_exit_debug_log("interrupt_window", exit_context.VpContext.Rip, "");
                    if self.inject_pending_interrupt(
                        exit_context.VpContext.Rip,
                        true,
                        "interrupt-window",
                    )? {
                        continue;
                    }
                    log::debug!(
                        "Interrupt window opened at RIP={:#x}",
                        exit_context.VpContext.Rip
                    );
                }
                reason if reason == WHvRunVpExitReasonX64ApicEoi => {
                    // APIC EOI - interrupt was acknowledged by guest
                    windows_exit_debug_log("apic_eoi", exit_context.VpContext.Rip, "");
                    log::debug!(
                        "APIC EOI at RIP={:#x} - interrupt was handled",
                        exit_context.VpContext.Rip
                    );
                }
                reason if reason == windows::Win32::System::Hypervisor::WHvRunVpExitReasonNone => {
                    // No state changes; re-enter VP run loop.
                }
                reason if reason == WHvRunVpExitReasonSynicSintDeliverable => {
                    windows_exit_debug_log(
                        "synic_sint_deliverable",
                        exit_context.VpContext.Rip,
                        "",
                    );
                    log::debug!(
                        "WHPX SynIC SINT deliverable exit at RIP={:#x}; resuming vCPU",
                        exit_context.VpContext.Rip
                    );
                }
                reason
                    if reason == WHvRunVpExitReasonUnsupportedFeature
                        || reason == WHvRunVpExitReasonInvalidVpRegisterValue =>
                {
                    if reason == WHvRunVpExitReasonInvalidVpRegisterValue {
                        self.log_invalid_vp_register_state(&exit_context);
                    }
                    warn!(
                        "Unsupported WHPX synthetic/hypercall exit (reason={}): stopping vCPU",
                        reason.0
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonX64ApicWriteTrap => {
                    // WHPX hardware-APIC mode: the virtual APIC has already
                    // processed the write. Record the write so we can confirm
                    // whether the guest is actually programming the LAPIC timer
                    // and virtual-wire delivery state.
                    let apic_write = unsafe { exit_context.Anonymous.ApicWrite };
                    static APIC_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
                    let count = APIC_WRITE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    if count <= 64 || count % 256 == 0 {
                        windows_exit_debug_log(
                            "apic_write_trap",
                            exit_context.VpContext.Rip,
                            format!(
                                "count={} type={} type_raw=0x{:x} value=0x{:016x}",
                                count,
                                describe_apic_write_type(apic_write.Type),
                                apic_write.Type.0 as u32,
                                apic_write.WriteValue,
                            ),
                        );
                    }
                }
                reason if reason == WHvRunVpExitReasonX64ApicInitSipiTrap => {
                    self.emulate_apic_init_sipi(&exit_context)?;
                }
                reason if reason == WHvRunVpExitReasonX64ApicSmiTrap => {
                    // SMIs are not expected in the guest. Treat as no-op.
                }
                reason if reason == WHvRunVpExitReasonHypercall => {
                    let hypercall = unsafe { exit_context.Anonymous.Hypercall };
                    windows_exit_debug_log(
                        "hypercall",
                        exit_context.VpContext.Rip,
                        format!("rax=0x{:x} rbx=0x{:x}", hypercall.Rax, hypercall.Rbx),
                    );
                    warn!(
                        "WHPX hypercall exit (rax=0x{:x}, rbx=0x{:x}): stopping vCPU",
                        hypercall.Rax, hypercall.Rbx
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonX64Halt => {
                    let rip = exit_context.VpContext.Rip;
                    let can_inject_now = (exit_context.VpContext.Rflags & (1 << 9)) != 0;
                    if self.inject_pending_interrupt(rip, can_inject_now, "hlt")? {
                        continue;
                    }
                    debug!(
                        "HLT instruction executed at RIP={:#x} - vCPU halted, waiting for interrupt",
                        rip
                    );
                    windows_exit_debug_log("hlt", rip, "");
                    return Ok(VcpuExit::Halted);
                }
                reason if reason == WHvRunVpExitReasonCanceled => {
                    // vCPU was interrupted by WHvCancelRunVirtualProcessor.
                    // This is used to force interrupt delivery when the guest
                    // is in a tight loop. Just continue execution.
                    windows_exit_debug_log("canceled", exit_context.VpContext.Rip, "");
                    let can_inject_now = (exit_context.VpContext.Rflags & (1 << 9)) != 0;
                    if self.inject_pending_interrupt(
                        exit_context.VpContext.Rip,
                        can_inject_now,
                        "canceled",
                    )? {
                        continue;
                    }
                    let halted_reenter = windows_pending_event_halted_reenter()
                        && self.pending_event_busy_and_halted()?;
                    windows_exit_debug_log(
                        "canceled_post_probe",
                        exit_context.VpContext.Rip,
                        format!("halted_reenter={}", halted_reenter),
                    );
                    if halted_reenter {
                        windows_exit_debug_log(
                            "canceled_halted_reenter",
                            exit_context.VpContext.Rip,
                            "pending_event_busy=true internal_activity=hlt",
                        );
                        return Ok(VcpuExit::Halted);
                    }
                    log::debug!(
                        "vCPU {} canceled by WHvCancelRunVirtualProcessor, continuing execution",
                        self.index
                    );
                    continue;
                }
                reason if reason == WHvRunVpExitReasonException => {
                    if self.emulate_exception(&exit_context)? {
                        continue;
                    }
                    let exception = unsafe { exit_context.Anonymous.VpException };
                    windows_exit_debug_log(
                        "exception",
                        exit_context.VpContext.Rip,
                        format!(
                            "type={} error=0x{:x}",
                            exception.ExceptionType, exception.ExceptionParameter
                        ),
                    );
                    warn!(
                        "Unhandled WHPX exception: ExceptionType={}, ErrorCode=0x{:x}, RIP=0x{:x}",
                        exception.ExceptionType,
                        exception.ExceptionParameter,
                        exit_context.VpContext.Rip
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonUnrecoverableException => {
                    let exception = unsafe { exit_context.Anonymous.VpException };
                    windows_exit_debug_log(
                        "unrecoverable_exception",
                        exit_context.VpContext.Rip,
                        format!(
                            "type={} error=0x{:x}",
                            exception.ExceptionType, exception.ExceptionParameter
                        ),
                    );
                    warn!(
                        "WHPX unrecoverable exception: ExceptionType={}, ErrorCode=0x{:x}, RIP=0x{:x}",
                        exception.ExceptionType,
                        exception.ExceptionParameter,
                        exit_context.VpContext.Rip
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                other => {
                    windows_exit_debug_log(
                        "unsupported",
                        exit_context.VpContext.Rip,
                        format!("reason={}", other.0),
                    );
                    warn!(
                        "Unsupported WHPX exit reason {} at RIP=0x{:x}: stopping vCPU",
                        other.0, exit_context.VpContext.Rip
                    );
                    return Ok(VcpuExit::Shutdown);
                }
            }
        }
    }
}

impl Drop for WhpxVcpu {
    fn drop(&mut self) {
        // SAFETY: WHvDeleteVirtualProcessor and WHvEmulatorDestroyEmulator are safe to
        // call with valid handles. We ignore errors because Drop cannot fail, and the
        // vCPU may already be in an invalid state during cleanup.
        unsafe {
            let _ = WHvEmulatorDestroyEmulator(self.emulator as *const c_void);
            let _ = WHvDeleteVirtualProcessor(self.partition, self.index);
        }
    }
}

// WHPX backend currently handles the x86_64 boot/runtime exits required for
// libkrun bring-up and maps unsupported synthetic/APIC traps to shutdown.

#[cfg(test)]
mod tests {
    use super::{MmioAccessKind, WhpxVcpu};
    use std::io;

    #[test]
    fn test_legacy_prefix_detection() {
        assert!(WhpxVcpu::is_legacy_prefix(0x66));
        assert!(WhpxVcpu::is_legacy_prefix(0xF3));
        assert!(WhpxVcpu::is_legacy_prefix(0x2E));
        assert!(!WhpxVcpu::is_legacy_prefix(0x90));
    }

    #[test]
    fn test_string_io_fallback_ports() {
        assert!(WhpxVcpu::allow_string_io_fallback(0x3F8));
        assert!(WhpxVcpu::allow_string_io_fallback(0x3FF));
        assert!(WhpxVcpu::allow_string_io_fallback(0xE9));
        assert!(WhpxVcpu::allow_string_io_fallback(0x80));
        assert!(WhpxVcpu::allow_string_io_fallback(0x402));
        assert!(!WhpxVcpu::allow_string_io_fallback(0x1234));
        assert!(!WhpxVcpu::allow_string_io_fallback(0x400));
    }

    #[test]
    fn test_gpr_name_bounds() {
        assert!(WhpxVcpu::gpr_name(0).is_ok());
        assert!(WhpxVcpu::gpr_name(15).is_ok());
        assert!(matches!(
            WhpxVcpu::gpr_name(16),
            Err(err) if err.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn test_reg_bits_and_merge() {
        assert_eq!(WhpxVcpu::reg_bits(0xA1B2, 1, false).unwrap(), 0xB2);
        assert_eq!(WhpxVcpu::reg_bits(0xA1B2, 1, true).unwrap(), 0xA1);
        assert_eq!(
            WhpxVcpu::reg_bits(0x1122_3344_5566_7788, 4, false).unwrap(),
            0x5566_7788
        );

        assert_eq!(
            WhpxVcpu::merge_reg_bits(0x1122_3344_5566_7788, 1, false, 0xAA).unwrap(),
            0x1122_3344_5566_77AA
        );
        assert_eq!(
            WhpxVcpu::merge_reg_bits(0x1122_3344_5566_7788, 1, true, 0xBB).unwrap(),
            0x1122_3344_5566_BB88
        );
        assert_eq!(
            WhpxVcpu::merge_reg_bits(0xFFFF_0000_FFFF_0000, 4, false, 0x1234_5678).unwrap(),
            0x0000_0000_1234_5678
        );

        assert!(matches!(
            WhpxVcpu::reg_bits(0x12, 3, false),
            Err(err) if err.kind() == io::ErrorKind::InvalidInput
        ));
        assert!(matches!(
            WhpxVcpu::merge_reg_bits(0x12, 3, false, 0x34),
            Err(err) if err.kind() == io::ErrorKind::InvalidInput
        ));
    }

    #[test]
    fn test_skip_modrm_address() {
        // mod=00, rm=101 => disp32
        assert_eq!(
            WhpxVcpu::skip_modrm_address(&[0, 0, 0, 0], 0, 0x05).unwrap(),
            4
        );
        // mod=01 => disp8
        assert_eq!(WhpxVcpu::skip_modrm_address(&[0], 0, 0x40).unwrap(), 1);
        // mod=10 => disp32
        assert_eq!(
            WhpxVcpu::skip_modrm_address(&[0, 0, 0, 0], 0, 0x80).unwrap(),
            4
        );
        // mod=00, rm=100 + SIB base=101 => SIB + disp32
        assert_eq!(
            WhpxVcpu::skip_modrm_address(&[0x25, 0, 0, 0, 0], 0, 0x04).unwrap(),
            5
        );
        // mod=00, rm=100 + SIB base!=101 => SIB only
        assert_eq!(WhpxVcpu::skip_modrm_address(&[0x20], 0, 0x04).unwrap(), 1);
    }

    #[test]
    fn test_skip_modrm_address_errors() {
        assert!(matches!(
            WhpxVcpu::skip_modrm_address(&[], 0, 0xC0),
            Err(err) if err.kind() == io::ErrorKind::Unsupported
        ));
        assert!(matches!(
            WhpxVcpu::skip_modrm_address(&[], 0, 0x04),
            Err(err) if err.kind() == io::ErrorKind::InvalidData
        ));
        // idx overflow on displacement advance.
        assert!(matches!(
            WhpxVcpu::skip_modrm_address(&[0], usize::MAX, 0x40),
            Err(err) if err.kind() == io::ErrorKind::InvalidData
        ));
    }

    #[test]
    fn test_decode_mmio_access_prefetch_noop() {
        let decoded = WhpxVcpu::decode_mmio_access(0x1000, &[0x0f, 0x18, 0x00], false).unwrap();
        assert_eq!(decoded.next_rip, 0x1003);
        assert!(matches!(decoded.kind, MmioAccessKind::Noop));
    }

    #[test]
    fn test_decode_mmio_access_movzx_and_movsxd() {
        let decoded_movzx =
            WhpxVcpu::decode_mmio_access(0x2000, &[0x0f, 0xb6, 0x18], false).unwrap();
        assert_eq!(decoded_movzx.next_rip, 0x2003);
        assert!(matches!(
            decoded_movzx.kind,
            MmioAccessKind::ReadRegZeroExtend { reg_index: 3 }
        ));

        let decoded_movsxd =
            WhpxVcpu::decode_mmio_access(0x3000, &[0x44, 0x63, 0x08], false).unwrap();
        assert_eq!(decoded_movsxd.next_rip, 0x3003);
        assert!(matches!(
            decoded_movsxd.kind,
            MmioAccessKind::ReadRegSignExtend { reg_index: 9 }
        ));

        // Legacy high-8 register encoding without REX.
        let decoded_high8 = WhpxVcpu::decode_mmio_access(0x3100, &[0x8a, 0x20], false).unwrap();
        assert_eq!(decoded_high8.next_rip, 0x3102);
        assert!(matches!(
            decoded_high8.kind,
            MmioAccessKind::ReadReg {
                reg_index: 0,
                high8: true
            }
        ));

        // With REX prefix the same reg field maps to extended register, not high-8.
        let decoded_rex = WhpxVcpu::decode_mmio_access(0x3200, &[0x44, 0x8a, 0x20], false).unwrap();
        assert_eq!(decoded_rex.next_rip, 0x3203);
        assert!(matches!(
            decoded_rex.kind,
            MmioAccessKind::ReadReg {
                reg_index: 12,
                high8: false
            }
        ));
    }

    #[test]
    fn test_decode_mmio_access_write_immediates() {
        let c6 =
            WhpxVcpu::decode_mmio_access(0x4000, &[0xc6, 0x05, 0, 0, 0, 0, 0x7f], true).unwrap();
        assert_eq!(c6.next_rip, 0x4007);
        assert!(matches!(c6.kind, MmioAccessKind::WriteImm { value: 0x7f }));

        let c7 = WhpxVcpu::decode_mmio_access(
            0x5000,
            &[0x48, 0xc7, 0x05, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
            true,
        )
        .unwrap();
        assert_eq!(c7.next_rip, 0x500b);
        assert!(matches!(
            c7.kind,
            MmioAccessKind::WriteImm { value: u64::MAX }
        ));

        // moffs write form should map to RAX register source.
        let moffs_write =
            WhpxVcpu::decode_mmio_access(0x5100, &[0x48, 0xa3, 0, 0, 0, 0], true).unwrap();
        assert_eq!(moffs_write.next_rip, 0x5106);
        assert!(matches!(
            moffs_write.kind,
            MmioAccessKind::WriteReg {
                reg_index: 0,
                high8: false
            }
        ));

        // C7 with 16-bit immediate uses imm16 width.
        let c7_imm16 =
            WhpxVcpu::decode_mmio_access(0x5200, &[0x66, 0xc7, 0x05, 0, 0, 0, 0, 0x34, 0x12], true)
                .unwrap();
        assert_eq!(c7_imm16.next_rip, 0x5209);
        assert!(matches!(
            c7_imm16.kind,
            MmioAccessKind::WriteImm { value: 0x1234 }
        ));
    }

    #[test]
    fn test_decode_mmio_access_table_driven_core_cases() {
        struct Case {
            rip: u64,
            bytes: &'static [u8],
            access_size: usize,
            is_write: bool,
        }

        // Format: (input case, expected next_rip, expected reg/high8)
        let read_reg_cases = [
            (
                Case {
                    rip: 0x6000,
                    bytes: &[0x8a, 0x18], // reg=3, no REX, 8-bit read
                    access_size: 1,
                    is_write: false,
                },
                0x6002,
                3_u8,
                false,
            ),
            (
                Case {
                    rip: 0x6010,
                    bytes: &[0x44, 0x88, 0x20], // reg=4 + REX.R => 12
                    access_size: 1,
                    is_write: true,
                },
                0x6013,
                12_u8,
                false,
            ),
            (
                Case {
                    rip: 0x6020,
                    bytes: &[0xa0, 0, 0, 0, 0], // moffs read -> RAX source
                    access_size: 1,
                    is_write: false,
                },
                0x6025,
                0_u8,
                false,
            ),
        ];

        for (case, expected_rip, expected_reg, expected_high8) in read_reg_cases {
            let decoded =
                WhpxVcpu::decode_mmio_access(case.rip, case.bytes, case.is_write).unwrap();
            assert_eq!(decoded.next_rip, expected_rip);
            match decoded.kind {
                MmioAccessKind::ReadReg { reg_index, high8 }
                | MmioAccessKind::WriteReg { reg_index, high8 } => {
                    assert_eq!(reg_index, expected_reg);
                    assert_eq!(high8, expected_high8);
                }
                other => panic!("unexpected decode kind: {:?}", other),
            }
        }

        let zero_sign_cases = [
            (
                Case {
                    rip: 0x6100,
                    bytes: &[0x0f, 0xb7, 0x08],
                    access_size: 2,
                    is_write: false,
                },
                true,
                1_u8,
            ),
            (
                Case {
                    rip: 0x6110,
                    bytes: &[0x0f, 0xbf, 0x08],
                    access_size: 2,
                    is_write: false,
                },
                false,
                1_u8,
            ),
        ];

        for (case, expect_zero_extend, expected_reg) in zero_sign_cases {
            let decoded =
                WhpxVcpu::decode_mmio_access(case.rip, case.bytes, case.is_write).unwrap();
            if expect_zero_extend {
                assert!(matches!(
                    decoded.kind,
                    MmioAccessKind::ReadRegZeroExtend { reg_index } if reg_index == expected_reg
                ));
            } else {
                assert!(matches!(
                    decoded.kind,
                    MmioAccessKind::ReadRegSignExtend { reg_index } if reg_index == expected_reg
                ));
            }
        }
    }

    #[test]
    fn test_decode_mmio_access_table_driven_error_cases() {
        struct ErrCase {
            bytes: &'static [u8],
            access_size: usize,
            is_write: bool,
            kind: io::ErrorKind,
        }

        let invalid_data_cases = [
            ErrCase {
                bytes: &[0x0f], // missing second opcode byte
                access_size: 1,
                is_write: false,
                kind: io::ErrorKind::InvalidData,
            },
            ErrCase {
                bytes: &[0x0f, 0xb6], // missing ModRM
                access_size: 1,
                is_write: false,
                kind: io::ErrorKind::InvalidData,
            },
            ErrCase {
                bytes: &[0xc6, 0x00], // missing imm8
                access_size: 1,
                is_write: true,
                kind: io::ErrorKind::InvalidData,
            },
            ErrCase {
                bytes: &[0xc7, 0x00, 0x11, 0x22], // missing full imm32
                access_size: 4,
                is_write: true,
                kind: io::ErrorKind::InvalidData,
            },
        ];

        for case in invalid_data_cases {
            let res = WhpxVcpu::decode_mmio_access(0x7000, case.bytes, case.is_write);
            assert!(matches!(res, Err(err) if err.kind() == case.kind));
        }

        let unsupported_cases = [
            ErrCase {
                bytes: &[0x90], // unsupported opcode
                access_size: 1,
                is_write: false,
                kind: io::ErrorKind::Unsupported,
            },
            ErrCase {
                bytes: &[0x88, 0x00], // write opcode in read path
                access_size: 1,
                is_write: false,
                kind: io::ErrorKind::Unsupported,
            },
            ErrCase {
                bytes: &[0xa2, 0, 0, 0, 0], // moffs write opcode in read path
                access_size: 1,
                is_write: false,
                kind: io::ErrorKind::Unsupported,
            },
            ErrCase {
                bytes: &[0x0f, 0xaa, 0x00], // unsupported two-byte opcode
                access_size: 1,
                is_write: false,
                kind: io::ErrorKind::Unsupported,
            },
        ];

        for case in unsupported_cases {
            let res = WhpxVcpu::decode_mmio_access(0x7100, case.bytes, case.is_write);
            assert!(matches!(res, Err(err) if err.kind() == case.kind));
        }
    }

    #[test]
    fn test_decode_mmio_access_errors() {
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[], false),
            Err(err) if err.kind() == io::ErrorKind::InvalidData
        ));

        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xa0], true),
            Err(err) if err.kind() == io::ErrorKind::Unsupported
        ));

        // Unsupported ModRM extension for C6/C7 immediate write forms.
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xc6, 0x08, 0x12], true),
            Err(err) if err.kind() == io::ErrorKind::Unsupported
        ));
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xc7, 0x08, 0, 0, 0, 0], true),
            Err(err) if err.kind() == io::ErrorKind::Unsupported
        ));

        // Immediate bytes missing.
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xc7, 0x05, 0, 0, 0, 0], true),
            Err(err) if err.kind() == io::ErrorKind::InvalidData
        ));

        // next_rip must wrap correctly on overflow.
        let wrapped = WhpxVcpu::decode_mmio_access(u64::MAX, &[0x8a, 0x00], false).unwrap();
        assert_eq!(wrapped.next_rip, 1);
    }
}
