// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! WHPX vCPU implementation for x86_64 architecture.
//!
//! This module provides the WhpxVcpu wrapper around Windows Hypervisor Platform
//! virtual processor APIs, handling VM exits and vCPU execution.

use windows::Win32::System::Hypervisor::WHV_PARTITION_HANDLE;

/// Represents a WHPX virtual CPU.
pub struct WhpxVcpu {
    /// Handle to the WHPX partition this vCPU belongs to
    partition: WHV_PARTITION_HANDLE,
    /// Index of this vCPU within the partition
    index: u32,
}
