// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! WHPX vCPU implementation for x86_64 architecture.
//!
//! This module provides the WhpxVcpu wrapper around Windows Hypervisor Platform
//! virtual processor APIs, handling VM exits and vCPU execution.

use std::io;
use windows::Win32::System::Hypervisor::{
    WHvCreateVirtualProcessor, WHvDeleteVirtualProcessor, WHV_PARTITION_HANDLE,
};

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
        // SAFETY: WHvCreateVirtualProcessor is safe to call with a valid partition handle
        unsafe {
            WHvCreateVirtualProcessor(partition, index, 0)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to create vCPU: {}", e)))?;
        }

        Ok(Self { partition, index })
    }
}

impl Drop for WhpxVcpu {
    fn drop(&mut self) {
        // SAFETY: WHvDeleteVirtualProcessor is safe to call with valid handles
        unsafe {
            let _ = WHvDeleteVirtualProcessor(self.partition, self.index);
        }
    }
}
