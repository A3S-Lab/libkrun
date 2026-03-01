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
//! 4. The caller handles the exit via `Vcpu::run_emulation()`
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
//! ```no_run
//! # use windows::Win32::System::Hypervisor::WHV_PARTITION_HANDLE;
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

use std::io;
use utils::time::timestamp_cycles;
use windows::Win32::System::Hypervisor::{
    WHvCreateVirtualProcessor, WHvDeleteVirtualProcessor, WHvGetVirtualProcessorRegisters,
    WHvMemoryAccessRead, WHvMemoryAccessWrite, WHvRunVirtualProcessor, WHvRunVpExitReasonCanceled,
    WHvRunVpExitReasonException, WHvRunVpExitReasonHypercall,
    WHvRunVpExitReasonInvalidVpRegisterValue, WHvRunVpExitReasonMemoryAccess,
    WHvRunVpExitReasonSynicSintDeliverable, WHvRunVpExitReasonUnrecoverableException,
    WHvRunVpExitReasonUnsupportedFeature, WHvRunVpExitReasonX64ApicEoi,
    WHvRunVpExitReasonX64ApicInitSipiTrap, WHvRunVpExitReasonX64ApicSmiTrap,
    WHvRunVpExitReasonX64ApicWriteTrap, WHvRunVpExitReasonX64Cpuid, WHvRunVpExitReasonX64Halt,
    WHvRunVpExitReasonX64InterruptWindow, WHvRunVpExitReasonX64IoPortAccess,
    WHvRunVpExitReasonX64MsrAccess, WHvRunVpExitReasonX64Rdtsc, WHvSetVirtualProcessorRegisters,
    WHvX64ExceptionTypeBreakpointTrap, WHvX64ExceptionTypeOverflowTrap, WHvX64RegisterRax,
    WHvX64RegisterRbx, WHvX64RegisterRcx, WHvX64RegisterRdx, WHvX64RegisterRip,
    WHV_PARTITION_HANDLE, WHV_REGISTER_NAME, WHV_REGISTER_VALUE, WHV_RUN_VP_EXIT_CONTEXT,
};

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
    /// Buffer for MMIO/IO port data transfer.
    data_buffer: [u8; 8],
    pending_io_read: Option<PendingIoRead>,
    pending_io_write: Option<PendingIoWrite>,
    pending_mmio_read: Option<PendingMmioRead>,
    pending_mmio_write: Option<PendingMmioWrite>,
}

#[derive(Debug, Clone, Copy)]
struct PendingIoRead {
    size: usize,
    next_rip: u64,
}

#[derive(Debug, Clone, Copy)]
struct PendingIoWrite {
    next_rip: u64,
}

#[derive(Debug, Clone, Copy)]
struct PendingMmioRead {
    size: usize,
    next_rip: u64,
    reg_index: u8,
    high8: bool,
    write_full: bool,
    sign_extend: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingMmioWrite {
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
}

impl WhpxVcpu {
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
        let values = [WHV_REGISTER_VALUE { Reg64: next_rip }];
        self.set_registers(&names, &values)
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
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to get vCPU register {}: {}", reg_index, e),
                )
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

    fn decode_mmio_access(
        rip: u64,
        instruction_bytes: &[u8],
        access_size: usize,
        is_write: bool,
    ) -> io::Result<DecodedMmioAccess> {
        let mut idx = 0;
        let mut rex: u8 = 0;

        while let Some(&b) = instruction_bytes.get(idx) {
            if Self::is_legacy_prefix(b) {
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

            let reg_base = ((modrm >> 3) & 0x7) as u8;
            let rex_r = ((rex >> 2) & 1) as u8;
            let reg_index = reg_base + (rex_r << 3);
            let next_rip =
                rip.wrapping_add(instruction_bytes.len().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len")
                })?);

            let kind = match opcode2 {
                // Prefetch variants: memory-touching hints with no architectural side effects.
                0x0d | 0x18 | 0x1f => MmioAccessKind::Noop,
                0xb6 | 0xb7 if !is_write => MmioAccessKind::ReadRegZeroExtend { reg_index },
                0xbe | 0xbf if !is_write => MmioAccessKind::ReadRegSignExtend { reg_index },
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!(
                            "Unsupported MMIO instruction opcode 0x0f 0x{opcode2:02x} (is_write={is_write})"
                        ),
                    ));
                }
            };

            return Ok(DecodedMmioAccess { kind, next_rip });
        }

        // moffs forms: mov AL/AX/EAX/RAX, moffs and mov moffs, AL/AX/EAX/RAX.
        if matches!(opcode, 0xa0 | 0xa1 | 0xa2 | 0xa3) {
            let next_rip =
                rip.wrapping_add(instruction_bytes.len().try_into().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len")
                })?);

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

            return Ok(DecodedMmioAccess { kind, next_rip });
        }

        let modrm = *instruction_bytes.get(idx).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Missing ModRM in MMIO instruction",
            )
        })?;
        idx += 1;

        let reg_base = ((modrm >> 3) & 0x7) as u8;
        let rex_r = ((rex >> 2) & 1) as u8;
        let reg_extended = reg_base + (rex_r << 3);

        let next_rip = rip.wrapping_add(
            instruction_bytes
                .len()
                .try_into()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Bad instruction len"))?,
        );

        let kind = match opcode {
            0x8a | 0x8b if !is_write => {
                let high8 = access_size == 1 && rex == 0 && (4..=7).contains(&reg_base);
                let reg_index = if high8 { reg_base - 4 } else { reg_extended };
                MmioAccessKind::ReadReg { reg_index, high8 }
            }
            0x63 if !is_write => MmioAccessKind::ReadRegSignExtend {
                reg_index: reg_extended,
            },
            0x88 | 0x89 if is_write => {
                let high8 = access_size == 1 && rex == 0 && (4..=7).contains(&reg_base);
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
                let imm_len = if access_size == 2 { 2 } else { 4 };
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
                    if access_size == 8 {
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

        Ok(DecodedMmioAccess { kind, next_rip })
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
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to set vCPU registers: {}", e),
                )
            })
        }
    }

    fn emulate_cpuid(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) -> io::Result<()> {
        let cpuid = unsafe { exit_context.Anonymous.CpuidAccess };
        let next_rip = exit_context.VpContext.Rip.wrapping_add(2);

        let names = [
            WHvX64RegisterRax,
            WHvX64RegisterRbx,
            WHvX64RegisterRcx,
            WHvX64RegisterRdx,
            WHvX64RegisterRip,
        ];
        let values = [
            WHV_REGISTER_VALUE {
                Reg64: cpuid.DefaultResultRax,
            },
            WHV_REGISTER_VALUE {
                Reg64: cpuid.DefaultResultRbx,
            },
            WHV_REGISTER_VALUE {
                Reg64: cpuid.DefaultResultRcx,
            },
            WHV_REGISTER_VALUE {
                Reg64: cpuid.DefaultResultRdx,
            },
            WHV_REGISTER_VALUE { Reg64: next_rip },
        ];

        self.set_registers(&names, &values)
    }

    fn emulate_msr(&self, exit_context: &WHV_RUN_VP_EXIT_CONTEXT) -> io::Result<()> {
        let msr = unsafe { exit_context.Anonymous.MsrAccess };
        let is_write = unsafe { msr.AccessInfo.AsUINT32 } & 1 != 0;
        let next_rip = exit_context.VpContext.Rip.wrapping_add(2);

        if is_write {
            let names = [WHvX64RegisterRip];
            let values = [WHV_REGISTER_VALUE { Reg64: next_rip }];
            self.set_registers(&names, &values)
        } else {
            let read_value: u64 = match msr.MsrNumber {
                // IA32_TSC (0x10): return a monotonic host value.
                0x10 => timestamp_cycles(),
                // Default to zero for currently unsupported virtual MSRs.
                _ => 0,
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
        self.set_registers(&names, &values)
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
    pub fn new(partition: WHV_PARTITION_HANDLE, index: u32) -> io::Result<Self> {
        // SAFETY: We assume the caller has provided a valid partition handle.
        // The partition must remain valid for the lifetime of this vCPU (documented in struct).
        // The third parameter (0) represents flags, with 0 meaning default behavior.
        unsafe {
            WHvCreateVirtualProcessor(partition, index, 0 /* flags: default behavior */).map_err(
                |e| {
                    io::Error::new(
                        io::ErrorKind::Other,
                        format!("Failed to create vCPU: {}", e),
                    )
                },
            )?;
        }

        Ok(Self {
            partition,
            index,
            data_buffer: [0; 8],
            pending_io_read: None,
            pending_io_write: None,
            pending_mmio_read: None,
            pending_mmio_write: None,
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

        let names = [Self::gpr_name(pending.reg_index)?, WHvX64RegisterRip];
        let values = [
            WHV_REGISTER_VALUE { Reg64: merged },
            WHV_REGISTER_VALUE {
                Reg64: pending.next_rip,
            },
        ];
        self.set_registers(&names, &values)
    }

    pub fn complete_mmio_write(&mut self) -> io::Result<()> {
        let pending = self.pending_mmio_write.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "No pending WHPX MMIO write exit",
            )
        })?;

        let names = [WHvX64RegisterRip];
        let values = [WHV_REGISTER_VALUE {
            Reg64: pending.next_rip,
        }];
        self.set_registers(&names, &values)
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

        let current_rax = self.get_register_u64(0)?;
        let merged_rax = Self::merge_reg_bits(current_rax, pending.size, false, value)?;

        let names = [WHvX64RegisterRax, WHvX64RegisterRip];
        let values = [
            WHV_REGISTER_VALUE { Reg64: merged_rax },
            WHV_REGISTER_VALUE {
                Reg64: pending.next_rip,
            },
        ];
        self.set_registers(&names, &values)
    }

    pub fn complete_io_write(&mut self) -> io::Result<()> {
        let pending = self.pending_io_write.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "No pending WHPX I/O write exit",
            )
        })?;

        let names = [WHvX64RegisterRip];
        let values = [WHV_REGISTER_VALUE {
            Reg64: pending.next_rip,
        }];
        self.set_registers(&names, &values)
    }

    pub fn clear_pending_io(&mut self) {
        self.pending_io_read = None;
        self.pending_io_write = None;
    }

    pub fn clear_pending_mmio(&mut self) {
        self.pending_mmio_read = None;
        self.pending_mmio_write = None;
    }

    /// Runs the virtual CPU until a VM exit occurs.
    ///
    /// # Returns
    /// Returns a `VcpuExit` describing why the vCPU stopped executing.
    ///
    /// # Errors
    /// Returns an error if running the vCPU fails.
    pub fn run(&mut self) -> io::Result<VcpuExit<'_>> {
        loop {
            let mut exit_context = WHV_RUN_VP_EXIT_CONTEXT::default();

            // SAFETY: WHvRunVirtualProcessor is safe to call with valid partition and vCPU handles.
            // The exit_context is a valid mutable reference that will be filled by the API.
            unsafe {
                WHvRunVirtualProcessor(
                    self.partition,
                    self.index,
                    (&mut exit_context as *mut WHV_RUN_VP_EXIT_CONTEXT).cast(),
                    std::mem::size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32,
                )
                .map_err(|e| {
                    io::Error::new(io::ErrorKind::Other, format!("Failed to run vCPU: {}", e))
                })?;
            }

            // Parse the exit reason.
            match exit_context.ExitReason {
                reason if reason == WHvRunVpExitReasonMemoryAccess => {
                    let memory_access = unsafe { exit_context.Anonymous.MemoryAccess };
                    let gpa = memory_access.Gpa;
                    let access_info = unsafe { memory_access.AccessInfo.AsUINT32 };
                    let access_type = (access_info & 0x3) as i32;
                    let access_size = (((access_info >> 4) & 0xf) as usize).max(1);
                    if access_size > self.data_buffer.len() {
                        warn!(
                            "Unsupported WHPX MMIO access size {} at gpa=0x{gpa:x}",
                            access_size
                        );
                        return Ok(VcpuExit::Shutdown);
                    }
                    let instruction_len = memory_access.InstructionByteCount as usize;
                    let instruction_bytes = memory_access
                        .InstructionBytes
                        .get(..instruction_len)
                        .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Invalid WHPX MMIO instruction length",
                        )
                    })?;

                    match access_type {
                        x if x == WHvMemoryAccessRead.0 => {
                            let decoded = match Self::decode_mmio_access(
                                exit_context.VpContext.Rip,
                                instruction_bytes,
                                access_size,
                                false,
                            ) {
                                Ok(decoded) => decoded,
                                Err(e) => {
                                    warn!(
                                        "WHPX MMIO read decode failed (gpa=0x{gpa:x}, size={access_size}): {e}"
                                    );
                                    return Ok(VcpuExit::Shutdown);
                                }
                            };
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
                            self.pending_mmio_read = Some(PendingMmioRead {
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
                                exit_context.VpContext.Rip,
                                instruction_bytes,
                                access_size,
                                true,
                            ) {
                                Ok(decoded) => decoded,
                                Err(e) => {
                                    warn!(
                                        "WHPX MMIO write decode failed (gpa=0x{gpa:x}, size={access_size}): {e}"
                                    );
                                    return Ok(VcpuExit::Shutdown);
                                }
                            };
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

                            for i in 0..access_size {
                                self.data_buffer[i] = ((write_value >> (i * 8)) & 0xff) as u8;
                            }

                            self.pending_mmio_write = Some(PendingMmioWrite {
                                next_rip: decoded.next_rip,
                            });
                            self.pending_mmio_read = None;
                            return Ok(VcpuExit::MmioWrite(gpa, &self.data_buffer[..access_size]));
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
                    let next_rip = exit_context
                        .VpContext
                        .Rip
                        .wrapping_add(io_port.InstructionByteCount as u64);

                    if string_op || rep_prefix {
                        // Best-effort compatibility path for debug/legacy serial ports.
                        if Self::allow_string_io_fallback(port) {
                            if rep_prefix {
                                // Treat REP string I/O as fully consumed to avoid re-executing
                                // the same instruction in tight debug output loops.
                                let names = [WHvX64RegisterRip, WHvX64RegisterRcx];
                                let values = [
                                    WHV_REGISTER_VALUE { Reg64: next_rip },
                                    WHV_REGISTER_VALUE { Reg64: 0 },
                                ];
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
                        self.pending_io_write = Some(PendingIoWrite { next_rip });
                        return Ok(VcpuExit::IoPortWrite(port, &self.data_buffer[..size]));
                    } else {
                        self.pending_io_read = Some(PendingIoRead { size, next_rip });
                        return Ok(VcpuExit::IoPortRead(port, &mut self.data_buffer[..size]));
                    }
                }
                reason if reason == WHvRunVpExitReasonX64Cpuid => {
                    self.emulate_cpuid(&exit_context)?;
                }
                reason if reason == WHvRunVpExitReasonX64MsrAccess => {
                    self.emulate_msr(&exit_context)?;
                }
                reason if reason == WHvRunVpExitReasonX64Rdtsc => {
                    self.emulate_rdtsc(&exit_context)?;
                }
                reason if reason == WHvRunVpExitReasonX64InterruptWindow => {
                    // No explicit action needed; resume execution.
                }
                reason if reason == WHvRunVpExitReasonX64ApicEoi => {
                    // No explicit action needed for now.
                }
                reason if reason == windows::Win32::System::Hypervisor::WHvRunVpExitReasonNone => {
                    // No state changes; re-enter VP run loop.
                }
                reason
                    if reason == WHvRunVpExitReasonUnsupportedFeature
                        || reason == WHvRunVpExitReasonInvalidVpRegisterValue
                        || reason == WHvRunVpExitReasonSynicSintDeliverable =>
                {
                    warn!(
                        "Unsupported WHPX synthetic/hypercall exit (reason={}): stopping vCPU",
                        reason.0
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonX64ApicWriteTrap => {
                    let apic_write = unsafe { exit_context.Anonymous.ApicWrite };
                    warn!(
                        "WHPX APIC write trap (type={}, value=0x{:x}): stopping vCPU",
                        apic_write.Type.0, apic_write.WriteValue
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonX64ApicInitSipiTrap => {
                    let init_sipi = unsafe { exit_context.Anonymous.ApicInitSipi };
                    warn!(
                        "WHPX APIC INIT/SIPI trap (icr=0x{:x}): stopping vCPU",
                        init_sipi.ApicIcr
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonX64ApicSmiTrap => {
                    let apic_smi = unsafe { exit_context.Anonymous.ApicSmi };
                    warn!(
                        "WHPX APIC SMI trap at GPA 0x{:x}: stopping vCPU",
                        apic_smi.ApicIcr
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonHypercall => {
                    let hypercall = unsafe { exit_context.Anonymous.Hypercall };
                    warn!(
                        "WHPX hypercall exit (rax=0x{:x}, rbx=0x{:x}): stopping vCPU",
                        hypercall.Rax, hypercall.Rbx
                    );
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonX64Halt => return Ok(VcpuExit::Halted),
                reason if reason == WHvRunVpExitReasonCanceled => return Ok(VcpuExit::Shutdown),
                reason if reason == WHvRunVpExitReasonException => {
                    if self.emulate_exception(&exit_context)? {
                        continue;
                    }
                    warn!("Unhandled WHPX exception exit: stopping vCPU");
                    return Ok(VcpuExit::Shutdown);
                }
                reason if reason == WHvRunVpExitReasonUnrecoverableException => {
                    return Ok(VcpuExit::Shutdown);
                }
                other => {
                    warn!("Unsupported WHPX exit reason {}: stopping vCPU", other.0);
                    return Ok(VcpuExit::Shutdown);
                }
            }
        }
    }
}

impl Drop for WhpxVcpu {
    fn drop(&mut self) {
        // SAFETY: WHvDeleteVirtualProcessor is safe to call with valid handles.
        // We ignore errors because Drop cannot fail, and the vCPU may already be
        // in an invalid state during cleanup.
        unsafe {
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
        let decoded = WhpxVcpu::decode_mmio_access(0x1000, &[0x0f, 0x18, 0x00], 1, false).unwrap();
        assert_eq!(decoded.next_rip, 0x1003);
        assert!(matches!(decoded.kind, MmioAccessKind::Noop));
    }

    #[test]
    fn test_decode_mmio_access_movzx_and_movsxd() {
        let decoded_movzx =
            WhpxVcpu::decode_mmio_access(0x2000, &[0x0f, 0xb6, 0x18], 1, false).unwrap();
        assert_eq!(decoded_movzx.next_rip, 0x2003);
        assert!(matches!(
            decoded_movzx.kind,
            MmioAccessKind::ReadRegZeroExtend { reg_index: 3 }
        ));

        let decoded_movsxd =
            WhpxVcpu::decode_mmio_access(0x3000, &[0x44, 0x63, 0x08], 4, false).unwrap();
        assert_eq!(decoded_movsxd.next_rip, 0x3003);
        assert!(matches!(
            decoded_movsxd.kind,
            MmioAccessKind::ReadRegSignExtend { reg_index: 9 }
        ));

        // Legacy high-8 register encoding without REX.
        let decoded_high8 = WhpxVcpu::decode_mmio_access(0x3100, &[0x8a, 0x20], 1, false).unwrap();
        assert_eq!(decoded_high8.next_rip, 0x3102);
        assert!(matches!(
            decoded_high8.kind,
            MmioAccessKind::ReadReg {
                reg_index: 0,
                high8: true
            }
        ));

        // With REX prefix the same reg field maps to extended register, not high-8.
        let decoded_rex =
            WhpxVcpu::decode_mmio_access(0x3200, &[0x44, 0x8a, 0x20], 1, false).unwrap();
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
            WhpxVcpu::decode_mmio_access(0x4000, &[0xc6, 0x05, 0, 0, 0, 0, 0x7f], 1, true).unwrap();
        assert_eq!(c6.next_rip, 0x4007);
        assert!(matches!(c6.kind, MmioAccessKind::WriteImm { value: 0x7f }));

        let c7 = WhpxVcpu::decode_mmio_access(
            0x5000,
            &[0xc7, 0x05, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
            8,
            true,
        )
        .unwrap();
        assert_eq!(c7.next_rip, 0x500a);
        assert!(matches!(
            c7.kind,
            MmioAccessKind::WriteImm { value: u64::MAX }
        ));

        // moffs write form should map to RAX register source.
        let moffs_write =
            WhpxVcpu::decode_mmio_access(0x5100, &[0xa3, 0, 0, 0, 0], 8, true).unwrap();
        assert_eq!(moffs_write.next_rip, 0x5105);
        assert!(matches!(
            moffs_write.kind,
            MmioAccessKind::WriteReg {
                reg_index: 0,
                high8: false
            }
        ));

        // C7 with 16-bit immediate uses imm16 width.
        let c7_imm16 =
            WhpxVcpu::decode_mmio_access(0x5200, &[0xc7, 0x05, 0, 0, 0, 0, 0x34, 0x12], 2, true)
                .unwrap();
        assert_eq!(c7_imm16.next_rip, 0x5208);
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
                WhpxVcpu::decode_mmio_access(case.rip, case.bytes, case.access_size, case.is_write)
                    .unwrap();
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
                WhpxVcpu::decode_mmio_access(case.rip, case.bytes, case.access_size, case.is_write)
                    .unwrap();
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
            let res =
                WhpxVcpu::decode_mmio_access(0x7000, case.bytes, case.access_size, case.is_write);
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
            let res =
                WhpxVcpu::decode_mmio_access(0x7100, case.bytes, case.access_size, case.is_write);
            assert!(matches!(res, Err(err) if err.kind() == case.kind));
        }
    }

    #[test]
    fn test_decode_mmio_access_errors() {
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[], 1, false),
            Err(err) if err.kind() == io::ErrorKind::InvalidData
        ));

        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xa0], 1, true),
            Err(err) if err.kind() == io::ErrorKind::Unsupported
        ));

        // Unsupported ModRM extension for C6/C7 immediate write forms.
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xc6, 0x08, 0x12], 1, true),
            Err(err) if err.kind() == io::ErrorKind::Unsupported
        ));
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xc7, 0x08, 0, 0, 0, 0], 4, true),
            Err(err) if err.kind() == io::ErrorKind::Unsupported
        ));

        // Immediate bytes missing.
        assert!(matches!(
            WhpxVcpu::decode_mmio_access(0x0, &[0xc7, 0x05, 0, 0, 0, 0], 4, true),
            Err(err) if err.kind() == io::ErrorKind::InvalidData
        ));

        // next_rip must wrap correctly on overflow.
        let wrapped = WhpxVcpu::decode_mmio_access(u64::MAX, &[0x8a, 0x00], 1, false).unwrap();
        assert_eq!(wrapped.next_rip, 1);
    }
}
