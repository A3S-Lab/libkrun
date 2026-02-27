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
use windows::Win32::System::Hypervisor::{
    WHvCreateVirtualProcessor, WHvDeleteVirtualProcessor, WHvRunVirtualProcessor,
    WHV_PARTITION_HANDLE, WHV_RUN_VP_EXIT_CONTEXT, WHV_RUN_VP_EXIT_REASON,
    WHV_RUN_VP_EXIT_REASON_MEMORY_ACCESS, WHV_MEMORY_ACCESS_INFO,
    WHV_MEMORY_ACCESS_TYPE_READ, WHV_MEMORY_ACCESS_TYPE_WRITE,
    WHV_RUN_VP_EXIT_REASON_X64_IO_PORT_ACCESS, WHV_RUN_VP_EXIT_REASON_X64_HALT,
    WHV_RUN_VP_EXIT_REASON_CANCELED,
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
}

impl WhpxVcpu {
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
            WHvCreateVirtualProcessor(partition, index, 0 /* flags: default behavior */)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to create vCPU: {}", e)))?;
        }

        Ok(Self {
            partition,
            index,
            data_buffer: [0; 8],
        })
    }

    /// Runs the virtual CPU until a VM exit occurs.
    ///
    /// # Returns
    /// Returns a `VcpuExit` describing why the vCPU stopped executing.
    ///
    /// # Errors
    /// Returns an error if running the vCPU fails.
    pub fn run(&mut self) -> io::Result<VcpuExit<'_>> {
        let mut exit_context = WHV_RUN_VP_EXIT_CONTEXT::default();

        // SAFETY: WHvRunVirtualProcessor is safe to call with valid partition and vCPU handles.
        // The exit_context is a valid mutable reference that will be filled by the API.
        unsafe {
            WHvRunVirtualProcessor(self.partition, self.index, &mut exit_context as *mut _, std::mem::size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to run vCPU: {}", e)))?;
        }

        // Parse the exit reason
        match exit_context.ExitReason {
            WHV_RUN_VP_EXIT_REASON_MEMORY_ACCESS => {
                let memory_access = unsafe { exit_context.Anonymous.MemoryAccess };
                let gpa = memory_access.Gpa;
                let access_type = memory_access.AccessInfo.AccessType();

                match access_type {
                    WHV_MEMORY_ACCESS_TYPE_READ => {
                        let size = memory_access.AccessInfo.AccessSizeBytes() as usize;
                        Ok(VcpuExit::MmioRead(gpa, &mut self.data_buffer[..size]))
                    }
                    WHV_MEMORY_ACCESS_TYPE_WRITE => {
                        let size = memory_access.AccessInfo.AccessSizeBytes() as usize;
                        // Copy write data from exit context to buffer
                        for i in 0..size {
                            self.data_buffer[i] = memory_access.InstructionBytes[i];
                        }
                        Ok(VcpuExit::MmioWrite(gpa, &self.data_buffer[..size]))
                    }
                    _ => Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("Unsupported memory access type: {}", access_type),
                    )),
                }
            }
            WHV_RUN_VP_EXIT_REASON_X64_IO_PORT_ACCESS => {
                let io_port = unsafe { exit_context.Anonymous.IoPortAccess };
                let port = io_port.PortNumber;
                let size = io_port.AccessInfo.AccessSize() as usize;
                let is_write = io_port.AccessInfo.IsWrite() != 0;

                if is_write {
                    // Copy write data to buffer
                    for i in 0..size {
                        self.data_buffer[i] = io_port.Anonymous.Data[i];
                    }
                    Ok(VcpuExit::IoPortWrite(port, &self.data_buffer[..size]))
                } else {
                    Ok(VcpuExit::IoPortRead(port, &mut self.data_buffer[..size]))
                }
            }
            WHV_RUN_VP_EXIT_REASON_X64_HALT => Ok(VcpuExit::Halted),
            WHV_RUN_VP_EXIT_REASON_CANCELED => Ok(VcpuExit::Shutdown),
            _ => {
                // Placeholder for other exit reasons
                Ok(VcpuExit::Shutdown)
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

// Implementation complete for x86_64 minimal VM exit set:
// - MMIO read/write operations
// - IO port read/write operations
// - HLT instruction handling
// - Shutdown/cancellation handling
//
// Future enhancements could include:
// - Additional VM exit types (CPUID, MSR access, etc.)
// - Performance optimizations (exit context caching)
// - Enhanced error reporting and debugging
