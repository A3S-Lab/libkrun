// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Enables pre-boot setup, instantiation and booting of a Firecracker VMM.

#[cfg(target_os = "macos")]
use crossbeam_channel::unbounded;
use crossbeam_channel::Sender;
use kernel::cmdline::Cmdline;
#[cfg(target_os = "macos")]
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::fs::File;
#[cfg(target_os = "windows")]
use std::fs::OpenOptions;
#[cfg(not(target_os = "windows"))]
use std::io::IsTerminal;
use std::io::{self, Read};
#[cfg(not(target_os = "windows"))]
use std::os::fd::AsRawFd;
#[cfg(not(target_os = "windows"))]
use std::os::fd::{BorrowedFd, FromRawFd};
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};

use super::{Error, Vmm};

#[cfg(target_arch = "x86_64")]
use crate::device_manager::legacy::PortIODeviceManager;
use crate::device_manager::mmio::MMIODeviceManager;
use crate::resources::{
    DefaultVirtioConsoleConfig, PortConfig, TsiFlags, VirtioConsoleConfigMode, VmResources,
};
#[cfg(target_os = "windows")]
use crate::vmm_config::block_windows::BlockWindowsBuilder;
use crate::vmm_config::external_kernel::{ExternalKernel, KernelFormat};
#[cfg(feature = "net")]
use crate::vmm_config::net::NetBuilder;
#[cfg(target_os = "windows")]
use crate::vmm_config::net_windows::NetWindowsBuilder;
#[cfg(target_arch = "x86_64")]
use devices::legacy::Cmos;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use devices::legacy::IoApic;
#[cfg(target_arch = "x86_64")]
use devices::legacy::IrqChipT;
#[cfg(all(target_os = "linux", target_arch = "riscv64"))]
use devices::legacy::KvmAia;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use devices::legacy::KvmIoapic;
use devices::legacy::Serial;
#[cfg(target_os = "macos")]
use devices::legacy::VcpuList;
#[cfg(target_os = "macos")]
use devices::legacy::{GicV3, HvfGicV3};
use devices::legacy::{IrqChip, IrqChipDevice};
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
use devices::legacy::{KvmGicV2, KvmGicV3};
use devices::virtio::{port_io, MmioTransport, PortDescription, VirtioDevice, Vsock};

#[cfg(feature = "tee")]
use kbs_types::Tee;

use crate::device_manager;
#[cfg(target_os = "linux")]
use crate::signal_handler::register_sigint_handler;
#[cfg(target_os = "linux")]
use crate::signal_handler::register_sigwinch_handler;
#[cfg(not(target_os = "windows"))]
use crate::terminal::{term_restore_mode, term_set_raw_mode};
#[cfg(feature = "blk")]
use crate::vmm_config::block::BlockBuilder;
#[cfg(not(any(feature = "tee", feature = "nitro")))]
use crate::vmm_config::fs::FsDeviceConfig;
use crate::vmm_config::kernel_cmdline::DEFAULT_KERNEL_CMDLINE;
#[cfg(target_os = "linux")]
use crate::vstate::KvmContext;
#[cfg(all(target_os = "linux", feature = "tee"))]
use crate::vstate::MeasuredRegion;
use crate::vstate::{Error as VstateError, Vcpu, VcpuConfig, Vm};
#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
use crate::windows::interrupts::{PendingInterrupt, PendingInterruptQueue};
use arch::{ArchMemoryInfo, InitrdConfig};
use device_manager::shm::ShmManager;
#[cfg(feature = "gpu")]
use devices::virtio::display::DisplayInfo;
#[cfg(feature = "gpu")]
use devices::virtio::display::NoopDisplayBackend;
#[cfg(not(any(feature = "tee", feature = "nitro")))]
use devices::virtio::{fs::ExportTable, VirtioShmRegion};
use flate2::read::GzDecoder;
#[cfg(feature = "gpu")]
use krun_display::DisplayBackend;
#[cfg(feature = "gpu")]
use krun_display::IntoDisplayBackend;
#[cfg(feature = "amd-sev")]
use kvm_bindings::KVM_MAX_CPUID_ENTRIES;
#[cfg(not(target_os = "windows"))]
use libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};

/// On Windows, wrap a CRT file descriptor as a `Write` sink.
///
/// Uses the CRT `_write()` function so that any fd—including pipes and file
/// handles obtained from `_open_osfhandle`—works correctly.  stdout / stderr
/// are handled separately above; this wrapper covers every other fd > 2.
#[cfg(target_os = "windows")]
struct CrtFdWriter(i32);

#[cfg(target_os = "windows")]
impl std::io::Write for CrtFdWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = unsafe { libc::write(self.0, buf.as_ptr() as *const _, buf.len() as _) };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn open_windows_console_output_file(path: &std::path::Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(target_os = "windows")]
fn windows_attach_implicit_virtio_console() -> bool {
    std::env::var("LIBKRUN_WINDOWS_IMPLICIT_VIRTIO_CONSOLE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn windows_attach_implicit_virtio_console() -> bool {
    true
}
#[cfg(target_arch = "x86_64")]
use linux_loader::loader::{self, KernelLoader};
#[cfg(not(target_os = "windows"))]
use nix::unistd::isatty;
use polly::event_manager::{Error as EventManagerError, EventManager};
use utils::eventfd::EventFd;
use utils::worker_message::WorkerMessage;
#[cfg(all(
    target_arch = "x86_64",
    not(feature = "efi"),
    not(feature = "tee"),
    not(target_os = "windows")
))]
use vm_memory::mmap::MmapRegion;
#[cfg(not(any(feature = "tee", feature = "nitro")))]
use vm_memory::Address;
use vm_memory::Bytes;
#[cfg(not(feature = "nitro"))]
use vm_memory::GuestMemory;
#[cfg(all(
    target_arch = "x86_64",
    not(feature = "tee"),
    not(target_os = "windows")
))]
use vm_memory::GuestRegionMmap;
use vm_memory::{GuestAddress, GuestMemoryMmap};

#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
static EDK2_BINARY: &[u8] = include_bytes!("../../../edk2/KRUN_EFI.silent.fd");

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
struct WhpxIrqChip {
    partition: windows::Win32::System::Hypervisor::WHV_PARTITION_HANDLE,
    vcpu_count: u32,
    irq_pending_evt: Arc<utils::eventfd::EventFd>,
    pending_interrupt: PendingInterruptQueue,
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn windows_irq_debug_log(message: impl AsRef<str>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| {
        std::env::var("LIBKRUN_WINDOWS_VERBOSE_DEBUG")
            .or_else(|_| std::env::var("LIBKRUN_WINDOWS_IO_DEBUG"))
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }) {
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

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn windows_force_pic_irq0_fixed() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_IRQ0_FIXED")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn windows_pic_fixed_pending_interruption() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_FIXED_INJECT")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "pending-interruption" | "pending-interrupt" | "register"
                )
            })
            .unwrap_or(false)
    })
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn windows_pic_fixed_builder_request_interrupt() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_FIXED_BUILDER_REQUEST_INTERRUPT")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn windows_skip_pic_fixed_ack() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_SKIP_PIC_FIXED_ACK")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn windows_skip_cancel_for_halted_irq_delivery() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_SKIP_CANCEL_ON_HLT_IRQ")
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowsPicFixedBuilderDirectMode {
    PendingInterruption,
    PendingEventExtInt,
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn windows_pic_fixed_builder_direct_mode() -> WindowsPicFixedBuilderDirectMode {
    static VALUE: std::sync::OnceLock<WindowsPicFixedBuilderDirectMode> =
        std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WHPX_PIC_FIXED_BUILDER_DIRECT_MODE")
            .ok()
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "pending-event" | "pending-event-extint" | "event" | "extint" => {
                    WindowsPicFixedBuilderDirectMode::PendingEventExtInt
                }
                _ => WindowsPicFixedBuilderDirectMode::PendingInterruption,
            })
            .unwrap_or(WindowsPicFixedBuilderDirectMode::PendingInterruption)
    })
}

#[cfg(target_os = "windows")]
fn windows_kernel_cmdline_append() -> Option<String> {
    std::env::var("LIBKRUN_WINDOWS_KERNEL_CMDLINE_APPEND")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn make_whpx_interrupt_control_bitfield(
    interrupt_type: windows::Win32::System::Hypervisor::WHV_INTERRUPT_TYPE,
    destination_mode: windows::Win32::System::Hypervisor::WHV_INTERRUPT_DESTINATION_MODE,
    trigger_mode: windows::Win32::System::Hypervisor::WHV_INTERRUPT_TRIGGER_MODE,
) -> u64 {
    (interrupt_type.0 as u64)
        | ((destination_mode.0 as u64) << 8)
        | ((trigger_mode.0 as u64) << 12)
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
#[derive(Clone, Copy, Debug)]
enum WhpxInterruptRoute {
    IoApic {
        vector: u32,
        destination_mode: windows::Win32::System::Hypervisor::WHV_INTERRUPT_DESTINATION_MODE,
        trigger_mode: windows::Win32::System::Hypervisor::WHV_INTERRUPT_TRIGGER_MODE,
        interrupt_type: windows::Win32::System::Hypervisor::WHV_INTERRUPT_TYPE,
        destination: u32,
    },
    PicExtIntRequest {
        irq: u8,
        vector: u8,
    },
    FallbackFixed {
        vector: u32,
    },
    Unresolved,
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
impl WhpxIrqChip {
    fn new(
        partition: windows::Win32::System::Hypervisor::WHV_PARTITION_HANDLE,
        vcpu_count: u32,
        irq_pending_evt: Arc<utils::eventfd::EventFd>,
        pending_interrupt: PendingInterruptQueue,
    ) -> Self {
        Self {
            partition,
            vcpu_count,
            irq_pending_evt,
            pending_interrupt,
        }
    }

    fn irq_to_vector(irq_line: u32) -> u32 {
        // Legacy ISA IRQ vectors are remapped starting at 0x20.
        0x20 + irq_line
    }

    fn resolve_interrupt_route(irq_line: u32) -> WhpxInterruptRoute {
        use windows::Win32::System::Hypervisor::{
            WHvX64InterruptDestinationModeLogical, WHvX64InterruptDestinationModePhysical,
            WHvX64InterruptTriggerModeEdge, WHvX64InterruptTriggerModeLevel,
            WHvX64InterruptTypeFixed, WHvX64InterruptTypeLowestPriority,
        };

        if let Some(route) = devices::legacy::windows_apic_stub::query_route(irq_line) {
            if !route.masked && route.vector != 0 {
                return WhpxInterruptRoute::IoApic {
                    vector: u32::from(route.vector),
                    destination_mode: if route.destination_mode_logical {
                        WHvX64InterruptDestinationModeLogical
                    } else {
                        WHvX64InterruptDestinationModePhysical
                    },
                    trigger_mode: if route.trigger_mode_level {
                        WHvX64InterruptTriggerModeLevel
                    } else {
                        WHvX64InterruptTriggerModeEdge
                    },
                    interrupt_type: if route.delivery_mode == 1 {
                        WHvX64InterruptTypeLowestPriority
                    } else {
                        WHvX64InterruptTypeFixed
                    },
                    destination: u32::from(route.destination),
                };
            }
        }

        if irq_line < 16 {
            if let Some(vector) = devices::legacy::windows_pic_stub::query_irq_vector(irq_line as u8)
            {
                return WhpxInterruptRoute::PicExtIntRequest {
                    irq: irq_line as u8,
                    vector,
                };
            }
            return WhpxInterruptRoute::Unresolved;
        }

        WhpxInterruptRoute::FallbackFixed {
            vector: Self::irq_to_vector(irq_line),
        }
    }

    fn cancel_blocked_vcpu_runs(&self) {
        use windows::Win32::System::Hypervisor::WHvCancelRunVirtualProcessor;

        for vcpu_index in 0..self.vcpu_count {
            let result = unsafe { WHvCancelRunVirtualProcessor(self.partition, vcpu_index, 0) };

            match result {
                Ok(()) => windows_irq_debug_log(format!("[IRQ] canceled_run vcpu={}", vcpu_index)),
                Err(e) => windows_irq_debug_log(format!(
                    "[IRQ] canceled_run_failed vcpu={} hr=0x{:x}",
                    vcpu_index,
                    e.code().0 as u32
                )),
            }
        }
    }

    fn replay_deferred_pic_interrupt(&self, irq_line: u32) -> Result<bool, devices::Error> {
        use windows::Win32::System::Hypervisor::{
            WHvGetVirtualProcessorRegisters, WHvX64RegisterRflags, WHvX64RegisterRip,
            WHV_REGISTER_NAME, WHV_REGISTER_VALUE,
        };

        let pending_vector = {
            let pending_interrupt = self.pending_interrupt.lock().unwrap();
            pending_interrupt.front().and_then(|pending| match *pending {
                PendingInterrupt::PicExtInt { irq, vector } => Some((irq, vector)),
                PendingInterrupt::PicFixed { irq, vector } => Some((irq, vector)),
            })
        };

        let Some((pending_irq, vector)) = pending_vector else {
            return Ok(false);
        };

        let reg_names: [WHV_REGISTER_NAME; 2] = [WHvX64RegisterRip, WHvX64RegisterRflags];
        let mut reg_values = [WHV_REGISTER_VALUE::default(); 2];

        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_mut_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to read WHPX vCPU state for deferred IRQ {} (pending IRQ {}) replay: {}",
                    irq_line, pending_irq, e
                )))
            })?;
        }

        let (rip, rflags) = unsafe { (reg_values[0].Reg64, reg_values[1].Reg64) };
        let interrupt_enabled = (rflags & (1 << 9)) != 0;

        if !interrupt_enabled {
            windows_irq_debug_log(format!(
                "[IRQ] deferred_wait irq={} pending_irq={} vector=0x{:02x} rip=0x{:016x} rflags=0x{:016x}",
                irq_line, pending_irq, vector, rip, rflags
            ));
            return Ok(false);
        }

        windows_irq_debug_log(format!(
            "[IRQ] deferred_replay irq={} pending_irq={} vector=0x{:02x} rip=0x{:016x} rflags=0x{:016x}",
            irq_line, pending_irq, vector, rip, rflags
        ));
        self.cancel_blocked_vcpu_runs();
        let _ = self.irq_pending_evt.write(1);
        Ok(true)
    }

    fn pending_interrupt_slots_busy(&self) -> Option<(bool, bool, u64)> {
        use windows::Win32::System::Hypervisor::{
            WHvGetVirtualProcessorRegisters, WHvRegisterInternalActivityState,
            WHvRegisterPendingEvent, WHvRegisterPendingInterruption, WHV_REGISTER_NAME,
            WHV_REGISTER_VALUE,
        };

        let reg_names: [WHV_REGISTER_NAME; 3] = [
            WHvRegisterPendingInterruption,
            WHvRegisterPendingEvent,
            WHvRegisterInternalActivityState,
        ];
        let mut reg_values = [WHV_REGISTER_VALUE::default(); 3];
        let result = unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_names.as_ptr(),
                reg_names.len() as u32,
                reg_values.as_mut_ptr(),
            )
        };

        match result {
            Ok(()) => {
                let pending_interrupt_busy =
                    unsafe { (reg_values[0].PendingInterruption.AsUINT64 & 1) != 0 };
                let pending_event_busy =
                    unsafe { (reg_values[1].ExtIntEvent.AsUINT128.Anonymous.Low64 & 1) != 0 };
                let internal_activity = unsafe { reg_values[2].InternalActivity.AsUINT64 };
                Some((pending_interrupt_busy, pending_event_busy, internal_activity))
            }
            Err(e) => {
                windows_irq_debug_log(format!(
                    "[IRQ] pending_slot_probe_failed hr=0x{:x}",
                    e.code().0 as u32
                ));
                None
            }
        }
    }

    fn should_cancel_halted_pending_event_duplicate() -> bool {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        count % 4 == 0
    }

    fn should_cancel_after_irq_delivery(&self) -> bool {
        if !windows_skip_cancel_for_halted_irq_delivery() {
            return true;
        }

        match self.pending_interrupt_slots_busy() {
            Some((_, _, internal_activity)) if (internal_activity & 0x2) != 0 => {
                windows_irq_debug_log(format!(
                    "[IRQ] skip_cancel_on_hlt internal_activity=0x{:x}",
                    internal_activity
                ));
                false
            }
            Some((_, _, internal_activity)) => {
                windows_irq_debug_log(format!(
                    "[IRQ] keep_cancel_not_hlt internal_activity=0x{:x}",
                    internal_activity
                ));
                true
            }
            None => true,
        }
    }

    fn kick_after_queue_update(&self, debug_label: &str) {
        if self.should_cancel_after_irq_delivery() {
            windows_irq_debug_log(format!("[IRQ] {} action=cancel", debug_label));
            self.cancel_blocked_vcpu_runs();
        } else {
            windows_irq_debug_log(format!("[IRQ] {} action=event-only", debug_label));
        }
        let _ = self.irq_pending_evt.write(1);
    }

    fn dump_vcpu_irq_state(&self, label: &str, irq_line: u32, route: &WhpxInterruptRoute) {
        use windows::Win32::System::Hypervisor::{
            WHvGetVirtualProcessorRegisters, WHvX64RegisterDeliverabilityNotifications,
            WHvRegisterInterruptState, WHvRegisterPendingEvent, WHvRegisterPendingInterruption,
            WHvX64RegisterApicBase, WHvX64RegisterApicLvtLint0, WHvX64RegisterApicLvtLint1,
            WHvX64RegisterApicSpurious, WHvX64RegisterApicTpr, WHvX64RegisterCr8,
            WHvX64RegisterRflags, WHvX64RegisterRip, WHV_REGISTER_NAME, WHV_REGISTER_VALUE,
        };

        let core_reg_names: [WHV_REGISTER_NAME; 6] = [
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
                0,
                core_reg_names.as_ptr(),
                core_reg_names.len() as u32,
                core_reg_values.as_mut_ptr(),
            ) {
                Ok(()) => {
                    let pending_event = core_reg_values[4].ExtIntEvent.AsUINT128.Anonymous;
                    windows_irq_debug_log(format!(
                        "[IRQSTATE] label={} irq={} route={:?} rip=0x{:016x} rflags=0x{:016x} interrupt_state=0x{:016x} pending_interrupt=0x{:016x} pending_event_hi=0x{:016x} pending_event_lo=0x{:016x} deliverability=0x{:016x}",
                        label,
                        irq_line,
                        route,
                        core_reg_values[0].Reg64,
                        core_reg_values[1].Reg64,
                        core_reg_values[2].InterruptState.AsUINT64,
                        core_reg_values[3].PendingInterruption.AsUINT64,
                        pending_event.High64,
                        pending_event.Low64,
                        core_reg_values[5].DeliverabilityNotifications.AsUINT64,
                    ));
                }
                Err(e) => windows_irq_debug_log(format!(
                    "[IRQSTATE] label={} irq={} route={:?} core_read_failed hr=0x{:x}",
                    label,
                    irq_line,
                    route,
                    e.code().0 as u32
                )),
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
                    0,
                    [reg_name].as_ptr(),
                    1,
                    value.as_mut_ptr(),
                ) {
                    Ok(()) => windows_irq_debug_log(format!(
                        "[IRQSTATE] label={} irq={} route={:?} {}=0x{:016x}",
                        label,
                        irq_line,
                        route,
                        name,
                        value[0].Reg64
                    )),
                    Err(e) => windows_irq_debug_log(format!(
                        "[IRQSTATE] label={} irq={} route={:?} {}_read_failed hr=0x{:x}",
                        label,
                        irq_line,
                        route,
                        name,
                        e.code().0 as u32
                    )),
                }
            }
        }
    }

    fn arm_interrupt_window_notification(
        &self,
        irq_line: u32,
        vector: u8,
        route: &WhpxInterruptRoute,
    ) -> Result<(), devices::Error> {
        use windows::Win32::System::Hypervisor::{
            WHvGetVirtualProcessorRegisters, WHvSetVirtualProcessorRegisters,
            WHvX64RegisterDeliverabilityNotifications, WHV_REGISTER_VALUE,
            WHV_X64_DELIVERABILITY_NOTIFICATIONS_REGISTER,
        };

        let priority = u64::from(vector >> 4) & 0xf;
        let notifications = WHV_X64_DELIVERABILITY_NOTIFICATIONS_REGISTER {
            AsUINT64: (1 << 1) | (priority << 2),
        };
        let reg_name = [WHvX64RegisterDeliverabilityNotifications];
        let reg_value = [WHV_REGISTER_VALUE {
            DeliverabilityNotifications: notifications,
        }];

        unsafe {
            WHvSetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_name.as_ptr(),
                1,
                reg_value.as_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to arm WHPX interrupt window for irq {} route {:?}: {}",
                    irq_line, route, e
                )))
            })?;

            let mut readback = [WHV_REGISTER_VALUE::default(); 1];
            match WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_name.as_ptr(),
                1,
                readback.as_mut_ptr(),
            ) {
                Ok(()) => windows_irq_debug_log(format!(
                    "[IRQ] interrupt_window_armed irq={} route={:?} vector=0x{:02x} readback=0x{:016x}",
                    irq_line,
                    route,
                    vector,
                    readback[0].DeliverabilityNotifications.AsUINT64
                )),
                Err(e) => windows_irq_debug_log(format!(
                    "[IRQ] interrupt_window_arm_readback_failed irq={} route={:?} vector=0x{:02x} hr=0x{:x}",
                    irq_line,
                    route,
                    vector,
                    e.code().0 as u32
                )),
            }
        }

        Ok(())
    }

    fn inject_pending_interruption_register(
        &self,
        irq_line: u32,
        irq: u8,
        vector: u8,
    ) -> Result<bool, devices::Error> {
        use windows::Win32::System::Hypervisor::{
            WHvGetVirtualProcessorRegisters, WHvRegisterInternalActivityState,
            WHvRegisterPendingInterruption, WHvSetVirtualProcessorRegisters,
            WHvX64RegisterRflags, WHvX64RegisterRip, WHV_REGISTER_NAME, WHV_REGISTER_VALUE,
        };

        let reg_name = [WHvRegisterPendingInterruption];
        let mut current = [WHV_REGISTER_VALUE::default(); 1];
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_name.as_ptr(),
                1,
                current.as_mut_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to read WHPX pending interruption state for irq {}: {}",
                    irq_line, e
                )))
            })?;
        }

        let current_bits = unsafe { current[0].PendingInterruption.AsUINT64 };
        if (current_bits & 1) != 0 {
            windows_irq_debug_log(format!(
                "[IRQ] pending_interrupt_busy irq={} pic_irq={} vector=0x{:02x} bits=0x{:016x}",
                irq_line, irq, vector, current_bits
            ));
            return Ok(true);
        }

        let vcpu_reg_names: [WHV_REGISTER_NAME; 3] = [
            WHvX64RegisterRip,
            WHvX64RegisterRflags,
            WHvRegisterInternalActivityState,
        ];
        let mut vcpu_regs = [WHV_REGISTER_VALUE::default(); 3];
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                vcpu_reg_names.as_ptr(),
                vcpu_reg_names.len() as u32,
                vcpu_regs.as_mut_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to read WHPX vCPU state for pending interruption on irq {}: {}",
                    irq_line, e
                )))
            })?;
        }

        let rip = unsafe { vcpu_regs[0].Reg64 };
        let rflags = unsafe { vcpu_regs[1].Reg64 };
        let internal_activity = unsafe { vcpu_regs[2].InternalActivity.AsUINT64 };
        if (rflags & (1 << 9)) == 0 {
            windows_irq_debug_log(format!(
                "[IRQ] pending_interrupt_wait_if0 irq={} pic_irq={} vector=0x{:02x} rip=0x{:016x} rflags=0x{:016x}",
                irq_line, irq, vector, rip, rflags
            ));
            return Ok(false);
        }
        if (internal_activity & 0x2) == 0 {
            windows_irq_debug_log(format!(
                "[IRQ] pending_interrupt_wait_hlt irq={} pic_irq={} vector=0x{:02x} rip=0x{:016x} rflags=0x{:016x} internal_activity=0x{:016x}",
                irq_line, irq, vector, rip, rflags, internal_activity
            ));
            return Ok(false);
        }

        let mut reg_value = [WHV_REGISTER_VALUE::default(); 1];
        reg_value[0].PendingInterruption.AsUINT64 = 1 | (u64::from(vector) << 16);
        unsafe {
            WHvSetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_name.as_ptr(),
                1,
                reg_value.as_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to set WHPX pending interruption for irq {}: {}",
                    irq_line, e
                )))
            })?;
        }

        let mut post = [WHV_REGISTER_VALUE::default(); 1];
        unsafe {
            let _ = WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_name.as_ptr(),
                1,
                post.as_mut_ptr(),
            );
        }
        let post_bits = unsafe { post[0].PendingInterruption.AsUINT64 };
        windows_irq_debug_log(format!(
            "[IRQ] pending_interrupt_set irq={} pic_irq={} vector=0x{:02x} post=0x{:016x}",
            irq_line, irq, vector, post_bits
        ));
        if self.should_cancel_after_irq_delivery() {
            self.cancel_blocked_vcpu_runs();
        }
        let _ = self.irq_pending_evt.write(1);
        Ok(true)
    }

    fn request_pic_fixed_interrupt_direct(
        &self,
        irq_line: u32,
        irq: u8,
        vector: u8,
    ) -> Result<bool, devices::Error> {
        use windows::Win32::System::Hypervisor::{
            WHvRequestInterrupt, WHvX64InterruptDestinationModePhysical,
            WHvX64InterruptTriggerModeEdge, WHvX64InterruptTypeFixed, WHV_INTERRUPT_CONTROL,
        };

        let interrupt = WHV_INTERRUPT_CONTROL {
            _bitfield: make_whpx_interrupt_control_bitfield(
                WHvX64InterruptTypeFixed,
                WHvX64InterruptDestinationModePhysical,
                WHvX64InterruptTriggerModeEdge,
            ),
            Destination: 0,
            Vector: u32::from(vector),
        };
        windows_irq_debug_log(format!(
            "[IRQ] builder_direct_request_begin irq={} pic_irq={} vector=0x{:02x} dest=0x{:x} type=0x{:x}",
            irq_line,
            irq,
            vector,
            interrupt.Destination,
            interrupt._bitfield & 0xff
        ));

        unsafe {
            WHvRequestInterrupt(
                self.partition,
                &interrupt,
                std::mem::size_of::<WHV_INTERRUPT_CONTROL>() as u32,
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed direct builder WHPX fixed interrupt request for irq {} vector 0x{:02x}: {}",
                    irq_line, vector, e
                )))
            })?;
        }

        let skipped_ack = windows_skip_pic_fixed_ack();
        if !skipped_ack {
            devices::legacy::windows_pic_stub::acknowledge_irq(irq);
        }

        windows_irq_debug_log(format!(
            "[IRQ] builder_direct_request_ok irq={} pic_irq={} vector=0x{:02x} skipped_ack={}",
            irq_line, irq, vector, skipped_ack
        ));
        if self.should_cancel_after_irq_delivery() {
            self.cancel_blocked_vcpu_runs();
        }
        let _ = self.irq_pending_evt.write(1);
        Ok(true)
    }

    fn inject_pending_event_extint_register(
        &self,
        irq_line: u32,
        irq: u8,
        vector: u8,
    ) -> Result<bool, devices::Error> {
        use windows::Win32::System::Hypervisor::{
            WHvGetVirtualProcessorRegisters, WHvRegisterInternalActivityState,
            WHvRegisterPendingEvent, WHvRegisterPendingInterruption,
            WHvSetVirtualProcessorRegisters, WHvX64PendingEventExtInt, WHvX64RegisterRflags,
            WHvX64RegisterRip, WHV_REGISTER_NAME, WHV_REGISTER_VALUE,
            WHV_X64_PENDING_EXT_INT_EVENT,
        };

        windows_irq_debug_log(format!(
            "[IRQ] pending_event_probe_begin irq={} pic_irq={} vector=0x{:02x}",
            irq_line, irq, vector
        ));
        let pending_reg_names: [WHV_REGISTER_NAME; 2] =
            [WHvRegisterPendingInterruption, WHvRegisterPendingEvent];
        let mut pending_regs = [WHV_REGISTER_VALUE::default(); 2];
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                pending_reg_names.as_ptr(),
                pending_reg_names.len() as u32,
                pending_regs.as_mut_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to read WHPX pending event state for irq {}: {}",
                    irq_line, e
                )))
            })?;
        }

        let pending_interrupt_bits = unsafe { pending_regs[0].PendingInterruption.AsUINT64 };
        let pending_event_bits = unsafe { pending_regs[1].ExtIntEvent.AsUINT128.Anonymous.Low64 };
        windows_irq_debug_log(format!(
            "[IRQ] pending_event_probe_pending irq={} pic_irq={} vector=0x{:02x} pending_interrupt=0x{:016x} pending_event_lo=0x{:016x}",
            irq_line, irq, vector, pending_interrupt_bits, pending_event_bits
        ));
        if (pending_interrupt_bits & 1) != 0 || (pending_event_bits & 1) != 0 {
            windows_irq_debug_log(format!(
                "[IRQ] pending_event_busy irq={} pic_irq={} vector=0x{:02x} pending_interrupt=0x{:016x} pending_event_lo=0x{:016x}",
                irq_line, irq, vector, pending_interrupt_bits, pending_event_bits
            ));
            return Ok(true);
        }

        let vcpu_reg_names: [WHV_REGISTER_NAME; 3] = [
            WHvX64RegisterRip,
            WHvX64RegisterRflags,
            WHvRegisterInternalActivityState,
        ];
        let mut vcpu_regs = [WHV_REGISTER_VALUE::default(); 3];
        unsafe {
            WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                vcpu_reg_names.as_ptr(),
                vcpu_reg_names.len() as u32,
                vcpu_regs.as_mut_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to read WHPX vCPU state for pending event on irq {}: {}",
                    irq_line, e
                )))
            })?;
        }

        let rip = unsafe { vcpu_regs[0].Reg64 };
        let rflags = unsafe { vcpu_regs[1].Reg64 };
        let internal_activity = unsafe { vcpu_regs[2].InternalActivity.AsUINT64 };
        windows_irq_debug_log(format!(
            "[IRQ] pending_event_probe_vcpu irq={} pic_irq={} vector=0x{:02x} rip=0x{:016x} rflags=0x{:016x} internal_activity=0x{:016x}",
            irq_line, irq, vector, rip, rflags, internal_activity
        ));
        if (rflags & (1 << 9)) == 0 {
            windows_irq_debug_log(format!(
                "[IRQ] pending_event_wait_if0 irq={} pic_irq={} vector=0x{:02x} rip=0x{:016x} rflags=0x{:016x}",
                irq_line, irq, vector, rip, rflags
            ));
            return Ok(false);
        }
        if (internal_activity & 0x2) == 0 {
            windows_irq_debug_log(format!(
                "[IRQ] pending_event_wait_hlt irq={} pic_irq={} vector=0x{:02x} rip=0x{:016x} rflags=0x{:016x} internal_activity=0x{:016x}",
                irq_line, irq, vector, rip, rflags, internal_activity
            ));
            return Ok(false);
        }

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
        windows_irq_debug_log(format!(
            "[IRQ] pending_event_write_begin irq={} pic_irq={} vector=0x{:02x} rip=0x{:016x}",
            irq_line, irq, vector, rip
        ));
        unsafe {
            WHvSetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_name.as_ptr(),
                1,
                reg_value.as_ptr(),
            )
            .map_err(|e| {
                devices::Error::FailedSignalingUsedQueue(io::Error::other(format!(
                    "Failed to set WHPX pending event for irq {}: {}",
                    irq_line, e
                )))
            })?;
        }

        let mut post = [WHV_REGISTER_VALUE::default(); 1];
        unsafe {
            let _ = WHvGetVirtualProcessorRegisters(
                self.partition,
                0,
                reg_name.as_ptr(),
                1,
                post.as_mut_ptr(),
            );
        }
        let post_bits = unsafe { post[0].ExtIntEvent.AsUINT128.Anonymous.Low64 };
        windows_irq_debug_log(format!(
            "[IRQ] pending_event_set irq={} pic_irq={} vector=0x{:02x} post_lo=0x{:016x}",
            irq_line, irq, vector, post_bits
        ));
        if self.should_cancel_after_irq_delivery() {
            self.cancel_blocked_vcpu_runs();
        }
        let _ = self.irq_pending_evt.write(1);
        Ok(true)
    }

}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
impl devices::BusDevice for WhpxIrqChip {}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
impl IrqChipT for WhpxIrqChip {
    fn get_mmio_addr(&self) -> u64 {
        0
    }

    fn get_mmio_size(&self) -> u64 {
        0
    }

    fn set_irq(
        &self,
        irq_line: Option<u32>,
        _interrupt_evt: Option<&EventFd>,
    ) -> Result<(), devices::Error> {
        use std::sync::atomic::{AtomicU64, Ordering};
        use windows::Win32::System::Hypervisor::{
            WHvRequestInterrupt, WHvX64InterruptDestinationModeLogical,
            WHvX64InterruptDestinationModePhysical,
            WHvX64InterruptTriggerModeEdge, WHvX64InterruptTypeFixed, WHV_INTERRUPT_CONTROL,
        };

        let irq_line = irq_line.ok_or_else(|| {
            devices::Error::FailedSignalingUsedQueue(io::Error::new(
                io::ErrorKind::NotFound,
                "Missing IRQ line for WHPX interrupt injection",
            ))
        })?;

        if irq_line < 16 {
            devices::legacy::windows_pic_stub::raise_irq(irq_line as u8);
        }

        let route = Self::resolve_interrupt_route(irq_line);

        if irq_line == 0 {
            static WAIT_COUNT: AtomicU64 = AtomicU64::new(0);
            static IOAPIC_COUNT: AtomicU64 = AtomicU64::new(0);
            match &route {
                WhpxInterruptRoute::Unresolved => {
                    let n = WAIT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    if n <= 20 || n % 100 == 0 {
                        windows_irq_debug_log(format!("[IRQ0] unresolved count={}", n));
                    }
                }
                WhpxInterruptRoute::IoApic {
                    vector,
                    destination,
                    ..
                } => {
                    let n = IOAPIC_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    if n <= 20 || n % 100 == 0 {
                        windows_irq_debug_log(format!(
                            "[IRQ0] ioapic vector=0x{:02x} dest=0x{:x} count={}",
                            vector, destination, n
                        ));
                    }
                }
                WhpxInterruptRoute::PicExtIntRequest { irq, vector } => {
                    windows_irq_debug_log(format!(
                        "[IRQ0] pic-extint-req irq={} vector=0x{:02x}",
                        irq, vector
                    ));
                }
                _ => {}
            }
        }

        if let WhpxInterruptRoute::Unresolved = route {
            if irq_line == 0 && self.replay_deferred_pic_interrupt(irq_line)? {
                windows_irq_debug_log(format!(
                    "[IRQ] unresolved_replayed irq={} route={}",
                    irq_line, "pic-deferred"
                ));
            }
            return Ok(());
        }

        let request_kind = match &route {
            WhpxInterruptRoute::IoApic { .. } => "ioapic",
            WhpxInterruptRoute::PicExtIntRequest { .. } => "pic-extint",
            WhpxInterruptRoute::FallbackFixed { .. } => "fallback-fixed",
            WhpxInterruptRoute::Unresolved => "unresolved",
        };

        if let WhpxInterruptRoute::PicExtIntRequest { irq, vector } = route {
            if irq == 0 && windows_force_pic_irq0_fixed() {
                windows_irq_debug_log(format!(
                    "[IRQ] pic_fixed_strategy irq={} pic_irq={} route={} vector=0x{:02x} builder_direct_enabled={} builder_direct_mode={:?} builder_request_interrupt={}",
                    irq_line,
                    irq,
                    request_kind,
                    vector,
                    windows_pic_fixed_pending_interruption(),
                    windows_pic_fixed_builder_direct_mode(),
                    windows_pic_fixed_builder_request_interrupt()
                ));
                self.dump_vcpu_irq_state("pic-fixed-before", irq_line, &route);
                if windows_pic_fixed_builder_request_interrupt()
                    && self.request_pic_fixed_interrupt_direct(irq_line, irq, vector)?
                {
                    windows_irq_debug_log(format!(
                        "[IRQ] direct_builder_request irq={} pic_irq={} route={} vector=0x{:02x}",
                        irq_line, irq, request_kind, vector
                    ));
                    return Ok(());
                }
                let direct_builder_injected = if windows_pic_fixed_pending_interruption() {
                    match windows_pic_fixed_builder_direct_mode() {
                        WindowsPicFixedBuilderDirectMode::PendingInterruption => {
                            self.inject_pending_interruption_register(irq_line, irq, vector)?
                        }
                        WindowsPicFixedBuilderDirectMode::PendingEventExtInt => false,
                    }
                } else {
                    false
                };
                if direct_builder_injected {
                    windows_irq_debug_log(format!(
                        "[IRQ] direct_builder_inject irq={} pic_irq={} route={} vector=0x{:02x} mode={:?}",
                        irq_line,
                        irq,
                        request_kind,
                        vector,
                        windows_pic_fixed_builder_direct_mode()
                    ));
                    return Ok(());
                }
                {
                    let mut pending_interrupt = self.pending_interrupt.lock().unwrap();
                    let already_queued = pending_interrupt.iter().any(|pending| match *pending {
                        PendingInterrupt::PicExtInt { irq: pending_irq, .. }
                        | PendingInterrupt::PicFixed { irq: pending_irq, .. } => pending_irq == irq,
                    });
                    if already_queued {
                        windows_irq_debug_log(format!(
                            "[IRQ] queue_skip_duplicate ptr=0x{:x} irq={} pic_irq={} mode=fixed depth={} front={:?}",
                            Arc::as_ptr(&self.pending_interrupt) as usize,
                            irq_line,
                            irq,
                            pending_interrupt.len(),
                            pending_interrupt.front().copied()
                        ));
                        drop(pending_interrupt);
                        match self.pending_interrupt_slots_busy() {
                            Some((pending_interrupt_busy, pending_event_busy, internal_activity))
                                if pending_interrupt_busy || pending_event_busy =>
                            {
                                if pending_event_busy
                                    && (internal_activity & 0x2) != 0
                                    && Self::should_cancel_halted_pending_event_duplicate()
                                {
                                    windows_irq_debug_log(format!(
                                        "[IRQ] duplicate_halted_kick ptr=0x{:x} irq={} pic_irq={} mode=fixed pending_interrupt_busy={} pending_event_busy={} internal_activity=0x{:x}",
                                        Arc::as_ptr(&self.pending_interrupt) as usize,
                                        irq_line,
                                        irq,
                                        pending_interrupt_busy,
                                        pending_event_busy,
                                        internal_activity
                                    ));
                                    self.kick_after_queue_update(
                                        "duplicate_halted_kick mode=fixed",
                                    );
                                } else {
                                    windows_irq_debug_log(format!(
                                        "[IRQ] duplicate_no_cancel ptr=0x{:x} irq={} pic_irq={} mode=fixed pending_interrupt_busy={} pending_event_busy={} internal_activity=0x{:x}",
                                        Arc::as_ptr(&self.pending_interrupt) as usize,
                                        irq_line,
                                        irq,
                                        pending_interrupt_busy,
                                        pending_event_busy,
                                        internal_activity
                                    ));
                                }
                            }
                            _ => {
                                windows_irq_debug_log(format!(
                                    "[IRQ] duplicate_cancel ptr=0x{:x} irq={} pic_irq={} mode=fixed",
                                    Arc::as_ptr(&self.pending_interrupt) as usize,
                                    irq_line,
                                    irq
                                ));
                                self.kick_after_queue_update("duplicate_cancel mode=fixed");
                            }
                        }
                        return Ok(());
                    }
                    pending_interrupt.push_back(PendingInterrupt::PicFixed { irq, vector });
                    windows_irq_debug_log(format!(
                        "[IRQ] queue_push ptr=0x{:x} irq={} pic_irq={} mode=fixed depth={} front={:?}",
                        Arc::as_ptr(&self.pending_interrupt) as usize,
                        irq_line,
                        irq,
                        pending_interrupt.len(),
                        pending_interrupt.front().copied()
                    ));
                }
                windows_irq_debug_log(format!(
                    "[IRQ] queued irq={} pic_irq={} route={} vector=0x{:02x} delivery=vcpu-fixed",
                    irq_line, irq, request_kind, vector
                ));
                self.kick_after_queue_update("queue_push_complete mode=fixed");
                return Ok(());
            } else {
                self.dump_vcpu_irq_state("pic-extint-before", irq_line, &route);
                {
                    let mut pending_interrupt = self.pending_interrupt.lock().unwrap();
                    let already_queued = pending_interrupt.iter().any(|pending| match *pending {
                        PendingInterrupt::PicExtInt { irq: pending_irq, .. }
                        | PendingInterrupt::PicFixed { irq: pending_irq, .. } => pending_irq == irq,
                    });
                    if already_queued {
                        windows_irq_debug_log(format!(
                            "[IRQ] queue_skip_duplicate ptr=0x{:x} irq={} pic_irq={} depth={} front={:?}",
                            Arc::as_ptr(&self.pending_interrupt) as usize,
                            irq_line,
                            irq,
                            pending_interrupt.len(),
                            pending_interrupt.front().copied()
                        ));
                        drop(pending_interrupt);
                        match self.pending_interrupt_slots_busy() {
                            Some((pending_interrupt_busy, pending_event_busy, internal_activity))
                                if pending_interrupt_busy || pending_event_busy =>
                            {
                                if pending_event_busy
                                    && (internal_activity & 0x2) != 0
                                    && Self::should_cancel_halted_pending_event_duplicate()
                                {
                                    windows_irq_debug_log(format!(
                                        "[IRQ] duplicate_halted_kick ptr=0x{:x} irq={} pic_irq={} pending_interrupt_busy={} pending_event_busy={} internal_activity=0x{:x}",
                                        Arc::as_ptr(&self.pending_interrupt) as usize,
                                        irq_line,
                                        irq,
                                        pending_interrupt_busy,
                                        pending_event_busy,
                                        internal_activity
                                    ));
                                    self.kick_after_queue_update("duplicate_halted_kick");
                                } else {
                                    windows_irq_debug_log(format!(
                                        "[IRQ] duplicate_no_cancel ptr=0x{:x} irq={} pic_irq={} pending_interrupt_busy={} pending_event_busy={} internal_activity=0x{:x}",
                                        Arc::as_ptr(&self.pending_interrupt) as usize,
                                        irq_line,
                                        irq,
                                        pending_interrupt_busy,
                                        pending_event_busy,
                                        internal_activity
                                    ));
                                }
                            }
                            _ => {
                                windows_irq_debug_log(format!(
                                    "[IRQ] duplicate_cancel ptr=0x{:x} irq={} pic_irq={}",
                                    Arc::as_ptr(&self.pending_interrupt) as usize,
                                    irq_line,
                                    irq
                                ));
                                self.kick_after_queue_update("duplicate_cancel");
                            }
                        }
                        return Ok(());
                    }
                    pending_interrupt.push_back(PendingInterrupt::PicExtInt { irq, vector });
                    windows_irq_debug_log(format!(
                        "[IRQ] queue_push ptr=0x{:x} irq={} pic_irq={} depth={} front={:?}",
                        Arc::as_ptr(&self.pending_interrupt) as usize,
                        irq_line,
                        irq,
                        pending_interrupt.len(),
                        pending_interrupt.front().copied()
                    ));
                }
                windows_irq_debug_log(format!(
                    "[IRQ] queued irq={} pic_irq={} route={} vector=0x{:02x} delivery=cancel-exit",
                    irq_line,
                    irq,
                    request_kind,
                    vector
                ));
                self.kick_after_queue_update("queue_push_complete");
                return Ok(());
            }
        }

        let interrupt = match route {
            WhpxInterruptRoute::IoApic {
                vector,
                destination_mode,
                trigger_mode,
                interrupt_type,
                destination,
            } => {
                let (effective_destination_mode, effective_destination) = if self.vcpu_count == 1
                    && destination_mode == WHvX64InterruptDestinationModeLogical
                {
                    windows_irq_debug_log(format!(
                        "[IRQ] ioapic_single_vcpu_normalized irq={} vector=0x{:02x} orig_dest_mode=logical orig_dest=0x{:x} new_dest_mode=physical new_dest=0x0",
                        irq_line, vector, destination
                    ));
                    (WHvX64InterruptDestinationModePhysical, 0)
                } else {
                    (destination_mode, destination)
                };

                WHV_INTERRUPT_CONTROL {
                    _bitfield: make_whpx_interrupt_control_bitfield(
                        interrupt_type,
                        effective_destination_mode,
                        trigger_mode,
                    ),
                    Destination: effective_destination,
                    Vector: vector,
                }
            }
            WhpxInterruptRoute::PicExtIntRequest { vector, .. } => WHV_INTERRUPT_CONTROL {
                // Fallback if pending-event ExtINT injection is rejected.
                _bitfield: make_whpx_interrupt_control_bitfield(
                    WHvX64InterruptTypeFixed,
                    WHvX64InterruptDestinationModePhysical,
                    WHvX64InterruptTriggerModeEdge,
                ),
                Destination: 0,
                Vector: u32::from(vector),
            },
            WhpxInterruptRoute::FallbackFixed { vector } => WHV_INTERRUPT_CONTROL {
                _bitfield: make_whpx_interrupt_control_bitfield(
                    WHvX64InterruptTypeFixed,
                    WHvX64InterruptDestinationModePhysical,
                    WHvX64InterruptTriggerModeEdge,
                ),
                Destination: 0,
                Vector: vector,
            },
            WhpxInterruptRoute::Unresolved => unreachable!(),
        };

        unsafe {
            windows_irq_debug_log(format!(
                "[IRQ] inject irq={} route={} vector=0x{:02x} dest=0x{:x} type=0x{:x} dest_mode=0x{:x} trigger=0x{:x}",
                irq_line,
                request_kind,
                interrupt.Vector,
                interrupt.Destination,
                interrupt._bitfield & 0xff,
                (interrupt._bitfield >> 8) & 0xf,
                (interrupt._bitfield >> 12) & 0xf
            ));
            let result = WHvRequestInterrupt(
                self.partition,
                &interrupt,
                std::mem::size_of::<WHV_INTERRUPT_CONTROL>() as u32,
            );

            if let Err(e) = result {
                windows_irq_debug_log(format!(
                    "[IRQ] request_failed irq={} route={} vector=0x{:02x} hr=0x{:x}",
                    irq_line,
                    request_kind,
                    interrupt.Vector,
                    e.code().0 as u32
                ));
                log::error!(
                    "❌ WHvRequestInterrupt FAILED for IRQ {} (vector {}): {}",
                    irq_line,
                    interrupt.Vector,
                    e
                );
                return Err(devices::Error::FailedSignalingUsedQueue(io::Error::other(
                    format!(
                        "WHPX interrupt injection failed for irq {} (vector {}): {}",
                        irq_line, interrupt.Vector, e
                    ),
                )));
            }
        }
        windows_irq_debug_log(format!(
            "[IRQ] request_ok irq={} route={} vector=0x{:02x}",
            irq_line, request_kind, interrupt.Vector
        ));

        log::debug!(
            "✅ WHvRequestInterrupt succeeded for IRQ {} (vector {})",
            irq_line,
            interrupt.Vector
        );
        if let WhpxInterruptRoute::PicExtIntRequest { irq, .. } = route {
            devices::legacy::windows_pic_stub::acknowledge_irq(irq);
        }
        if self.should_cancel_after_irq_delivery() {
            self.cancel_blocked_vcpu_runs();
        }
        windows_irq_debug_log(format!(
            "[IRQ] request irq={} route={} vector=0x{:02x} dest=0x{:x} type=0x{:x}",
            irq_line,
            request_kind,
            interrupt.Vector,
            interrupt.Destination,
            interrupt._bitfield & 0xff
        ));

        // Signal the vCPU thread so it can re-enter WHvRunVirtualProcessor and
        // deliver the queued interrupt if the guest is currently in HLT state.
        let _ = self.irq_pending_evt.write(1);

        Ok(())
    }
}

/// Errors associated with starting the instance.
#[derive(Debug)]
pub enum StartMicrovmError {
    /// Unable to attach block device to Vmm.
    AttachBlockDevice(io::Error),
    #[cfg(target_os = "macos")]
    /// Failed to create HVF in-kernel IrqChip.
    CreateHvfIrqChip(hvf::Error),
    #[cfg(target_os = "linux")]
    /// Failed to create KVM in-kernel IrqChip.
    CreateKvmIrqChip(kvm_ioctls::Error),
    /// Failed to create a `RateLimiter` object.
    CreateRateLimiter(io::Error),
    /// Cannot open the file containing the kernel code.
    ElfOpenKernel(io::Error),
    /// Cannot load the kernel into the VM.
    ElfLoadKernel(linux_loader::loader::Error),
    /// The firmware can't be loaded into the provided memory address.
    FirmwareInvalidAddress(vm_memory::GuestMemoryError),
    /// Cannot read firmware contents from file.
    FirmwareRead(io::Error),
    /// Memory regions are overlapping or mmap fails.
    GuestMemoryMmap(vm_memory::Error),
    /// Cannot create/size the snapshot guest-RAM backing file.
    SnapshotMemFile(io::Error),
    /// The BZIP2 decoder couldn't decompress the kernel.
    ImageBz2Decoder(io::Error),
    /// Cannot find compressed kernel in file.
    ImageBz2Invalid,
    /// Cannot load the kernel from the uncompressed ELF data.
    ImageBz2LoadKernel(linux_loader::loader::Error),
    /// Cannot open the file containing the kernel code.
    ImageBz2OpenKernel(io::Error),
    /// The GZIP decoder couldn't decompress the kernel.
    ImageGzDecoder(io::Error),
    /// Cannot find compressed kernel in file.
    ImageGzInvalid,
    /// Cannot load the kernel from the uncompressed ELF data.
    ImageGzLoadKernel(linux_loader::loader::Error),
    /// Cannot open the file containing the kernel code.
    ImageGzOpenKernel(io::Error),
    /// The ZSTD decoder couldn't decompress the kernel.
    ImageZstdDecoder(io::Error),
    /// Cannot find compressed kernel in file.
    ImageZstdInvalid,
    /// Cannot load the kernel from the uncompressed ELF data.
    ImageZstdLoadKernel(linux_loader::loader::Error),
    /// Cannot open the file containing the kernel code.
    ImageZstdOpenKernel(io::Error),
    /// Cannot load initrd due to an invalid memory configuration.
    InitrdLoad,
    /// Cannot load initrd due to an invalid image.
    InitrdRead(io::Error),
    /// Internal error encountered while starting a microVM.
    Internal(Error),
    /// Cannot inject the kernel into the guest memory due to a problem with the bundle.
    InvalidKernelBundle(vm_memory::mmap::MmapRegionError),
    /// The kernel command line is invalid.
    KernelCmdline(String),
    /// The kernel doesn't fit into the microVM memory.
    KernelDoesNotFit(u64, usize),
    /// The supplied kernel format is not supported.
    KernelFormatUnsupported,
    /// Cannot load command line string.
    LoadCommandline(kernel::cmdline::Error),
    /// The start command was issued more than once.
    MicroVMAlreadyRunning,
    /// Cannot start the VM because the kernel was not configured.
    MissingKernelConfig,
    /// Cannot start the VM because the size of the guest memory  was not specified.
    MissingMemSizeConfig,
    /// The net device configuration is missing the tap device.
    NetDeviceNotConfigured,
    /// Cannot open the block device backing file.
    OpenBlockDevice(io::Error),
    /// Cannot open console output file.
    OpenConsoleFile(io::Error),
    /// The GZIP decoder couldn't decompress the kernel.
    PeGzDecoder(io::Error),
    /// Cannot open the file containing the kernel code.
    PeGzOpenKernel(io::Error),
    /// Cannot find compressed kernel in file.
    PeGzInvalid,
    /// Cannot open the file containing the kernel code.
    RawOpenKernel(io::Error),
    /// Cannot initialize a MMIO Balloon device or add a device to the MMIO Bus.
    RegisterBalloonDevice(device_manager::mmio::Error),
    /// Cannot initialize a MMIO Block Device or add a device to the MMIO Bus.
    RegisterBlockDevice(device_manager::mmio::Error),
    /// Cannot register an EventHandler.
    RegisterEvent(EventManagerError),
    /// Cannot initialize a MMIO Fs Device or add ad device to the MMIO Bus.
    RegisterFsDevice(device_manager::mmio::Error),
    // Cannot initialize a MMIO Fs Device or add ad device to the MMIO Bus.
    RegisterConsoleDevice(device_manager::mmio::Error),
    /// Cannot register SIGWINCH event file descriptor.
    #[cfg(target_os = "linux")]
    RegisterFsSigwinch(kvm_ioctls::Error),
    /// Cannot initialize a MMIO Gpu device or add a device to the MMIO Bus.
    RegisterGpuDevice(device_manager::mmio::Error),
    /// Cannot initialize a MMIO Input device or add a device to the MMIO Bus.
    RegisterInputDevice(device_manager::mmio::Error),
    /// Cannot initialize a MMIO Network Device or add a device to the MMIO Bus.
    RegisterNetDevice(device_manager::mmio::Error),
    /// Cannot initialize a MMIO Rng device or add a device to the MMIO Bus.
    RegisterRngDevice(device_manager::mmio::Error),
    /// Cannot initialize a MMIO Snd device or add a device to the MMIO Bus.
    RegisterSndDevice(device_manager::mmio::Error),
    /// Cannot initialize a MMIO Vsock Device or add a device to the MMIO Bus.
    RegisterVsockDevice(device_manager::mmio::Error),
    /// Cannot restore VM or vCPU KVM state from a snapshot.
    RestoreState(VstateError),
    /// Cannot attest the VM in the Secure Virtualization context.
    SecureVirtAttest(VstateError),
    /// Cannot initialize the Secure Virtualization backend.
    SecureVirtPrepare(VstateError),
    /// Error configuring an SHM region.
    ShmConfig(device_manager::shm::Error),
    /// Error creating an SHM region.
    ShmCreate(device_manager::shm::Error),
    /// Error obtaining the host address of an SHM region.
    ShmHostAddr(vm_memory::GuestMemoryError),
    /// The TEE specified is not supported.
    InvalidTee,
}

/// It's convenient to automatically convert `kernel::cmdline::Error`s
/// to `StartMicrovmError`s.
impl std::convert::From<kernel::cmdline::Error> for StartMicrovmError {
    fn from(e: kernel::cmdline::Error) -> StartMicrovmError {
        StartMicrovmError::KernelCmdline(e.to_string())
    }
}

impl Display for StartMicrovmError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        use self::StartMicrovmError::*;
        match *self {
            AttachBlockDevice(ref err) => {
                write!(f, "Unable to attach block device to Vmm. Error: {err}")
            }
            #[cfg(target_os = "macos")]
            CreateHvfIrqChip(ref err) => {
                write!(f, "Cannot create HVF in-kernel IrqChip: {err}")
            }
            #[cfg(target_os = "linux")]
            CreateKvmIrqChip(ref err) => {
                write!(f, "Cannot create KVM in-kernel IrqChip: {err}")
            }
            CreateRateLimiter(ref err) => write!(f, "Cannot create RateLimiter: {err}"),
            ElfOpenKernel(ref err) => {
                write!(f, "Cannot open the file containing the kernel code: {err}")
            }
            ElfLoadKernel(ref err) => {
                write!(f, "Cannot load the kernel into the VM: {err}")
            }
            FirmwareInvalidAddress(ref err) => {
                write!(
                    f,
                    "The firmware can't be loaded into the guest memory: {err}"
                )
            }
            FirmwareRead(ref err) => {
                write!(f, "Cannot read firmware contents from file: {err}")
            }
            SnapshotMemFile(ref err) => {
                write!(f, "Cannot create snapshot guest-RAM backing file: {err}")
            }
            GuestMemoryMmap(ref err) => {
                // Remove imbricated quotes from error message.
                let mut err_msg = format!("{err:?}");
                err_msg = err_msg.replace('\"', "");
                write!(f, "Invalid Memory Configuration: {err_msg}")
            }
            ImageBz2Decoder(ref err) => {
                write!(f, "The BZIP2 decoder couldn't decompress the kernel. {err}")
            }
            ImageBz2Invalid => {
                write!(f, "Cannot find compressed kernel in file.")
            }
            ImageBz2LoadKernel(ref err) => {
                write!(
                    f,
                    "Cannot load the kernel from the uncompressed ELF data. {err}"
                )
            }
            ImageBz2OpenKernel(ref err) => {
                write!(f, "Cannot open the file containing the kernel code. {err}")
            }
            ImageGzDecoder(ref err) => {
                write!(f, "The GZIP decoder couldn't decompress the kernel. {err}")
            }
            ImageGzInvalid => {
                write!(f, "Cannot find compressed kernel in file.")
            }
            ImageGzLoadKernel(ref err) => {
                write!(
                    f,
                    "Cannot load the kernel from the uncompressed ELF data. {err}"
                )
            }
            ImageGzOpenKernel(ref err) => {
                write!(f, "Cannot open the file containing the kernel code. {err}")
            }
            ImageZstdDecoder(ref err) => {
                write!(f, "The ZSTD decoder couldn't decompress the kernel. {err}")
            }
            ImageZstdInvalid => {
                write!(f, "Cannot find compressed kernel in file.")
            }
            ImageZstdLoadKernel(ref err) => {
                write!(
                    f,
                    "Cannot load the kernel from the uncompressed ELF data. {err}"
                )
            }
            ImageZstdOpenKernel(ref err) => {
                write!(f, "Cannot open the file containing the kernel code. {err}")
            }
            InitrdLoad => write!(
                f,
                "Cannot load initrd due to an invalid memory configuration."
            ),
            InitrdRead(ref err) => write!(f, "Cannot load initrd due to an invalid image: {err}"),
            Internal(ref err) => write!(f, "Internal error while starting microVM: {err:?}"),
            InvalidKernelBundle(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(
                    f,
                    "Cannot inject the kernel into the guest memory due to a problem with the \
                     bundle. {err_msg}"
                )
            }
            KernelCmdline(ref err) => write!(f, "Invalid kernel command line: {err}"),
            KernelDoesNotFit(load_addr, size) => write!(
                f,
                "The kernel doesn't fit in the microVM memory (load_addr={load_addr}, size={size})"
            ),
            KernelFormatUnsupported => {
                write!(f, "The supplied kernel format is not supported.")
            }
            LoadCommandline(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(f, "Cannot load command line string. {err_msg}")
            }
            MicroVMAlreadyRunning => write!(f, "Microvm already running."),
            MissingKernelConfig => write!(f, "Cannot start microvm without kernel configuration."),
            MissingMemSizeConfig => {
                write!(f, "Cannot start microvm without guest mem_size config.")
            }
            NetDeviceNotConfigured => {
                write!(f, "The net device configuration is missing the tap device.")
            }
            OpenBlockDevice(ref err) => {
                let mut err_msg = format!("{err:?}");
                err_msg = err_msg.replace('\"', "");

                write!(f, "Cannot open the block device backing file. {err_msg}")
            }
            OpenConsoleFile(ref err) => {
                let mut err_msg = format!("{err:?}");
                err_msg = err_msg.replace('\"', "");

                write!(f, "Cannot open the console output file. {err_msg}")
            }
            PeGzDecoder(ref err) => {
                write!(f, "The GZIP decoder couldn't decompress the kernel. {err}")
            }
            PeGzOpenKernel(ref err) => {
                write!(f, "Cannot open the file containing the kernel code. {err}")
            }
            PeGzInvalid => {
                write!(f, "Cannot find compressed kernel in file.")
            }
            RawOpenKernel(ref err) => {
                write!(f, "Cannot open the file containing the kernel code: {err}")
            }
            RegisterBalloonDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(
                    f,
                    "Cannot initialize a MMIO Balloon Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterBlockDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(
                    f,
                    "Cannot initialize a MMIO Block Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterEvent(ref err) => write!(f, "Cannot register EventHandler. {err:?}"),
            RegisterFsDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Cannot initialize a MMIO Fs Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterConsoleDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Cannot initialize a MMIO Console Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            #[cfg(target_os = "linux")]
            RegisterFsSigwinch(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Cannot register SIGWINCH file descriptor for Fs Device. {err_msg}"
                )
            }
            RegisterGpuDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(
                    f,
                    "Cannot initialize a MMIO Gpu Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterInputDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(
                    f,
                    "Cannot initialize a MMIO Input Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterNetDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Cannot initialize a MMIO Network Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterRngDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(
                    f,
                    "Cannot initialize a MMIO Rng Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterSndDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(
                    f,
                    "Cannot initialize a MMIO Snd Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RegisterVsockDevice(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Cannot initialize a MMIO Vsock Device or add a device to the MMIO Bus. {err_msg}"
                )
            }
            RestoreState(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");
                write!(f, "Cannot restore VM/vCPU state from snapshot. {err_msg}")
            }
            SecureVirtAttest(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Cannot attest the VM in the Secure Virtualization context. {err_msg}"
                )
            }
            SecureVirtPrepare(ref err) => {
                let mut err_msg = format!("{err}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Cannot initialize the Secure Virtualization backend. {err_msg}"
                )
            }
            ShmHostAddr(ref err) => {
                let mut err_msg = format!("{err:?}");
                err_msg = err_msg.replace('\"', "");

                write!(
                    f,
                    "Error obtaining the host address of an SHM region. {err_msg}"
                )
            }
            ShmConfig(ref err) => {
                let mut err_msg = format!("{err:?}");
                err_msg = err_msg.replace('\"', "");

                write!(f, "Error while configuring an SHM region. {err_msg}")
            }
            ShmCreate(ref err) => {
                let mut err_msg = format!("{err:?}");
                err_msg = err_msg.replace('\"', "");

                write!(f, "Error while creating an SHM region. {err_msg}")
            }
            InvalidTee => {
                write!(f, "TEE selected is not currently supported")
            }
        }
    }
}

pub enum Payload {
    #[cfg(all(target_arch = "x86_64", not(feature = "tee")))]
    KernelMmap,
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    KernelCopy,
    ExternalKernel(ExternalKernel),
    #[cfg(test)]
    Empty,
    Firmware,
    #[cfg(feature = "tee")]
    Tee,
}

fn choose_payload(vm_resources: &VmResources) -> Result<Payload, StartMicrovmError> {
    if let Some(_kernel_bundle) = &vm_resources.kernel_bundle {
        #[cfg(feature = "tee")]
        if vm_resources.qboot_bundle.is_none() || vm_resources.initrd_bundle.is_none() {
            return Err(StartMicrovmError::MissingKernelConfig);
        }

        #[cfg(feature = "tee")]
        return Ok(Payload::Tee);

        #[cfg(all(target_arch = "x86_64", not(feature = "tee")))]
        return Ok(Payload::KernelMmap);

        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        return Ok(Payload::KernelCopy);
    } else if let Some(external_kernel) = vm_resources.external_kernel() {
        Ok(Payload::ExternalKernel(external_kernel.clone()))
    } else if cfg!(feature = "efi") || vm_resources.firmware_config.is_some() {
        Ok(Payload::Firmware)
    } else {
        Err(StartMicrovmError::MissingKernelConfig)
    }
}

/// Builds and starts a microVM based on the current Firecracker VmResources configuration.
///
/// This is the default build recipe, one could build other microVM flavors by using the
/// independent functions in this module instead of calling this recipe.
///
/// An `Arc` reference of the built `Vmm` is also plugged in the `EventManager`, while another
/// is returned.
pub fn build_microvm(
    vm_resources: &super::resources::VmResources,
    event_manager: &mut EventManager,
    _shutdown_efd: Option<EventFd>,
    _sender: Sender<WorkerMessage>,
) -> std::result::Result<Arc<Mutex<Vmm>>, StartMicrovmError> {
    let payload = choose_payload(vm_resources)?;

    let (guest_memory, arch_memory_info, mut _shm_manager, payload_config) = create_guest_memory(
        vm_resources
            .vm_config()
            .mem_size_mib
            .ok_or(StartMicrovmError::MissingMemSizeConfig)?,
        vm_resources,
        &payload,
    )?;

    let vcpu_config = vm_resources.vcpu_config();

    // Clone the command-line so that a failed boot doesn't pollute the original.
    #[allow(unused_mut)]
    let mut kernel_cmdline = Cmdline::new(arch::CMDLINE_MAX_SIZE);
    if let Some(cmdline) = payload_config.kernel_cmdline {
        kernel_cmdline.insert_str_safe(cmdline.as_str()).unwrap();
    } else if let Some(cmdline) = &vm_resources.kernel_cmdline.prolog {
        kernel_cmdline.insert_str_safe(cmdline).unwrap();
    } else {
        kernel_cmdline.insert_str(DEFAULT_KERNEL_CMDLINE).unwrap();
    }

    if let Some(cmdline) = &vm_resources.kernel_cmdline.krun_env {
        kernel_cmdline
            .insert_str_safe(cmdline.as_str())
            .map_err(|e| {
                // Log the offending string for debugging but convert to a proper error
                // that won't panic - this is cross-platform compatible
                format!("Failed to insert krun_env into kernel cmdline: {:?}. krun_env was: {:?}", e, cmdline.as_str())
            })
            .unwrap();
    }

    if let Some(kernel_console) = &vm_resources.kernel_console {
        let cmdline = kernel_cmdline.as_str();
        let console_start_idx = cmdline.find("console=").unwrap();
        let console_end_idx = cmdline
            .get(console_start_idx..)
            .and_then(|s| s.find(" ").map(|i| i + console_start_idx));

        let cmdline = cmdline.replace(
            &cmdline[console_start_idx..console_end_idx.unwrap()],
            format!("console={kernel_console}").as_str(),
        );
        kernel_cmdline = Cmdline::new(arch::CMDLINE_MAX_SIZE);
        kernel_cmdline.insert_str_safe(cmdline).unwrap();
    }

    #[cfg(target_os = "windows")]
    if let Some(extra_cmdline) = windows_kernel_cmdline_append() {
        kernel_cmdline.insert_str_safe(extra_cmdline.as_str()).unwrap();
    }

    #[cfg(all(not(feature = "tee"), not(target_os = "windows")))]
    #[allow(unused_mut)]
    let mut vm = setup_vm(&guest_memory, vm_resources.nested_enabled)?;

    #[cfg(all(not(feature = "tee"), target_os = "windows"))]
    #[allow(unused_mut)]
    let mut vm = setup_vm(
        &guest_memory,
        vm_resources.nested_enabled,
        vcpu_config.vcpu_count as u32,
    )?;

    #[cfg(feature = "tee")]
    let (_kvm, vm) = {
        let kvm = KvmContext::new()
            .map_err(Error::KvmContext)
            .map_err(StartMicrovmError::Internal)?;
        let vm = setup_vm(
            &kvm,
            &guest_memory,
            vm_resources,
            #[cfg(feature = "tdx")]
            _sender.clone(),
        )?;
        (kvm, vm)
    };

    #[cfg(feature = "tee")]
    let tee = vm_resources.tee_config().tee;

    #[cfg(feature = "amd-sev")]
    let snp_launcher = match tee {
        Tee::Snp => Some(
            vm.snp_secure_virt_prepare(&guest_memory)
                .map_err(StartMicrovmError::SecureVirtPrepare)?,
        ),
        _ => None,
    };

    #[cfg(feature = "tdx")]
    let mut tdx_launcher = match tee {
        Tee::Tdx => vm
            .tdx_secure_virt_prepare()
            .map_err(StartMicrovmError::SecureVirtPrepare)?,
        _ => panic!(),
    };

    #[cfg(all(feature = "tee", not(feature = "tdx")))]
    let measured_regions = {
        println!("Injecting and measuring memory regions. This may take a while.");

        let qboot_size = if let Some(qboot_bundle) = &vm_resources.qboot_bundle {
            qboot_bundle.size
        } else {
            return Err(StartMicrovmError::MissingKernelConfig);
        };
        let (kernel_guest_addr, kernel_size) =
            if let Some(kernel_bundle) = &vm_resources.kernel_bundle {
                (kernel_bundle.guest_addr, kernel_bundle.size)
            } else {
                return Err(StartMicrovmError::MissingKernelConfig);
            };
        let (initrd_addr, initrd_size) = if let Some(initrd_config) = &payload_config.initrd_config
        {
            (initrd_config.address, initrd_config.size)
        } else {
            return Err(StartMicrovmError::MissingKernelConfig);
        };

        vec![
            MeasuredRegion {
                guest_addr: arch::FIRMWARE_START,
                host_addr: guest_memory
                    .get_host_address(GuestAddress(arch::FIRMWARE_START))
                    .unwrap() as u64,
                size: qboot_size,
            },
            MeasuredRegion {
                guest_addr: kernel_guest_addr,
                host_addr: guest_memory
                    .get_host_address(GuestAddress(kernel_guest_addr))
                    .unwrap() as u64,
                size: kernel_size,
            },
            MeasuredRegion {
                guest_addr: initrd_addr.0,
                host_addr: guest_memory.get_host_address(initrd_addr).unwrap() as u64,
                size: initrd_size,
            },
            MeasuredRegion {
                guest_addr: arch::x86_64::layout::ZERO_PAGE_START,
                host_addr: guest_memory
                    .get_host_address(GuestAddress(arch::x86_64::layout::ZERO_PAGE_START))
                    .unwrap() as u64,
                size: 4096,
            },
        ]
    };

    #[cfg(feature = "tdx")]
    let measured_regions = {
        println!("Injecting and measuring memory regions. This may take a while.");
        let qboot_size = if let Some(qboot_bundle) = &vm_resources.qboot_bundle {
            qboot_bundle.size
        } else {
            return Err(StartMicrovmError::MissingKernelConfig);
        };
        let m = vec![
            MeasuredRegion {
                guest_addr: 0,
                host_addr: guest_memory.get_host_address(GuestAddress(0)).unwrap() as u64,
                size: 0x8000_0000,
            },
            MeasuredRegion {
                guest_addr: arch::FIRMWARE_START,
                host_addr: guest_memory
                    .get_host_address(GuestAddress(arch::FIRMWARE_START))
                    .unwrap() as u64,
                size: qboot_size,
            },
        ];

        m
    };

    let mut serial_devices = Vec::new();

    // Create the legacy serial device if we're booting from a firmware
    if (cfg!(feature = "efi") || vm_resources.firmware_config.is_some())
        && !vm_resources.disable_implicit_console
    {
        serial_devices.push(setup_serial_device(
            event_manager,
            None,
            None,
            // Uncomment this to get EFI output when debugging EDK2.
            //Some(Box::new(io::stdout())),
        )?);
    };

    // We can't call to `setup_terminal_raw_mode` until `Vmm` is created,
    // so let's keep track of FDs connected to legacy serial devices here
    // and set raw mode on them later.
    #[cfg(not(target_os = "windows"))]
    let mut serial_ttys = Vec::new();
    #[cfg(target_os = "windows")]
    let serial_ttys: Vec<i32> = Vec::new();

    #[cfg(not(target_os = "windows"))]
    for s in &vm_resources.serial_consoles {
        let input = unsafe { BorrowedFd::borrow_raw(s.input_fd) };
        if input.is_terminal() {
            serial_ttys.push(input);
        }
        let input: Option<Box<dyn devices::legacy::ReadableFd + Send>> = if s.input_fd >= 0 {
            Some(Box::new(unsafe { File::from_raw_fd(s.input_fd) }))
        } else {
            None
        };

        let output: Option<Box<dyn io::Write + Send>> = if s.output_fd >= 0 {
            Some(Box::new(unsafe { File::from_raw_fd(s.output_fd) }))
        } else {
            None
        };

        serial_devices.push(setup_serial_device(event_manager, input, output)?);
    }

    #[cfg(target_os = "windows")]
    for s in &vm_resources.serial_consoles {
        let output: Option<Box<dyn io::Write + Send>> = match s.output_fd {
            1 => Some(Box::new(io::stdout())),
            2 => Some(Box::new(io::stderr())),
            fd if fd >= 0 => Some(Box::new(CrtFdWriter(fd))),
            _ => None,
        };
        let input: Option<Box<dyn devices::legacy::ReadableFd + Send>> =
            crate::windows::stdin_reader::WindowsStdinInput::new()
                .ok()
                .map(|r| Box::new(r) as Box<dyn devices::legacy::ReadableFd + Send>);
        serial_devices.push(setup_serial_device(event_manager, input, output)?);
    }

    // On Windows, if the caller did not configure any serial console, auto-add a
    // default COM1 device (stdout output + stdin input) so that a Linux guest
    // booting with `console=ttyS0` produces visible output.  Without this,
    // PortIODeviceManager::register_devices() skips COM1 registration entirely.
    #[cfg(target_os = "windows")]
    if serial_devices.is_empty() {
        let output: Option<Box<dyn io::Write + Send>> = if let Some(path) = &vm_resources.console_output
        {
            Some(Box::new(
                open_windows_console_output_file(path)
                    .map_err(StartMicrovmError::OpenConsoleFile)?,
            ))
        } else {
            Some(Box::new(io::stdout()))
        };
        let input: Option<Box<dyn devices::legacy::ReadableFd + Send>> =
            crate::windows::stdin_reader::WindowsStdinInput::new()
                .ok()
                .map(|r| Box::new(r) as Box<dyn devices::legacy::ReadableFd + Send>);
        serial_devices.push(setup_serial_device(event_manager, input, output)?);
    }

    #[cfg(target_os = "windows")]
    let _ = &serial_ttys;

    let exit_evt = EventFd::new(utils::eventfd::EFD_NONBLOCK)
        .map_err(Error::EventFd)
        .map_err(StartMicrovmError::Internal)?;

    #[cfg(target_arch = "x86_64")]
    // Safe to unwrap 'serial_device' as it's always 'Some' on x86_64.
    // x86_64 uses the i8042 reset event as the Vmm exit event.
    let mut pio_device_manager = PortIODeviceManager::new(
        Arc::new(Mutex::new(Cmos::new(
            arch_memory_info.ram_below_gap,
            arch_memory_info.ram_above_gap,
        ))),
        serial_devices,
        exit_evt
            .try_clone()
            .map_err(Error::EventFd)
            .map_err(StartMicrovmError::Internal)?,
    )
    .map_err(Error::CreateLegacyDevice)
    .map_err(StartMicrovmError::Internal)?;

    // Instantiate the MMIO device manager.
    // 'mmio_base' address has to be an address which is protected by the kernel
    // and is architectural specific.
    #[allow(unused_mut)]
    let mut mmio_device_manager = MMIODeviceManager::new(
        &mut (arch::MMIO_MEM_START.clone()),
        (arch::IRQ_BASE, arch::IRQ_MAX),
    );

    #[cfg(target_os = "macos")]
    let vcpu_list = {
        let cpu_count = vm_resources.vm_config().vcpu_count.unwrap();
        Arc::new(VcpuList::new(cpu_count as u64))
    };

    let mut vcpus;
    let intc: IrqChip;
    // For x86_64 we need to create the interrupt controller before calling `KVM_CREATE_VCPUS`
    // while on aarch64 we need to do it the other way around.
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        let ioapic: Box<dyn IrqChipT> = if vm_resources.split_irqchip {
            Box::new(
                IoApic::new(vm.fd(), _sender.clone())
                    .map_err(StartMicrovmError::CreateKvmIrqChip)?,
            )
        } else {
            Box::new(KvmIoapic::new(vm.fd()).map_err(StartMicrovmError::CreateKvmIrqChip)?)
        };
        intc = Arc::new(Mutex::new(IrqChipDevice::new(ioapic)));

        attach_legacy_devices(
            &vm,
            vm_resources.split_irqchip,
            &mut pio_device_manager,
            &mut mmio_device_manager,
            Some(intc.clone()),
        )?;

        // In restore mode the vCPU regs/sregs/MSRs/page-tables are all reloaded
        // from the snapshot state, and the boot-time `setup_sregs` would otherwise
        // write boot page tables into the (already-populated) snapshot RAM and
        // corrupt it. Skip boot vCPU configuration entirely when restoring.
        #[cfg(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee")))]
        let restoring = crate::snapshot::restore_state_path().is_some();
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee"))))]
        let restoring = false;

        let kernel_boot =
            vm_resources.firmware_config.is_none() && !cfg!(feature = "tee") && !restoring;

        vcpus = create_vcpus_x86_64(
            &vm,
            &vcpu_config,
            &guest_memory,
            payload_config.entry_addr,
            &pio_device_manager.io_bus,
            &exit_evt,
            kernel_boot,
            #[cfg(feature = "tee")]
            _sender,
        )
        .map_err(StartMicrovmError::Internal)?;
    }

    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    {
        let irq_notify = Arc::new(
            utils::eventfd::EventFd::new(utils::eventfd::EFD_NONBLOCK)
                .map_err(|e| StartMicrovmError::Internal(Error::EventFd(e)))?,
        );
        let pending_interrupt: PendingInterruptQueue =
            Arc::new(Mutex::new(std::collections::VecDeque::new()));
        intc = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(WhpxIrqChip::new(
            vm.partition(),
            u32::from(vcpu_config.vcpu_count),
            irq_notify.clone(),
            pending_interrupt.clone(),
        )))));

        attach_legacy_devices(
            &mut pio_device_manager,
            &mut mmio_device_manager,
            intc.clone(),
        )?;

        vcpus = create_vcpus_x86_64(
            &vm,
            &vcpu_config,
            &guest_memory,
            payload_config.entry_addr,
            &pio_device_manager.io_bus,
            &exit_evt,
            Some(irq_notify),
            Some(pending_interrupt),
        )
        .map_err(StartMicrovmError::Internal)?;

        // Set MMIO bus for each vCPU to handle APIC/IOAPIC accesses
        for vcpu in &mut vcpus {
            vcpu.set_mmio_bus(mmio_device_manager.bus.clone());
        }
    }

    #[cfg(feature = "tdx")]
    {
        for vcpu in &vcpus {
            vcpu.tdx_secure_virt_prepare(&mut tdx_launcher);
        }
        vm.tdx_secure_virt_init_vcpus(&mut tdx_launcher).unwrap();
    }

    // On aarch64, the vCPUs need to be created (i.e call KVM_CREATE_VCPU) and configured before
    // setting up the IRQ chip because the `KVM_CREATE_VCPU` ioctl will return error if the IRQCHIP
    // was already initialized.
    // Search for `kvm_arch_vcpu_create` in arch/arm/kvm/arm.c.
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        vcpus = create_vcpus_aarch64(
            &vm,
            &vcpu_config,
            &arch_memory_info,
            payload_config.entry_addr,
            &exit_evt,
        )
        .map_err(StartMicrovmError::Internal)?;

        intc = {
            // The SoC in some popular boards (namely, the RPi family) doesn't support an
            // architected vGIC, which is required for requesting KVM the instantiation of a
            // GICv3. To relieve the users from having to configure the gic version manually,
            // try first to instantiate a GICv3, and fall back to a GICv2 if it fails.
            let vcpu_count = vm_resources.vm_config().vcpu_count.unwrap() as u64;
            let gic = match KvmGicV3::new(vm.fd(), vcpu_count) {
                Ok(gicv3) => IrqChipDevice::new(Box::new(gicv3)),
                Err(_) => {
                    warn!("KVM GICv3 creation failed, falling back to KVM GICv2");
                    IrqChipDevice::new(Box::new(KvmGicV2::new(vm.fd(), vcpu_count)))
                }
            };
            Arc::new(Mutex::new(gic))
        };

        attach_legacy_devices(
            &vm,
            &mut mmio_device_manager,
            &mut kernel_cmdline,
            intc.clone(),
            serial_devices,
        )?;
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        intc = {
            // If the system supports the in-kernel GIC, use it. Otherwise, fall back to the
            // userspace implementation.
            let gic = match HvfGicV3::new(vm_resources.vm_config().vcpu_count.unwrap() as u64) {
                Ok(hvfgic) => IrqChipDevice::new(Box::new(hvfgic)),
                Err(_) => IrqChipDevice::new(Box::new(GicV3::new(vcpu_list.clone()))),
            };
            Arc::new(Mutex::new(gic))
        };

        vcpus = create_vcpus_aarch64(
            &vm,
            &vcpu_config,
            &arch_memory_info,
            payload_config.entry_addr,
            &exit_evt,
            vcpu_list.clone(),
            vm_resources.nested_enabled,
        )
        .map_err(StartMicrovmError::Internal)?;

        attach_legacy_devices(
            &vm,
            &mut mmio_device_manager,
            &mut kernel_cmdline,
            intc.clone(),
            serial_devices,
            event_manager,
            _shutdown_efd,
        )?;
    }

    #[cfg(all(target_arch = "riscv64", target_os = "linux"))]
    {
        vcpus = create_vcpus_riscv64(
            &vm,
            &vcpu_config,
            &guest_memory,
            payload_config.entry_addr,
            &exit_evt,
        )
        .map_err(StartMicrovmError::Internal)?;

        intc = Arc::new(Mutex::new(IrqChipDevice::new(Box::new(
            KvmAia::new(vm.fd(), vm_resources.vm_config().vcpu_count.unwrap() as u32).unwrap(),
        ))));

        attach_legacy_devices(
            &vm,
            &mut mmio_device_manager,
            &mut kernel_cmdline,
            serial_device,
        )?;
    }

    // We use this atomic to record the exit code set by init/init.c in the VM.
    let exit_code = Arc::new(AtomicI32::new(i32::MAX));

    let mut vmm = Vmm {
        guest_memory,
        arch_memory_info,
        kernel_cmdline,
        vcpus_handles: Vec::new(),
        exit_evt,
        exit_observers: Vec::new(),
        exit_code: exit_code.clone(),
        vm,
        mmio_device_manager,
        #[cfg(target_arch = "x86_64")]
        pio_device_manager,
    };

    // Set raw mode for FDs that are connected to legacy serial devices.
    for serial_tty in serial_ttys {
        setup_terminal_raw_mode(&mut vmm, Some(serial_tty), false);
    }

    #[cfg(not(feature = "tee"))]
    attach_balloon_device(&mut vmm, event_manager, intc.clone())?;
    #[cfg(not(feature = "tee"))]
    attach_rng_device(&mut vmm, event_manager, intc.clone())?;
    let mut console_id = 0;
    if !vm_resources.disable_implicit_console && windows_attach_implicit_virtio_console() {
        attach_console_devices(
            &mut vmm,
            event_manager,
            intc.clone(),
            vm_resources,
            None,
            console_id,
        )?;
        console_id += 1;
    }

    for console_cfg in vm_resources.virtio_consoles.iter() {
        attach_console_devices(
            &mut vmm,
            event_manager,
            intc.clone(),
            vm_resources,
            Some(console_cfg),
            console_id,
        )?;
        console_id += 1;
    }

    #[cfg(not(any(feature = "tee", feature = "nitro")))]
    let export_table: Option<ExportTable> = if cfg!(feature = "gpu") {
        Some(Default::default())
    } else {
        None
    };

    #[cfg(feature = "gpu")]
    if let Some(virgl_flags) = vm_resources.gpu_virgl_flags {
        let display_backend = vm_resources
            .display_backend
            .unwrap_or_else(|| NoopDisplayBackend::into_display_backend(None));

        attach_gpu_device(
            &mut vmm,
            event_manager,
            &mut _shm_manager,
            #[cfg(not(feature = "tee"))]
            export_table.clone(),
            intc.clone(),
            virgl_flags,
            Box::from(&vm_resources.displays[..]),
            display_backend,
            #[cfg(target_os = "macos")]
            _sender.clone(),
        )?;
    }

    #[cfg(feature = "input")]
    if !vm_resources.input_backends.is_empty() {
        attach_input_devices(&mut vmm, &vm_resources.input_backends, intc.clone())?;
    }

    #[cfg(not(any(feature = "tee", feature = "nitro")))]
    attach_fs_devices(
        &mut vmm,
        &vm_resources.fs,
        &mut _shm_manager,
        #[cfg(not(feature = "tee"))]
        export_table,
        intc.clone(),
        exit_code,
        #[cfg(target_os = "macos")]
        _sender,
    )?;
    #[cfg(feature = "blk")]
    attach_block_devices(&mut vmm, &vm_resources.block, intc.clone())?;

    if let Some(vsock) = vm_resources.vsock.get() {
        attach_unixsock_vsock_device(&mut vmm, vsock, event_manager, intc.clone())?;
        let tsi_flags = vm_resources.vsock.tsi_flags();
        if tsi_flags.contains(TsiFlags::HIJACK_INET) {
            vmm.kernel_cmdline.insert_str("tsi_hijack")?;
        }
        if tsi_flags.contains(TsiFlags::HIJACK_UNIX) {
            vmm.kernel_cmdline.insert_str("tsi_hijack_unix")?;
        }
    }

    #[cfg(feature = "net")]
    attach_net_devices(&mut vmm, &vm_resources.net, intc.clone())?;
    #[cfg(target_os = "windows")]
    attach_net_devices_windows(
        &mut vmm,
        &vm_resources.net_windows,
        event_manager,
        intc.clone(),
    )?;
    #[cfg(target_os = "windows")]
    attach_block_devices_windows(
        &mut vmm,
        &vm_resources.block_windows,
        event_manager,
        intc.clone(),
    )?;
    #[cfg(feature = "snd")]
    if vm_resources.snd_device {
        attach_snd_device(&mut vmm, intc.clone())?;
    }

    if let Some(s) = &vm_resources.kernel_cmdline.epilog {
        vmm.kernel_cmdline.insert_str_safe(s).unwrap();
    };

    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    {
        windows_irq_debug_log(format!("[CMDLINE] {}", vmm.kernel_cmdline.as_str()));
        for ((device_type, device_id), info) in vmm.mmio_device_manager.get_device_info() {
            windows_irq_debug_log(format!(
                "[MMIODEV] type={:?} id={} info={:?}",
                device_type, device_id, info
            ));
        }
    }

    // Log the final kernel command line
    #[cfg(target_os = "windows")]
    log::debug!("Final kernel cmdline: {}", vmm.kernel_cmdline.as_str());
    #[cfg(not(target_os = "windows"))]
    log::info!("Final kernel cmdline: {}", vmm.kernel_cmdline.as_str());

    // Restore mode: the cmdline and boot system config are already in the
    // snapshotted RAM; skip them and instead restore the saved KVM VM + vCPU
    // state so the vCPUs resume exactly where the template was paused.
    #[cfg(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee")))]
    let restoring = crate::snapshot::restore_state_path().is_some();
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee"))))]
    let restoring = false;

    #[cfg(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee")))]
    if restoring {
        let state_path = crate::snapshot::restore_state_path().unwrap();
        let state = crate::snapshot::read_state_file(&state_path)
            .map_err(StartMicrovmError::SnapshotMemFile)?;
        vmm.kvm_vm()
            .restore_state(&state.vm_state)
            .map_err(StartMicrovmError::RestoreState)?;
        if state.vcpu_states.len() != vcpus.len() {
            return Err(StartMicrovmError::SnapshotMemFile(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "snapshot vcpu count mismatch",
            )));
        }
        for (vcpu, vcpu_state) in vcpus.iter_mut().zip(state.vcpu_states.into_iter()) {
            vcpu.restore_state(vcpu_state)
                .map_err(StartMicrovmError::RestoreState)?;
        }
    }

    // Write the kernel command line to guest memory. This is x86_64 specific, since on
    // aarch64 the command line will be specified through the FDT.
    #[cfg(all(target_arch = "x86_64", not(feature = "tee")))]
    if !restoring {
        load_cmdline(&vmm)?;
    }

    if !restoring {
        vmm.configure_system(
            vcpus.as_slice(),
            &intc,
            &payload_config.initrd_config,
            &vm_resources.smbios_oem_strings,
        )
        .map_err(StartMicrovmError::Internal)?;
    }

    #[cfg(feature = "tee")]
    {
        match tee {
            #[cfg(feature = "amd-sev")]
            Tee::Snp => {
                let cpuid = _kvm
                    .fd()
                    .get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)
                    .map_err(VstateError::KvmCpuId)
                    .map_err(StartMicrovmError::SecureVirtAttest)?;
                vmm.kvm_vm()
                    .snp_secure_virt_measure(
                        cpuid,
                        vmm.guest_memory(),
                        measured_regions,
                        snp_launcher.unwrap(),
                    )
                    .map_err(StartMicrovmError::SecureVirtAttest)?;
            }
            #[cfg(feature = "tdx")]
            Tee::Tdx => {
                vmm.kvm_vm()
                    .tdx_secure_virt_prepare_memory(&mut tdx_launcher, &measured_regions)
                    .unwrap();
                vmm.kvm_vm()
                    .tdx_secure_virt_finalize_vm(tdx_launcher)
                    .map_err(StartMicrovmError::SecureVirtPrepare)?;
            }
            _ => return Err(StartMicrovmError::InvalidTee),
        }

        println!("Starting TEE/microVM.");
    }

    vmm.start_vcpus(vcpus)
        .map_err(StartMicrovmError::Internal)?;

    // Clippy thinks we don't need Arc<Mutex<...
    // but we don't want to change the event_manager interface
    #[allow(clippy::arc_with_non_send_sync)]
    let vmm = Arc::new(Mutex::new(vmm));
    event_manager
        .add_subscriber(vmm.clone())
        .map_err(StartMicrovmError::RegisterEvent)?;

    Ok(vmm)
}

fn load_external_kernel(
    guest_mem: &GuestMemoryMmap,
    arch_mem_info: &ArchMemoryInfo,
    external_kernel: &ExternalKernel,
) -> std::result::Result<(GuestAddress, Option<InitrdConfig>, Option<String>), StartMicrovmError> {
    let entry_addr = match external_kernel.format {
        // Raw images are treated as bundled kernels on x86_64
        #[cfg(target_arch = "x86_64")]
        KernelFormat::Raw => unreachable!(),
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        KernelFormat::Raw => {
            let data: Vec<u8> = std::fs::read(external_kernel.path.clone())
                .map_err(StartMicrovmError::RawOpenKernel)?;
            guest_mem.write(&data, GuestAddress(0x8000_0000)).unwrap();
            GuestAddress(0x8000_0000)
        }
        #[cfg(target_arch = "x86_64")]
        KernelFormat::Elf => {
            let mut file = File::options()
                .read(true)
                .write(false)
                .open(external_kernel.path.clone())
                .map_err(StartMicrovmError::ElfOpenKernel)?;
            let load_result = loader::Elf::load(guest_mem, None, &mut file, None)
                .map_err(StartMicrovmError::ElfLoadKernel)?;
            load_result.kernel_load
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        KernelFormat::PeGz => {
            let data: Vec<u8> = std::fs::read(external_kernel.path.clone())
                .map_err(StartMicrovmError::PeGzOpenKernel)?;
            if let Some(magic) = data
                .windows(3)
                .position(|window| window == [0x1f, 0x8b, 0x8])
            {
                debug!("Found GZIP header on PE file at: 0x{magic:x}");
                let (_, compressed) = data.split_at(magic);
                let mut gz = GzDecoder::new(compressed);
                let mut kernel_data: Vec<u8> = Vec::new();
                gz.read_to_end(&mut kernel_data)
                    .map_err(StartMicrovmError::PeGzDecoder)?;
                guest_mem
                    .write(&kernel_data, GuestAddress(0x8000_0000))
                    .unwrap();
                GuestAddress(0x8000_0000)
            } else {
                return Err(StartMicrovmError::PeGzInvalid);
            }
        }
        #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
        KernelFormat::ImageBz2 => {
            let data: Vec<u8> = std::fs::read(external_kernel.path.clone())
                .map_err(StartMicrovmError::ImageBz2OpenKernel)?;
            if let Some(magic) = data
                .windows(4)
                .position(|window| window == [b'B', b'Z', b'h'])
            {
                debug!("Found BZIP2 header on Image file at: 0x{magic:x}");
                let (_, compressed) = data.split_at(magic);
                let mut kernel_data: Vec<u8> = Vec::new();
                let mut bz2 = bzip2::read::BzDecoder::new(compressed);
                bz2.read_to_end(&mut kernel_data)
                    .map_err(StartMicrovmError::ImageBz2Decoder)?;
                let load_result = loader::Elf::load(
                    guest_mem,
                    None,
                    &mut std::io::Cursor::new(kernel_data),
                    None,
                )
                .map_err(StartMicrovmError::ImageBz2LoadKernel)?;
                load_result.kernel_load
            } else {
                return Err(StartMicrovmError::ImageBz2Invalid);
            }
        }
        #[cfg(target_arch = "x86_64")]
        KernelFormat::ImageGz => {
            let data: Vec<u8> = std::fs::read(external_kernel.path.clone())
                .map_err(StartMicrovmError::ImageGzOpenKernel)?;
            if let Some(magic) = data
                .windows(3)
                .position(|window| window == [0x1f, 0x8b, 0x8])
            {
                debug!("Found GZIP header on Image file at: 0x{magic:x}");
                let (_, compressed) = data.split_at(magic);
                let mut gz = GzDecoder::new(compressed);
                let mut kernel_data: Vec<u8> = Vec::new();
                gz.read_to_end(&mut kernel_data)
                    .map_err(StartMicrovmError::ImageGzDecoder)?;
                let load_result = loader::Elf::load(
                    guest_mem,
                    None,
                    &mut std::io::Cursor::new(kernel_data),
                    None,
                )
                .map_err(StartMicrovmError::ImageGzLoadKernel)?;
                load_result.kernel_load
            } else {
                return Err(StartMicrovmError::ImageGzInvalid);
            }
        }
        #[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
        KernelFormat::ImageZstd => {
            let data: Vec<u8> = std::fs::read(external_kernel.path.clone())
                .map_err(StartMicrovmError::ImageZstdOpenKernel)?;
            if let Some(magic) = data
                .windows(4)
                .position(|window| window == [0x28, 0xb5, 0x2f, 0xfd])
            {
                debug!("Found ZSTD header on Image file at: 0x{magic:x}");
                let (_, zstd_data) = data.split_at(magic);
                let mut kernel_data: Vec<u8> = Vec::new();
                let _ = zstd::stream::copy_decode(zstd_data, &mut kernel_data);
                let load_result = loader::Elf::load(
                    guest_mem,
                    None,
                    &mut std::io::Cursor::new(kernel_data),
                    None,
                )
                .map_err(StartMicrovmError::ImageZstdLoadKernel)?;
                load_result.kernel_load
            } else {
                return Err(StartMicrovmError::ImageZstdInvalid);
            }
        }
        _ => return Err(StartMicrovmError::KernelFormatUnsupported),
    };

    debug!("load_external_kernel: 0x{:x}", entry_addr.0);

    let initrd_config = if let Some(initramfs_path) = &external_kernel.initramfs_path {
        let data = std::fs::read(initramfs_path).map_err(StartMicrovmError::InitrdRead)?;
        guest_mem
            .write(&data, GuestAddress(arch_mem_info.initrd_addr))
            .unwrap();
        Some(InitrdConfig {
            address: GuestAddress(arch_mem_info.initrd_addr),
            size: data.len(),
        })
    } else {
        None
    };

    Ok((entry_addr, initrd_config, external_kernel.cmdline.clone()))
}

/// Create guest memory for the given regions (snapshot-aware). By default the regions are
/// anonymous mmaps; when `KRUN_SNAPSHOT_MEM_FILE` is set (snapshot-fork mode,
/// unix only) they are backed by that file with `MAP_SHARED` so the file holds
/// the live RAM contents — a memory snapshot is then a pause + msync of the
/// file, and a restore maps the same file `MAP_PRIVATE` (CoW).
fn create_guest_memory_regions(
    arch_mem_regions: &[(vm_memory::GuestAddress, usize)],
) -> std::result::Result<GuestMemoryMmap, StartMicrovmError> {
    // Restore mode: map the snapshot RAM file MAP_PRIVATE (kernel page-level CoW)
    // using the layout saved in the state file — guest RAM starts as the template's
    // RAM, divergent writes stay private to this child. This IS the fork.
    #[cfg(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee")))]
    if let (Some(state_path), Some(mem_path)) = (
        crate::snapshot::restore_state_path(),
        crate::snapshot::mem_file_path(),
    ) {
        let state =
            crate::snapshot::read_state_file(&state_path).map_err(StartMicrovmError::SnapshotMemFile)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&mem_path)
            .map_err(StartMicrovmError::SnapshotMemFile)?;
        let mut regions = Vec::with_capacity(state.mem_layout.len());
        for (addr, size, offset) in &state.mem_layout {
            let region_file = file
                .try_clone()
                .map_err(StartMicrovmError::SnapshotMemFile)?;
            // MAP_PRIVATE of the snapshot file = kernel page-level CoW. Reads see
            // the template's RAM; writes fault a private copy. PROT_READ|WRITE.
            let mmap_region = vm_memory::mmap::MmapRegionBuilder::new(*size)
                .with_mmap_prot(libc::PROT_READ | libc::PROT_WRITE)
                .with_mmap_flags(libc::MAP_NORESERVE | libc::MAP_PRIVATE)
                .with_file_offset(vm_memory::FileOffset::new(region_file, *offset))
                .build()
                .map_err(|e| {
                    StartMicrovmError::GuestMemoryMmap(vm_memory::Error::MmapRegion(e))
                })?;
            let region = vm_memory::GuestRegionMmap::new(mmap_region, vm_memory::GuestAddress(*addr))
                .map_err(StartMicrovmError::GuestMemoryMmap)?;
            regions.push(region);
        }
        let guest_mem = GuestMemoryMmap::from_regions(regions)
            .map_err(StartMicrovmError::GuestMemoryMmap)?;
        return Ok(guest_mem);
    }

    #[cfg(all(unix, not(feature = "tee")))]
    if let Some(path) = crate::snapshot::mem_file_path() {
        let total: u64 = arch_mem_regions.iter().map(|(_, s)| *s as u64).sum();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| {
                StartMicrovmError::SnapshotMemFile(e)
            })?;
        file.set_len(total).map_err(|e| {
            StartMicrovmError::SnapshotMemFile(e)
        })?;

        let mut offset: u64 = 0;
        let mut ranges = Vec::with_capacity(arch_mem_regions.len());
        let mut layout = Vec::with_capacity(arch_mem_regions.len());
        for (addr, size) in arch_mem_regions {
            let region_file = file.try_clone().map_err(|e| {
                StartMicrovmError::SnapshotMemFile(e)
            })?;
            ranges.push((
                *addr,
                *size,
                Some(vm_memory::FileOffset::new(region_file, offset)),
            ));
            layout.push((*addr, *size, offset));
            offset += *size as u64;
        }

        let guest_mem = GuestMemoryMmap::from_ranges_with_files(ranges)
            .map_err(StartMicrovmError::GuestMemoryMmap)?;
        *crate::snapshot::MEM_BACKING.lock().unwrap() =
            Some(crate::snapshot::MemBacking { file, layout });
        return Ok(guest_mem);
    }

    GuestMemoryMmap::from_ranges(arch_mem_regions).map_err(StartMicrovmError::GuestMemoryMmap)
}

fn load_payload(
    _vm_resources: &VmResources,
    guest_mem: GuestMemoryMmap,
    _arch_mem_info: &ArchMemoryInfo,
    payload: &Payload,
) -> std::result::Result<
    (
        GuestMemoryMmap,
        GuestAddress,
        Option<InitrdConfig>,
        Option<String>,
    ),
    StartMicrovmError,
> {
    match payload {
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        Payload::KernelCopy => {
            let (kernel_entry_addr, kernel_host_addr, kernel_guest_addr, kernel_size) =
                if let Some(kernel_bundle) = &_vm_resources.kernel_bundle {
                    (
                        kernel_bundle.entry_addr,
                        kernel_bundle.host_addr,
                        kernel_bundle.guest_addr,
                        kernel_bundle.size,
                    )
                } else {
                    return Err(StartMicrovmError::MissingKernelConfig);
                };

            let kernel_data =
                unsafe { std::slice::from_raw_parts(kernel_host_addr as *mut u8, kernel_size) };
            if kernel_guest_addr + kernel_size as u64 > _arch_mem_info.ram_last_addr {
                return Err(StartMicrovmError::KernelDoesNotFit(
                    kernel_guest_addr,
                    kernel_size,
                ));
            }
            guest_mem
                .write(kernel_data, GuestAddress(kernel_guest_addr))
                .unwrap();
            Ok((guest_mem, GuestAddress(kernel_entry_addr), None, None))
        }
        #[cfg(all(
            target_arch = "x86_64",
            not(feature = "tee"),
            not(target_os = "windows")
        ))]
        Payload::KernelMmap => {
            let (kernel_entry_addr, kernel_host_addr, kernel_guest_addr, kernel_size) =
                if let Some(kernel_bundle) = &_vm_resources.kernel_bundle {
                    (
                        kernel_bundle.entry_addr,
                        kernel_bundle.host_addr,
                        kernel_bundle.guest_addr,
                        kernel_bundle.size,
                    )
                } else {
                    return Err(StartMicrovmError::MissingKernelConfig);
                };

            let kernel_region = unsafe {
                MmapRegion::build_raw(kernel_host_addr as *mut u8, kernel_size, 0, 0)
                    .map_err(StartMicrovmError::InvalidKernelBundle)?
            };

            Ok((
                guest_mem
                    .insert_region(Arc::new(
                        GuestRegionMmap::new(kernel_region, GuestAddress(kernel_guest_addr))
                            .map_err(StartMicrovmError::GuestMemoryMmap)?,
                    ))
                    .map_err(StartMicrovmError::GuestMemoryMmap)?,
                GuestAddress(kernel_entry_addr),
                None,
                None,
            ))
        }
        #[cfg(all(target_arch = "x86_64", target_os = "windows", not(feature = "tee")))]
        Payload::KernelMmap => {
            let (kernel_entry_addr, kernel_host_addr, kernel_guest_addr, kernel_size) =
                if let Some(kernel_bundle) = &_vm_resources.kernel_bundle {
                    (
                        kernel_bundle.entry_addr,
                        kernel_bundle.host_addr,
                        kernel_bundle.guest_addr,
                        kernel_bundle.size,
                    )
                } else {
                    return Err(StartMicrovmError::MissingKernelConfig);
                };

            log::debug!(
                "Windows: Loading kernel to guest memory: guest_addr=0x{:x}, entry=0x{:x}, size={} bytes",
                kernel_guest_addr,
                kernel_entry_addr,
                kernel_size
            );

            let kernel_data =
                unsafe { std::slice::from_raw_parts(kernel_host_addr as *mut u8, kernel_size) };
            if kernel_guest_addr + kernel_size as u64 > _arch_mem_info.ram_last_addr {
                return Err(StartMicrovmError::KernelDoesNotFit(
                    kernel_guest_addr,
                    kernel_size,
                ));
            }
            guest_mem
                .write(kernel_data, GuestAddress(kernel_guest_addr))
                .unwrap();

            log::debug!(
                "Windows: Kernel loaded successfully, will start at entry point 0x{:x}",
                kernel_entry_addr
            );

            Ok((guest_mem, GuestAddress(kernel_entry_addr), None, None))
        }
        Payload::ExternalKernel(external_kernel) => {
            let (entry_addr, initrd_config, cmdline) =
                load_external_kernel(&guest_mem, _arch_mem_info, external_kernel)?;
            Ok((guest_mem, entry_addr, initrd_config, cmdline))
        }
        #[cfg(test)]
        Payload::Empty => Ok((guest_mem, GuestAddress(0), None, None)),
        #[cfg(feature = "tee")]
        Payload::Tee => {
            let (kernel_host_addr, kernel_guest_addr, kernel_size) =
                if let Some(kernel_bundle) = &_vm_resources.kernel_bundle {
                    (
                        kernel_bundle.host_addr,
                        kernel_bundle.guest_addr,
                        kernel_bundle.size,
                    )
                } else {
                    return Err(StartMicrovmError::MissingKernelConfig);
                };
            let kernel_data =
                unsafe { std::slice::from_raw_parts(kernel_host_addr as *mut u8, kernel_size) };
            guest_mem
                .write(kernel_data, GuestAddress(kernel_guest_addr))
                .unwrap();

            let (qboot_host_addr, qboot_size) =
                if let Some(qboot_bundle) = &_vm_resources.qboot_bundle {
                    (qboot_bundle.host_addr, qboot_bundle.size)
                } else {
                    return Err(StartMicrovmError::MissingKernelConfig);
                };
            let qboot_data =
                unsafe { std::slice::from_raw_parts(qboot_host_addr as *mut u8, qboot_size) };
            guest_mem
                .write(qboot_data, GuestAddress(arch::FIRMWARE_START))
                .unwrap();

            let (initrd_host_addr, initrd_size) =
                if let Some(initrd_bundle) = &_vm_resources.initrd_bundle {
                    (initrd_bundle.host_addr, initrd_bundle.size)
                } else {
                    return Err(StartMicrovmError::MissingKernelConfig);
                };
            let initrd_data =
                unsafe { std::slice::from_raw_parts(initrd_host_addr as *mut u8, initrd_size) };
            guest_mem
                .write(initrd_data, GuestAddress(_arch_mem_info.initrd_addr))
                .unwrap();

            let initrd_config = InitrdConfig {
                address: GuestAddress(_arch_mem_info.initrd_addr),
                size: initrd_data.len(),
            };

            Ok((
                guest_mem,
                GuestAddress(arch::RESET_VECTOR),
                Some(initrd_config),
                None,
            ))
        }
        Payload::Firmware => Ok((guest_mem, GuestAddress(arch::RESET_VECTOR), None, None)),
    }
}

pub struct PayloadConfig {
    entry_addr: GuestAddress,
    initrd_config: Option<InitrdConfig>,
    kernel_cmdline: Option<String>,
}

pub fn create_guest_memory(
    mem_size: usize,
    vm_resources: &VmResources,
    payload: &Payload,
) -> std::result::Result<
    (GuestMemoryMmap, ArchMemoryInfo, ShmManager, PayloadConfig),
    StartMicrovmError,
> {
    let mem_size = mem_size << 20;

    #[cfg(not(feature = "efi"))]
    let (firmware_data, firmware_size) = if let Some(firmware) = &vm_resources.firmware_config {
        let data = std::fs::read(firmware.path.clone()).map_err(StartMicrovmError::FirmwareRead)?;
        let len = data.len();
        (Some(data), Some(len))
    } else {
        (None, None)
    };
    #[cfg(feature = "efi")]
    let (firmware_data, firmware_size) = (Some(EDK2_BINARY), Some(EDK2_BINARY.len()));

    #[cfg(target_arch = "x86_64")]
    let (arch_mem_info, mut arch_mem_regions) = match payload {
        #[cfg(not(feature = "tee"))]
        Payload::KernelMmap => {
            // On Windows the kernel is copied into guest memory (no mmap support),
            // so we must NOT punch a hole — pass None so the full range is mapped
            // and the subsequent guest_mem.write() call succeeds.
            // On other platforms libkrunfw injects the kernel via mmap into the hole.
            #[cfg(target_os = "windows")]
            {
                arch::arch_memory_regions(mem_size, None, 0, 0, None)
            }
            #[cfg(not(target_os = "windows"))]
            {
                let (kernel_guest_addr, kernel_size) =
                    if let Some(kernel_bundle) = &vm_resources.kernel_bundle {
                        (kernel_bundle.guest_addr, kernel_bundle.size)
                    } else {
                        return Err(StartMicrovmError::MissingKernelConfig);
                    };
                arch::arch_memory_regions(mem_size, Some(kernel_guest_addr), kernel_size, 0, None)
            }
        }
        Payload::ExternalKernel(external_kernel) => arch::arch_memory_regions(
            mem_size,
            None,
            0,
            external_kernel.initramfs_size,
            firmware_size,
        ),
        #[cfg(feature = "tee")]
        Payload::Tee => {
            let (kernel_guest_addr, kernel_size) =
                if let Some(kernel_bundle) = &vm_resources.kernel_bundle {
                    (kernel_bundle.guest_addr, kernel_bundle.size)
                } else {
                    return Err(StartMicrovmError::MissingKernelConfig);
                };
            arch::arch_memory_regions(mem_size, Some(kernel_guest_addr), kernel_size, 0, None)
        }
        #[cfg(test)]
        Payload::Empty => arch::arch_memory_regions(mem_size, None, 0, 0, None),
        Payload::Firmware => arch::arch_memory_regions(mem_size, None, 0, 0, firmware_size),
    };
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    let (arch_mem_info, mut arch_mem_regions) = match payload {
        Payload::ExternalKernel(external_kernel) => {
            arch::arch_memory_regions(mem_size, external_kernel.initramfs_size, None)
        }
        _ => arch::arch_memory_regions(mem_size, 0, firmware_size),
    };

    let mut shm_manager = ShmManager::new(&arch_mem_info);

    #[cfg(not(feature = "tee"))]
    for (index, fs) in vm_resources.fs.iter().enumerate() {
        if let Some(shm_size) = fs.shm_size {
            shm_manager
                .create_fs_region(index, shm_size)
                .map_err(StartMicrovmError::ShmCreate)?;
        }
    }
    if vm_resources.gpu_virgl_flags.is_some() {
        let size = vm_resources.gpu_shm_size.unwrap_or(1 << 33);
        shm_manager
            .create_gpu_region(size)
            .map_err(StartMicrovmError::ShmCreate)?;
    }

    arch_mem_regions.extend(shm_manager.regions());

    let guest_mem = create_guest_memory_regions(&arch_mem_regions)?;

    // Restore mode: the kernel, cmdline, initrd and firmware are already present
    // in the snapshotted RAM (mapped CoW above), so skip loading the payload and
    // writing firmware. The entry address is irrelevant — vCPU registers are
    // restored from the state file before resume.
    #[cfg(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee")))]
    let restoring = crate::snapshot::restore_state_path().is_some();
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(feature = "tee"))))]
    let restoring = false;

    let (guest_mem, entry_addr, initrd_config, cmdline) = if restoring {
        (guest_mem, GuestAddress(0), None, None)
    } else {
        load_payload(vm_resources, guest_mem, &arch_mem_info, payload)?
    };

    // Only write firmware if data exists AND this isn't an ExternalKernel payload
    // (ExternalKernel does direct kernel boot and doesn't use EFI firmware)
    if !restoring && !matches!(payload, Payload::ExternalKernel(_)) {
        if let Some(firmware_data) = firmware_data.as_ref() {
            guest_mem
                .write(firmware_data, GuestAddress(arch_mem_info.firmware_addr))
                .map_err(StartMicrovmError::FirmwareInvalidAddress)?;
        }
    }

    let payload_config = PayloadConfig {
        entry_addr,
        initrd_config,
        kernel_cmdline: cmdline.clone(),
    };

    Ok((guest_mem, arch_mem_info, shm_manager, payload_config))
}

#[cfg(all(target_arch = "x86_64", not(feature = "tee")))]
fn load_cmdline(vmm: &Vmm) -> std::result::Result<(), StartMicrovmError> {
    kernel::loader::load_cmdline(
        vmm.guest_memory(),
        GuestAddress(arch::x86_64::layout::CMDLINE_START),
        &vmm.kernel_cmdline
            .as_cstring()
            .map_err(StartMicrovmError::LoadCommandline)?,
    )
    .map_err(StartMicrovmError::LoadCommandline)
}

#[cfg(all(target_os = "linux", not(feature = "tee")))]
pub(crate) fn setup_vm(
    guest_memory: &GuestMemoryMmap,
    _nested_enabled: bool,
) -> std::result::Result<Vm, StartMicrovmError> {
    let kvm = KvmContext::new()
        .map_err(Error::KvmContext)
        .map_err(StartMicrovmError::Internal)?;
    let mut vm = Vm::new(kvm.fd())
        .map_err(Error::Vm)
        .map_err(StartMicrovmError::Internal)?;
    vm.memory_init(guest_memory, kvm.max_memslots())
        .map_err(Error::Vm)
        .map_err(StartMicrovmError::Internal)?;
    Ok(vm)
}
#[cfg(all(target_os = "linux", feature = "tee"))]
pub(crate) fn setup_vm(
    kvm: &KvmContext,
    guest_memory: &GuestMemoryMmap,
    resources: &super::resources::VmResources,
    #[cfg(feature = "tdx")] _sender: Sender<WorkerMessage>,
) -> std::result::Result<Vm, StartMicrovmError> {
    let mut vm = Vm::new(
        kvm.fd(),
        resources.tee_config(),
        #[cfg(feature = "tdx")]
        _sender,
    )
    .map_err(Error::Vm)
    .map_err(StartMicrovmError::Internal)?;
    vm.memory_init(guest_memory, kvm.max_memslots())
        .map_err(Error::Vm)
        .map_err(StartMicrovmError::Internal)?;
    Ok(vm)
}
#[cfg(target_os = "macos")]
pub(crate) fn setup_vm(
    guest_memory: &GuestMemoryMmap,
    nested_enabled: bool,
) -> std::result::Result<Vm, StartMicrovmError> {
    let mut vm = Vm::new(nested_enabled)
        .map_err(Error::Vm)
        .map_err(StartMicrovmError::Internal)?;
    vm.memory_init(guest_memory)
        .map_err(Error::Vm)
        .map_err(StartMicrovmError::Internal)?;
    Ok(vm)
}

#[cfg(target_os = "windows")]
pub(crate) fn setup_vm(
    guest_memory: &GuestMemoryMmap,
    nested_enabled: bool,
    vcpu_count: u32,
) -> std::result::Result<Vm, StartMicrovmError> {
    let mut vm = Vm::new(nested_enabled, vcpu_count, true)
        .map_err(Error::Vm)
        .map_err(StartMicrovmError::Internal)?;
    vm.memory_init(guest_memory)
        .map_err(Error::Vm)
        .map_err(StartMicrovmError::Internal)?;
    Ok(vm)
}

/// Sets up the serial device.
pub fn setup_serial_device(
    event_manager: &mut EventManager,
    input: Option<Box<dyn devices::legacy::ReadableFd + Send>>,
    out: Option<Box<dyn io::Write + Send>>,
) -> std::result::Result<Arc<Mutex<Serial>>, StartMicrovmError> {
    let interrupt_evt = EventFd::new(utils::eventfd::EFD_NONBLOCK)
        .map_err(Error::EventFd)
        .map_err(StartMicrovmError::Internal)?;
    let has_input = input.is_some();
    let serial = Arc::new(Mutex::new(Serial::new(interrupt_evt, out, input)));
    if has_input {
        if let Err(e) = event_manager.add_subscriber(serial.clone()) {
            // TODO: We just log this message, and immediately return Ok, instead of returning the
            // actual error because this operation always fails with EPERM when adding a fd which
            // has been redirected to /dev/null via dup2 (this may happen inside the jailer).
            // Find a better solution to this (and think about the state of the serial device
            // while we're at it).
            warn!("Could not add serial input event to epoll: {e:?}");
        }
    }
    Ok(serial)
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn attach_legacy_devices(
    vm: &Vm,
    split_irqchip: bool,
    pio_device_manager: &mut PortIODeviceManager,
    mmio_device_manager: &mut MMIODeviceManager,
    intc: Option<Arc<Mutex<IrqChipDevice>>>,
) -> std::result::Result<(), StartMicrovmError> {
    pio_device_manager
        .register_devices()
        .map_err(Error::LegacyIOBus)
        .map_err(StartMicrovmError::Internal)?;

    if split_irqchip {
        mmio_device_manager
            .register_mmio_ioapic(intc)
            .map_err(Error::RegisterMMIODevice)
            .map_err(StartMicrovmError::Internal)?;
    }

    macro_rules! register_irqfd_evt {
        ($evt: ident, $index: expr) => {{
            vm.fd()
                .register_irqfd(&pio_device_manager.$evt, $index)
                .map_err(|e| {
                    Error::LegacyIOBus(device_manager::legacy::Error::EventFd(
                        io::Error::from_raw_os_error(e.errno()),
                    ))
                })
                .map_err(StartMicrovmError::Internal)?;
        }};
    }

    register_irqfd_evt!(com_evt_1, 4);
    register_irqfd_evt!(com_evt_2, 3);
    register_irqfd_evt!(com_evt_3, 4);
    register_irqfd_evt!(com_evt_4, 3);
    register_irqfd_evt!(kbd_evt, 1);
    Ok(())
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn attach_legacy_devices(
    pio_device_manager: &mut PortIODeviceManager,
    mmio_device_manager: &mut MMIODeviceManager,
    intc: IrqChip,
) -> std::result::Result<(), StartMicrovmError> {
    pio_device_manager
        .register_devices()
        .map_err(Error::LegacyIOBus)
        .map_err(StartMicrovmError::Internal)?;

    // PIT 8254 at 0x40–0x43.
    // Provides realistic counter read-back for TSC calibration.
    let pit = Arc::new(Mutex::new(devices::legacy::Pit::new()));
    pio_device_manager
        .io_bus
        .insert(pit, 0x40, 0x4)
        .map_err(device_manager::legacy::Error::BusError)
        .map_err(Error::LegacyIOBus)
        .map_err(StartMicrovmError::Internal)?;

    // Primary 8259A PIC at 0x20–0x21.
    let pic_primary = devices::legacy::windows_pic_stub::PicStub::primary();
    pio_device_manager
        .io_bus
        .insert(pic_primary, 0x20, 0x2)
        .map_err(device_manager::legacy::Error::BusError)
        .map_err(Error::LegacyIOBus)
        .map_err(StartMicrovmError::Internal)?;

    // Secondary (cascaded) 8259A PIC at 0xA0–0xA1.
    let pic_secondary = devices::legacy::windows_pic_stub::PicStub::secondary();
    pio_device_manager
        .io_bus
        .insert(pic_secondary, 0xA0, 0x2)
        .map_err(device_manager::legacy::Error::BusError)
        .map_err(Error::LegacyIOBus)
        .map_err(StartMicrovmError::Internal)?;

    // Legacy PCI configuration mechanism #1 ports (0xCF8-0xCFF).
    // WHPX does not supply a chipset PCI root complex for this MMIO-only guest,
    // but Linux still probes these ports during early x86 boot.
    let pci_config = Arc::new(Mutex::new(devices::legacy::PciConfigIoStub::new()));
    pio_device_manager
        .io_bus
        .insert(pci_config, 0xCF8, 0x8)
        .map_err(device_manager::legacy::Error::BusError)
        .map_err(Error::LegacyIOBus)
        .map_err(StartMicrovmError::Internal)?;

    // Register APIC stub devices to handle MMIO accesses without crashing
    let (ioapic_base, ioapic_size) = devices::legacy::windows_apic_stub::ApicStub::ioapic_range();
    let ioapic_stub = devices::legacy::windows_apic_stub::ApicStub::ioapic();
    mmio_device_manager
        .bus
        .insert(ioapic_stub, ioapic_base, ioapic_size)
        .map_err(device_manager::mmio::Error::BusError)
        .map_err(Error::RegisterMMIODevice)
        .map_err(StartMicrovmError::Internal)?;

    log::debug!("Registered IOAPIC stub device at 0x{:x}", ioapic_base);

    // PIT IRQ 0 timer thread (100 Hz).
    // On Windows, IRQ0 begins as a legacy PIC ExtINT via LAPIC LINT0 and later
    // migrates to IOAPIC delivery. Do not inject a blind fixed vector fallback.
    let intc_clone = intc.clone();
    std::thread::Builder::new()
        .name("pit-timer".into())
        .spawn(move || {
            let mut last_route = "waiting";
            let mut count = 0u64;

            loop {
                let route = if matches!(
                    devices::legacy::windows_apic_stub::query_route(0),
                    Some(route) if !route.masked && route.vector != 0
                ) {
                    "ioapic"
                } else if devices::legacy::windows_pic_stub::query_irq_vector(0).is_some() {
                    "pic"
                } else {
                    "waiting"
                };

                if route != last_route {
                    windows_irq_debug_log(format!(
                        "[PIT] route_change from={} to={} tick={}",
                        last_route, route, count
                    ));
                    match route {
                        "ioapic" => {
                            let current =
                                devices::legacy::windows_apic_stub::query_route(0).unwrap();
                            log::debug!(
                                "PIT timer: IRQ0 switched to IOAPIC vector=0x{:x} dest=0x{:x}",
                                current.vector,
                                current.destination
                            );
                        }
                        "pic" => {
                            let vector = devices::legacy::windows_pic_stub::query_irq_vector(0);
                            match vector {
                                Some(vector) => log::debug!(
                                    "PIT timer: IRQ0 using legacy PIC ExtINT vector=0x{:x}",
                                    vector
                                ),
                                None => log::debug!(
                                    "PIT timer: IRQ0 switched to legacy PIC routing, but no vector is currently deliverable"
                                ),
                            }
                        }
                        _ => {
                            log::debug!("PIT timer: waiting for guest IRQ0 routing (PIC or IOAPIC)")
                        }
                    }
                    last_route = route;
                }

                std::thread::sleep(std::time::Duration::from_millis(10));
                count += 1;

                if count <= 20 || count % 100 == 0 {
                    windows_irq_debug_log(format!(
                        "[PIT] tick={} route={} pic_irq0={:?} ioapic_irq0={:?}",
                        count,
                        route,
                        devices::legacy::windows_pic_stub::query_irq_vector(0),
                        devices::legacy::windows_apic_stub::query_route(0)
                            .map(|r| (r.vector, r.masked, r.destination)),
                    ));
                }

                if let Err(e) = intc_clone.lock().unwrap().set_irq(Some(0), None) {
                    windows_irq_debug_log(format!(
                        "[PIT] set_irq_failed tick={} route={} err={:?}",
                        count, last_route, e
                    ));
                    warn!(
                        "PIT IRQ0 injection failed after {} ticks (route={}): {e:?}",
                        count, last_route
                    );
                    break;
                }

                if count <= 20 || count % 100 == 0 {
                    windows_irq_debug_log(format!(
                        "[PIT] set_irq_ok tick={} route={}",
                        count, last_route
                    ));
                }

                if last_route != "waiting" && (count <= 10 || count % 100 == 0) {
                    log::debug!(
                        "PIT timer: injected {} IRQ0 ticks via {} routing",
                        count,
                        last_route
                    );
                }
            }
        })
        .map_err(|e| StartMicrovmError::Internal(Error::EventFd(e)))?;

    log::debug!("PIT timer thread started (100 Hz IRQ 0 routing)");

    Ok(())
}

#[cfg(all(
    any(target_arch = "aarch64", target_arch = "riscv64"),
    target_os = "linux"
))]
fn attach_legacy_devices(
    vm: &Vm,
    mmio_device_manager: &mut MMIODeviceManager,
    kernel_cmdline: &mut kernel::cmdline::Cmdline,
    intc: IrqChip,
    serial: Vec<Arc<Mutex<Serial>>>,
) -> std::result::Result<(), StartMicrovmError> {
    for s in serial {
        mmio_device_manager
            .register_mmio_serial(vm.fd(), kernel_cmdline, intc.clone(), s)
            .map_err(Error::RegisterMMIODevice)
            .map_err(StartMicrovmError::Internal)?;
    }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    mmio_device_manager
        .register_mmio_rtc(vm.fd())
        .map_err(Error::RegisterMMIODevice)
        .map_err(StartMicrovmError::Internal)?;

    Ok(())
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn attach_legacy_devices(
    vm: &Vm,
    mmio_device_manager: &mut MMIODeviceManager,
    kernel_cmdline: &mut kernel::cmdline::Cmdline,
    intc: IrqChip,
    serial: Vec<Arc<Mutex<Serial>>>,
    event_manager: &mut EventManager,
    shutdown_efd: Option<EventFd>,
) -> Result<(), StartMicrovmError> {
    for s in serial {
        mmio_device_manager
            .register_mmio_serial(vm, kernel_cmdline, intc.clone(), s)
            .map_err(Error::RegisterMMIODevice)
            .map_err(StartMicrovmError::Internal)?;
    }

    mmio_device_manager
        .register_mmio_rtc(vm, intc.clone())
        .map_err(Error::RegisterMMIODevice)
        .map_err(StartMicrovmError::Internal)?;

    mmio_device_manager
        .register_mmio_gic(vm, intc.clone())
        .map_err(Error::RegisterMMIODevice)
        .map_err(StartMicrovmError::Internal)?;

    if let Some(shutdown_efd) = shutdown_efd {
        mmio_device_manager
            .register_mmio_gpio(vm, intc.clone(), event_manager, shutdown_efd)
            .map_err(Error::RegisterMMIODevice)
            .map_err(StartMicrovmError::Internal)?;
    }

    Ok(())
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn create_vcpus_x86_64(
    vm: &Vm,
    vcpu_config: &VcpuConfig,
    guest_mem: &GuestMemoryMmap,
    entry_addr: GuestAddress,
    io_bus: &devices::Bus,
    exit_evt: &EventFd,
    kernel_boot: bool,
    #[cfg(feature = "tee")] pm_sender: Sender<WorkerMessage>,
) -> super::Result<Vec<Vcpu>> {
    let mut vcpus = Vec::with_capacity(vcpu_config.vcpu_count as usize);
    for cpu_index in 0..vcpu_config.vcpu_count {
        let mut vcpu = Vcpu::new_x86_64(
            cpu_index,
            vm.fd(),
            vm.supported_cpuid().clone(),
            vm.supported_msrs().clone(),
            io_bus.clone(),
            exit_evt.try_clone().map_err(Error::EventFd)?,
            #[cfg(feature = "tee")]
            pm_sender.clone(),
        )
        .map_err(Error::Vcpu)?;

        vcpu.configure_x86_64(guest_mem, entry_addr, vcpu_config, kernel_boot)
            .map_err(Error::Vcpu)?;

        vcpus.push(vcpu);
    }
    Ok(vcpus)
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
fn create_vcpus_x86_64(
    vm: &Vm,
    vcpu_config: &VcpuConfig,
    guest_mem: &GuestMemoryMmap,
    entry_addr: GuestAddress,
    io_bus: &devices::Bus,
    exit_evt: &EventFd,
    irq_pending_evt: Option<Arc<utils::eventfd::EventFd>>,
    pending_interrupt: Option<PendingInterruptQueue>,
) -> super::Result<Vec<Vcpu>> {
    let mut vcpus = Vec::with_capacity(vcpu_config.vcpu_count as usize);
    for cpu_index in 0..vcpu_config.vcpu_count {
        let vcpu = Vcpu::new(
            cpu_index,
            vm.partition(),
            guest_mem.clone(),
            entry_addr,
            io_bus.clone(),
            exit_evt.try_clone().map_err(Error::EventFd)?,
            irq_pending_evt.clone(),
            pending_interrupt.clone(),
        )
        .map_err(Error::Vcpu)?;

        vcpus.push(vcpu);
    }
    Ok(vcpus)
}

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
fn create_vcpus_aarch64(
    vm: &Vm,
    vcpu_config: &VcpuConfig,
    mem_info: &ArchMemoryInfo,
    entry_addr: GuestAddress,
    exit_evt: &EventFd,
) -> super::Result<Vec<Vcpu>> {
    let mut vcpus = Vec::with_capacity(vcpu_config.vcpu_count as usize);
    for cpu_index in 0..vcpu_config.vcpu_count {
        let mut vcpu = Vcpu::new_aarch64(
            cpu_index,
            vm.fd(),
            exit_evt.try_clone().map_err(Error::EventFd)?,
        )
        .map_err(Error::Vcpu)?;

        vcpu.configure_aarch64(vm.fd(), mem_info, entry_addr)
            .map_err(Error::Vcpu)?;

        vcpus.push(vcpu);
    }
    Ok(vcpus)
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn create_vcpus_aarch64(
    _vm: &Vm,
    vcpu_config: &VcpuConfig,
    mem_info: &ArchMemoryInfo,
    entry_addr: GuestAddress,
    exit_evt: &EventFd,
    vcpu_list: Arc<VcpuList>,
    nested_enabled: bool,
) -> super::Result<Vec<Vcpu>> {
    let mut vcpus = Vec::with_capacity(vcpu_config.vcpu_count as usize);
    let mut boot_senders: HashMap<u64, Sender<u64>> = HashMap::new();

    for cpu_index in 0..vcpu_config.vcpu_count {
        let (boot_sender, boot_receiver) = if cpu_index != 0 {
            let (boot_sender, boot_receiver) = unbounded();
            (Some(boot_sender), Some(boot_receiver))
        } else {
            (None, None)
        };

        let mut vcpu = Vcpu::new_aarch64(
            cpu_index,
            entry_addr,
            boot_receiver,
            exit_evt.try_clone().map_err(Error::EventFd)?,
            vcpu_list.clone(),
            nested_enabled,
        )
        .map_err(Error::Vcpu)?;

        vcpu.configure_aarch64(mem_info).map_err(Error::Vcpu)?;

        if let Some(boot_sender) = boot_sender {
            boot_senders.insert(vcpu.get_mpidr(), boot_sender);
        }

        vcpus.push(vcpu);
    }

    vcpus[0].set_boot_senders(boot_senders);

    Ok(vcpus)
}

#[cfg(all(target_arch = "riscv64", target_os = "linux"))]
fn create_vcpus_riscv64(
    vm: &Vm,
    vcpu_config: &VcpuConfig,
    guest_mem: &GuestMemoryMmap,
    entry_addr: GuestAddress,
    exit_evt: &EventFd,
) -> super::Result<Vec<Vcpu>> {
    let mut vcpus = Vec::with_capacity(vcpu_config.vcpu_count as usize);
    for cpu_index in 0..vcpu_config.vcpu_count {
        let mut vcpu = Vcpu::new_riscv64(
            cpu_index,
            vm.fd(),
            exit_evt.try_clone().map_err(Error::EventFd)?,
        )
        .map_err(Error::Vcpu)?;

        vcpu.configure_riscv64(vm.fd(), guest_mem, entry_addr)
            .map_err(Error::Vcpu)?;

        vcpus.push(vcpu);
    }
    Ok(vcpus)
}

/// Attaches an virtio mmio device to the device manager.
fn attach_mmio_device(
    vmm: &mut Vmm,
    id: String,
    intc: IrqChip,
    device: Arc<Mutex<dyn VirtioDevice>>,
) -> std::result::Result<(), device_manager::mmio::Error> {
    let mmio_device = MmioTransport::new(vmm.guest_memory().clone(), intc, device)?;

    let type_id = mmio_device.locked_device().device_type();
    let _cmdline = &mut vmm.kernel_cmdline;

    #[cfg(target_os = "linux")]
    let (_mmio_base, _irq) =
        vmm.mmio_device_manager
            .register_mmio_device(vmm.vm.fd(), mmio_device, type_id, id)?;
    #[cfg(target_os = "macos")]
    let (_mmio_base, _irq) =
        vmm.mmio_device_manager
            .register_mmio_device(mmio_device, type_id, id)?;
    #[cfg(target_os = "windows")]
    let (_mmio_base, _irq) =
        vmm.mmio_device_manager
            .register_mmio_device(mmio_device, type_id, id)?;

    #[cfg(target_arch = "x86_64")]
    vmm.mmio_device_manager
        .add_device_to_cmdline(_cmdline, _mmio_base, _irq)?;

    Ok(())
}

#[cfg(not(any(feature = "tee", feature = "nitro")))]
fn attach_fs_devices(
    vmm: &mut Vmm,
    fs_devs: &[FsDeviceConfig],
    shm_manager: &mut ShmManager,
    #[cfg(not(feature = "tee"))] export_table: Option<ExportTable>,
    intc: IrqChip,
    exit_code: Arc<AtomicI32>,
    #[cfg(target_os = "macos")] map_sender: Sender<WorkerMessage>,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;

    for (i, config) in fs_devs.iter().enumerate() {
        let fs = Arc::new(Mutex::new(
            devices::virtio::Fs::new(
                config.fs_id.clone(),
                config.shared_dir.clone(),
                exit_code.clone(),
            )
            .unwrap(),
        ));

        let id = format!("{}{}", String::from(fs.lock().unwrap().id()), i);

        // Set no_fsync option if enabled
        #[cfg(target_os = "macos")]
        if config.no_fsync {
            fs.lock().unwrap().set_no_fsync(true);
        }

        if let Some(shm_region) = shm_manager.fs_region(i) {
            fs.lock().unwrap().set_shm_region(VirtioShmRegion {
                host_addr: vmm
                    .guest_memory
                    .get_host_address(shm_region.guest_addr)
                    .map_err(StartMicrovmError::ShmHostAddr)? as u64,
                guest_addr: shm_region.guest_addr.raw_value(),
                size: shm_region.size,
            });
        }

        #[cfg(not(feature = "tee"))]
        if let Some(export_table) = export_table.as_ref() {
            fs.lock().unwrap().set_export_table(export_table.clone());
        }

        #[cfg(target_os = "macos")]
        fs.lock().unwrap().set_map_sender(map_sender.clone());

        // The device mutex mustn't be locked here otherwise it will deadlock.
        attach_mmio_device(vmm, id, intc.clone(), fs).map_err(RegisterFsDevice)?;
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn autoconfigure_console_ports(
    vmm: &mut Vmm,
    vm_resources: &VmResources,
    cfg: Option<&DefaultVirtioConsoleConfig>,
    creating_implicit_console: bool,
) -> std::result::Result<Vec<PortDescription>, StartMicrovmError> {
    use self::StartMicrovmError::*;

    let mut console_output_path: Option<PathBuf> = None;
    if let Some(path) = vm_resources.console_output.clone() {
        if !vm_resources.disable_implicit_console && creating_implicit_console {
            console_output_path = Some(path)
        }
    }

    if let Some(console_output_path) = console_output_path {
        let file = File::create(console_output_path).map_err(OpenConsoleFile)?;
        // Manually emulate our Legacy behavior: In the case of output_path we have always used the
        // stdin to determine the console size
        let stdin_fd = unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) };
        let term_fd = if isatty(stdin_fd).is_ok_and(|v| v) {
            port_io::term_fd(stdin_fd.as_raw_fd()).unwrap()
        } else {
            port_io::term_fixed_size(0, 0)
        };
        Ok(vec![PortDescription::console(
            Some(port_io::input_empty().unwrap()),
            Some(port_io::output_file(file).unwrap()),
            term_fd,
        )])
    } else {
        let (input_fd, output_fd, err_fd) = match cfg {
            Some(c) => (c.input_fd, c.output_fd, c.err_fd),
            None => (STDIN_FILENO, STDOUT_FILENO, STDERR_FILENO),
        };
        let input_is_terminal =
            input_fd >= 0 && isatty(unsafe { BorrowedFd::borrow_raw(input_fd) }).unwrap_or(false);
        let output_is_terminal =
            output_fd >= 0 && isatty(unsafe { BorrowedFd::borrow_raw(output_fd) }).unwrap_or(false);
        let error_is_terminal =
            err_fd >= 0 && isatty(unsafe { BorrowedFd::borrow_raw(err_fd) }).unwrap_or(false);

        let term_fd = if input_is_terminal {
            Some(unsafe { BorrowedFd::borrow_raw(input_fd) })
        } else if output_is_terminal {
            Some(unsafe { BorrowedFd::borrow_raw(output_fd) })
        } else if error_is_terminal {
            Some(unsafe { BorrowedFd::borrow_raw(err_fd) })
        } else {
            None
        };

        let forwarding_sigint;
        let console_input = if input_is_terminal && input_fd >= 0 {
            forwarding_sigint = false;
            Some(port_io::input_to_raw_fd_dup(input_fd).unwrap())
        } else {
            #[cfg(target_os = "linux")]
            {
                forwarding_sigint = true;
                let sigint_input = port_io::PortInputSigInt::new();
                let sigint_input_fd = sigint_input.sigint_evt().as_raw_fd();
                register_sigint_handler(sigint_input_fd).map_err(RegisterFsSigwinch)?;
                Some(Box::new(sigint_input) as _)
            }
            #[cfg(not(target_os = "linux"))]
            {
                forwarding_sigint = false;
                Some(port_io::input_empty().unwrap())
            }
        };

        let console_output = if output_is_terminal && output_fd >= 0 {
            Some(port_io::output_to_raw_fd_dup(output_fd).unwrap())
        } else {
            Some(port_io::output_to_log_as_err())
        };

        let terminal_properties = term_fd
            .map(|fd| port_io::term_fd(fd.as_raw_fd()).unwrap())
            .unwrap_or_else(|| port_io::term_fixed_size(0, 0));

        setup_terminal_raw_mode(vmm, term_fd, forwarding_sigint);

        let mut ports = vec![PortDescription::console(
            console_input,
            console_output,
            terminal_properties,
        )];

        if input_fd >= 0 && !input_is_terminal {
            ports.push(PortDescription::input_pipe(
                "krun-stdin",
                port_io::input_to_raw_fd_dup(input_fd).unwrap(),
            ));
        }

        if output_fd >= 0 && !output_is_terminal {
            ports.push(PortDescription::output_pipe(
                "krun-stdout",
                port_io::output_to_raw_fd_dup(output_fd).unwrap(),
            ));
        };

        if err_fd >= 0 && !error_is_terminal {
            ports.push(PortDescription::output_pipe(
                "krun-stderr",
                port_io::output_to_raw_fd_dup(err_fd).unwrap(),
            ));
        }

        Ok(ports)
    }
}

#[cfg(target_os = "windows")]
fn autoconfigure_console_ports(
    _vmm: &mut Vmm,
    vm_resources: &VmResources,
    cfg: Option<&DefaultVirtioConsoleConfig>,
    creating_implicit_console: bool,
) -> std::result::Result<Vec<PortDescription>, StartMicrovmError> {
    use self::StartMicrovmError::*;

    // Redirect console output to a file if configured (implicit console only).
    if let Some(path) = &vm_resources.console_output {
        if !vm_resources.disable_implicit_console && creating_implicit_console {
            let file = open_windows_console_output_file(path).map_err(OpenConsoleFile)?;
            return Ok(vec![PortDescription::console(
                port_io::input_to_raw_fd_dup(0).ok(),
                Some(port_io::output_file(file).unwrap()),
                port_io::term_fixed_size(0, 0),
            )]);
        }
    }

    let (input_fd, output_fd) = match cfg {
        Some(c) => (c.input_fd, c.output_fd),
        None => (0, 1), // stdin / stdout
    };

    Ok(vec![PortDescription::console(
        if input_fd >= 0 {
            port_io::input_to_raw_fd_dup(input_fd).ok()
        } else {
            None
        },
        if output_fd >= 0 {
            Some(
                port_io::output_to_raw_fd_dup(output_fd)
                    .unwrap_or_else(|_| port_io::output_to_log_as_err()),
            )
        } else {
            None
        },
        port_io::term_fixed_size(0, 0),
    )])
}

#[cfg(not(target_os = "windows"))]
fn setup_terminal_raw_mode(
    vmm: &mut Vmm,
    term_fd: Option<BorrowedFd<'_>>,
    handle_signals_by_terminal: bool,
) {
    if let Some(term_fd) = term_fd {
        match term_set_raw_mode(term_fd, handle_signals_by_terminal) {
            Ok(old_mode) => {
                let raw_fd = term_fd.as_raw_fd();
                vmm.exit_observers.push(Arc::new(Mutex::new(move || {
                    if let Err(e) =
                        term_restore_mode(unsafe { BorrowedFd::borrow_raw(raw_fd) }, &old_mode)
                    {
                        log::error!("Failed to restore terminal mode: {e}")
                    }
                })));
            }
            Err(e) => {
                log::error!("Failed to set terminal to raw mode: {e}")
            }
        };
    }
}

#[cfg(target_os = "windows")]
fn setup_terminal_raw_mode(
    _vmm: &mut Vmm,
    _term_fd: Option<i32>,
    _handle_signals_by_terminal: bool,
) {
}

#[cfg(not(target_os = "windows"))]
fn create_explicit_ports(
    vmm: &mut Vmm,
    port_configs: &[PortConfig],
) -> std::result::Result<Vec<PortDescription>, StartMicrovmError> {
    let mut ports = Vec::with_capacity(port_configs.len());

    for port_cfg in port_configs {
        let port_desc = match port_cfg {
            PortConfig::Tty { name, tty_fd } => {
                assert!(*tty_fd > 0, "PortConfig::Tty must have a valid tty_fd");
                let term_fd = unsafe { BorrowedFd::borrow_raw(*tty_fd) };
                setup_terminal_raw_mode(vmm, Some(term_fd), false);

                PortDescription {
                    name: name.clone().into(),
                    input: Some(port_io::input_to_raw_fd_dup(*tty_fd).unwrap()),
                    output: Some(port_io::output_to_raw_fd_dup(*tty_fd).unwrap()),
                    terminal: Some(port_io::term_fd(*tty_fd).unwrap()),
                }
            }
            PortConfig::InOut {
                name,
                input_fd,
                output_fd,
            } => PortDescription {
                name: name.clone().into(),
                input: if *input_fd < 0 {
                    None
                } else {
                    Some(port_io::input_to_raw_fd_dup(*input_fd).unwrap())
                },
                output: if *output_fd < 0 {
                    None
                } else {
                    Some(port_io::output_to_raw_fd_dup(*output_fd).unwrap())
                },
                terminal: None,
            },
        };

        ports.push(port_desc);
    }

    Ok(ports)
}

#[cfg(target_os = "windows")]
fn create_explicit_ports(
    _vmm: &mut Vmm,
    port_configs: &[PortConfig],
) -> std::result::Result<Vec<PortDescription>, StartMicrovmError> {
    let mut ports = Vec::with_capacity(port_configs.len());
    for port_cfg in port_configs {
        let port_desc = match port_cfg {
            PortConfig::Tty { name, tty_fd } => PortDescription {
                name: name.clone().into(),
                input: if *tty_fd >= 0 {
                    port_io::input_to_raw_fd_dup(*tty_fd)
                        .ok()
                        .map(|i| Arc::new(Mutex::new(i)))
                } else {
                    None
                },
                output: if *tty_fd >= 0 {
                    Some(Arc::new(Mutex::new(
                        port_io::output_to_raw_fd_dup(*tty_fd)
                            .unwrap_or_else(|_| port_io::output_to_log_as_err()),
                    )))
                } else {
                    None
                },
                terminal: Some(port_io::term_fixed_size(0, 0)),
            },
            PortConfig::InOut {
                name,
                input_fd,
                output_fd,
            } => PortDescription {
                name: name.clone().into(),
                input: if *input_fd >= 0 {
                    port_io::input_to_raw_fd_dup(*input_fd)
                        .ok()
                        .map(|i| Arc::new(Mutex::new(i)))
                } else {
                    None
                },
                output: if *output_fd >= 0 {
                    Some(Arc::new(Mutex::new(
                        port_io::output_to_raw_fd_dup(*output_fd)
                            .unwrap_or_else(|_| port_io::output_to_log_as_err()),
                    )))
                } else {
                    None
                },
                terminal: None,
            },
        };
        ports.push(port_desc);
    }
    Ok(ports)
}

fn attach_console_devices(
    vmm: &mut Vmm,
    event_manager: &mut EventManager,
    intc: IrqChip,
    vm_resources: &VmResources,
    cfg: Option<&VirtioConsoleConfigMode>,
    id_number: u32,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;
    #[cfg(target_os = "windows")]
    let _ = event_manager;

    let creating_implicit_console = cfg.is_none();

    let ports = match cfg {
        None => autoconfigure_console_ports(vmm, vm_resources, None, creating_implicit_console)?,
        Some(VirtioConsoleConfigMode::Autoconfigure(autocfg)) => autoconfigure_console_ports(
            vmm,
            vm_resources,
            Some(autocfg),
            creating_implicit_console,
        )?,
        Some(VirtioConsoleConfigMode::Explicit(ports)) => create_explicit_ports(vmm, ports)?,
    };

    let console = Arc::new(Mutex::new(devices::virtio::Console::new(ports).unwrap()));

    #[cfg(not(target_os = "windows"))]
    vmm.exit_observers.push(console.clone());

    event_manager
        .add_subscriber(console.clone())
        .map_err(RegisterEvent)?;

    #[cfg(target_os = "linux")]
    register_sigwinch_handler(console.lock().unwrap().get_sigwinch_fd())
        .map_err(RegisterFsSigwinch)?;

    // The device mutex mustn't be locked here otherwise it will deadlock.
    attach_mmio_device(vmm, format!("hvc{id_number}"), intc, console)
        .map_err(RegisterConsoleDevice)?;

    Ok(())
}

#[cfg(feature = "net")]
fn attach_net_devices(
    vmm: &mut Vmm,
    net_devices: &NetBuilder,
    intc: IrqChip,
) -> Result<(), StartMicrovmError> {
    for net_device in net_devices.list.iter() {
        let id = net_device.lock().unwrap().id().to_string();

        attach_mmio_device(vmm, id, intc.clone(), net_device.clone())
            .map_err(StartMicrovmError::RegisterNetDevice)?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn attach_net_devices_windows(
    vmm: &mut Vmm,
    net_devices: &NetWindowsBuilder,
    event_manager: &mut EventManager,
    intc: IrqChip,
) -> Result<(), StartMicrovmError> {
    for net_device in net_devices.list.iter() {
        let id = net_device.lock().unwrap().id().to_string();
        event_manager
            .add_subscriber(net_device.clone())
            .map_err(StartMicrovmError::RegisterEvent)?;
        attach_mmio_device(vmm, id, intc.clone(), net_device.clone())
            .map_err(StartMicrovmError::RegisterNetDevice)?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn attach_block_devices_windows(
    vmm: &mut Vmm,
    block_devices: &BlockWindowsBuilder,
    event_manager: &mut EventManager,
    intc: IrqChip,
) -> Result<(), StartMicrovmError> {
    for blk_device in block_devices.list.iter() {
        let id = blk_device.lock().unwrap().id().to_string();
        event_manager
            .add_subscriber(blk_device.clone())
            .map_err(StartMicrovmError::RegisterEvent)?;
        attach_mmio_device(vmm, id, intc.clone(), blk_device.clone())
            .map_err(StartMicrovmError::RegisterBlockDevice)?;
    }
    Ok(())
}

fn attach_unixsock_vsock_device(
    vmm: &mut Vmm,
    unix_vsock: &Arc<Mutex<Vsock>>,
    event_manager: &mut EventManager,
    intc: IrqChip,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;
    event_manager
        .add_subscriber(unix_vsock.clone())
        .map_err(RegisterEvent)?;

    let id = String::from(unix_vsock.lock().unwrap().id());

    // The device mutex mustn't be locked here otherwise it will deadlock.
    attach_mmio_device(vmm, id, intc, unix_vsock.clone()).map_err(RegisterVsockDevice)?;

    Ok(())
}

#[cfg(not(feature = "tee"))]
fn attach_balloon_device(
    vmm: &mut Vmm,
    event_manager: &mut EventManager,
    intc: IrqChip,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;
    let balloon = Arc::new(Mutex::new(devices::virtio::Balloon::new().unwrap()));

    event_manager
        .add_subscriber(balloon.clone())
        .map_err(RegisterEvent)?;

    let id = String::from(balloon.lock().unwrap().id());

    // The device mutex mustn't be locked here otherwise it will deadlock.
    attach_mmio_device(vmm, id, intc.clone(), balloon).map_err(RegisterBalloonDevice)?;

    Ok(())
}

#[cfg(feature = "blk")]
fn attach_block_devices(
    vmm: &mut Vmm,
    block_devs: &BlockBuilder,
    intc: IrqChip,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;

    for block in block_devs.list.iter() {
        let id = String::from(block.lock().unwrap().id());

        // The device mutex mustn't be locked here otherwise it will deadlock.
        attach_mmio_device(vmm, id, intc.clone(), block.clone()).map_err(RegisterBlockDevice)?;
    }

    Ok(())
}

#[cfg(not(feature = "tee"))]
fn attach_rng_device(
    vmm: &mut Vmm,
    event_manager: &mut EventManager,
    intc: IrqChip,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;
    let rng = Arc::new(Mutex::new(devices::virtio::Rng::new().unwrap()));

    event_manager
        .add_subscriber(rng.clone())
        .map_err(RegisterEvent)?;

    let id = String::from(rng.lock().unwrap().id());

    // The device mutex mustn't be locked here otherwise it will deadlock.
    attach_mmio_device(vmm, id, intc.clone(), rng).map_err(RegisterRngDevice)?;

    Ok(())
}

#[cfg(feature = "gpu")]
#[allow(clippy::too_many_arguments)]
fn attach_gpu_device(
    vmm: &mut Vmm,
    event_manager: &mut EventManager,
    shm_manager: &mut ShmManager,
    #[cfg(not(feature = "tee"))] mut export_table: Option<ExportTable>,
    intc: IrqChip,
    virgl_flags: u32,
    displays: Box<[DisplayInfo]>,
    display_backend: DisplayBackend<'static>,
    #[cfg(target_os = "macos")] map_sender: Sender<WorkerMessage>,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;

    let gpu = Arc::new(Mutex::new(
        devices::virtio::Gpu::new(
            virgl_flags,
            displays,
            display_backend,
            #[cfg(target_os = "macos")]
            map_sender,
        )
        .unwrap(),
    ));

    event_manager
        .add_subscriber(gpu.clone())
        .map_err(RegisterEvent)?;

    let id = String::from(gpu.lock().unwrap().id());

    if let Some(shm_region) = shm_manager.gpu_region() {
        gpu.lock().unwrap().set_shm_region(VirtioShmRegion {
            host_addr: vmm
                .guest_memory
                .get_host_address(shm_region.guest_addr)
                .map_err(StartMicrovmError::ShmHostAddr)? as u64,
            guest_addr: shm_region.guest_addr.raw_value(),
            size: shm_region.size,
        });
    }

    #[cfg(not(feature = "tee"))]
    if let Some(export_table) = export_table.take() {
        gpu.lock().unwrap().set_export_table(export_table);
    }

    // The device mutex mustn't be locked here otherwise it will deadlock.
    attach_mmio_device(vmm, id, intc, gpu).map_err(RegisterGpuDevice)?;

    Ok(())
}

#[cfg(feature = "input")]
fn attach_input_devices(
    vmm: &mut Vmm,
    input_backends: &[(
        krun_input::InputConfigBackend<'static>,
        krun_input::InputEventProviderBackend<'static>,
    )],
    intc: IrqChip,
) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;

    for (index, (config_backend, events_backend)) in input_backends.iter().enumerate() {
        let input_device = Arc::new(Mutex::new(
            devices::virtio::input::Input::new(*config_backend, *events_backend).unwrap(),
        ));

        let id = format!("input{}", index);
        attach_mmio_device(vmm, id, intc.clone(), input_device).map_err(RegisterInputDevice)?;
    }

    Ok(())
}

#[cfg(feature = "snd")]
fn attach_snd_device(vmm: &mut Vmm, intc: IrqChip) -> std::result::Result<(), StartMicrovmError> {
    use self::StartMicrovmError::*;

    let snd = Arc::new(Mutex::new(devices::virtio::Snd::new().unwrap()));
    let id = String::from(snd.lock().unwrap().id());

    // The device mutex mustn't be locked here otherwise it will deadlock.
    attach_mmio_device(vmm, id, intc, snd).map_err(RegisterSndDevice)?;

    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::vmm_config::kernel_bundle::KernelBundle;

    #[cfg(target_os = "linux")]
    fn default_guest_memory(
        mem_size_mib: usize,
    ) -> std::result::Result<
        (GuestMemoryMmap, ArchMemoryInfo, ShmManager, PayloadConfig),
        StartMicrovmError,
    > {
        let mut vm_resources = VmResources::default();
        vm_resources.kernel_bundle = Some(KernelBundle {
            host_addr: 0x1000,
            guest_addr: 0x1000,
            entry_addr: 0x1000,
            size: 0x1000,
        });

        create_guest_memory(mem_size_mib, &vm_resources, &Payload::Empty)
    }

    #[test]
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    fn test_create_vcpus_x86_64() {
        let vcpu_count = 2;

        let vcpu_config = VcpuConfig {
            vcpu_count,
            ht_enabled: false,
            cpu_template: None,
        };

        let (guest_memory, _arch_memory_info, _shm_manager, _payload_config) =
            default_guest_memory(128).unwrap();
        let vm = setup_vm(&guest_memory, false).unwrap();
        let _kvmioapic = KvmIoapic::new(&vm.fd()).unwrap();

        // Dummy entry_addr, vcpus will not boot.
        let entry_addr = GuestAddress(0);
        let bus = devices::Bus::new();
        let vcpu_vec = create_vcpus_x86_64(
            &vm,
            &vcpu_config,
            &guest_memory,
            entry_addr,
            &bus,
            &EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap(),
            true,
        )
        .unwrap();
        assert_eq!(vcpu_vec.len(), vcpu_count as usize);
    }

    #[test]
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    fn test_create_vcpus_aarch64() {
        let (guest_memory, arch_memory_info, _shm_manager, _payload_config) =
            default_guest_memory(128).unwrap();
        let vm = setup_vm(&guest_memory, false).unwrap();
        let vcpu_count = 2;

        let vcpu_config = VcpuConfig {
            vcpu_count,
            ht_enabled: false,
            cpu_template: None,
        };

        // Dummy entry_addr, vcpus will not boot.
        let entry_addr = GuestAddress(0);
        let vcpu_vec = create_vcpus_aarch64(
            &vm,
            &vcpu_config,
            &arch_memory_info,
            entry_addr,
            &EventFd::new(utils::eventfd::EFD_NONBLOCK).unwrap(),
        )
        .unwrap();
        assert_eq!(vcpu_vec.len(), vcpu_count as usize);
    }

    #[test]
    fn test_error_messages() {
        use crate::builder::StartMicrovmError::*;
        let err = AttachBlockDevice(io::Error::from_raw_os_error(0));
        let _ = format!("{err}{err:?}");

        let err = CreateRateLimiter(io::Error::from_raw_os_error(0));
        let _ = format!("{err}{err:?}");

        let err = Internal(Error::Serial(io::Error::from_raw_os_error(0)));
        let _ = format!("{err}{err:?}");

        #[cfg(not(target_os = "windows"))]
        let err = InvalidKernelBundle(vm_memory::mmap::MmapRegionError::InvalidPointer);
        #[cfg(target_os = "windows")]
        let err = InvalidKernelBundle(io::Error::from_raw_os_error(0));
        let _ = format!("{err}{err:?}");

        let err = KernelCmdline(String::from("dummy --cmdline"));
        let _ = format!("{err}{err:?}");

        let err = LoadCommandline(kernel::cmdline::Error::TooLarge);
        let _ = format!("{err}{err:?}");

        let err = MicroVMAlreadyRunning;
        let _ = format!("{err}{err:?}");

        let err = MissingKernelConfig;
        let _ = format!("{err}{err:?}");

        let err = MissingMemSizeConfig;
        let _ = format!("{err}{err:?}");

        let err = NetDeviceNotConfigured;
        let _ = format!("{err}{err:?}");

        let err = OpenBlockDevice(io::Error::from_raw_os_error(0));
        let _ = format!("{err}{err:?}");

        let err = RegisterBlockDevice(device_manager::mmio::Error::EventFd(
            io::Error::from_raw_os_error(0),
        ));
        let _ = format!("{err}{err:?}");

        let err = RegisterEvent(EventManagerError::EpollCreate(
            io::Error::from_raw_os_error(0),
        ));
        let _ = format!("{err}{err:?}");

        let err = RegisterNetDevice(device_manager::mmio::Error::EventFd(
            io::Error::from_raw_os_error(0),
        ));
        let _ = format!("{err}{err:?}");

        let err = RegisterVsockDevice(device_manager::mmio::Error::EventFd(
            io::Error::from_raw_os_error(0),
        ));
        let _ = format!("{err}{err:?}");
    }

    #[test]
    fn test_kernel_cmdline_err_to_startuvm_err() {
        let err = StartMicrovmError::from(kernel::cmdline::Error::HasSpace);
        let _ = format!("{err}{err:?}");
    }
}
