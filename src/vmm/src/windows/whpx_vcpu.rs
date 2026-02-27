// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! WHPX vCPU implementation for x86_64 architecture.
//!
//! This module provides the WhpxVcpu wrapper around Windows Hypervisor Platform
//! virtual processor APIs, handling VM exits and vCPU execution.

use std::io;
use windows::Win32::System::Hypervisor::{
    WHvCreateVirtualProcessor, WHvDeleteVirtualProcessor, WHvRunVirtualProcessor,
    WHV_PARTITION_HANDLE, WHV_RUN_VP_EXIT_CONTEXT,
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

        Ok(Self { partition, index })
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

        // TODO: Parse exit_context and return appropriate VcpuExit variant
        // For now, return Shutdown as a placeholder
        Ok(VcpuExit::Shutdown)
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
