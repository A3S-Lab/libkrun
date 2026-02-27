// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! WHPX vCPU implementation for x86_64 architecture.
//!
//! This module provides the WhpxVcpu wrapper around Windows Hypervisor Platform
//! virtual processor APIs, handling VM exits and vCPU execution.

use windows::Win32::System::Hypervisor::WHV_PARTITION_HANDLE;

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
