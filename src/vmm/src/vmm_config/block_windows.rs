// Copyright 2024 The libkrun Authors.
// SPDX-License-Identifier: Apache-2.0

//! Builder for Windows virtio-blk devices.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};

use devices::virtio::BlockWindows;

/// Configuration for a single Windows virtio-blk device.
pub struct BlockWindowsConfig {
    /// Unique ID used to register the device with the MMIO manager.
    pub block_id: String,
    /// Path to the raw disk image on the host.
    pub disk_image_path: String,
    /// Whether the device is read-only.
    pub is_disk_read_only: bool,
}

/// Errors that can occur when configuring a Windows block device.
#[derive(Debug)]
pub enum BlockWindowsError {
    /// Failed to open the disk image.
    OpenDisk(io::Error),
}

impl fmt::Display for BlockWindowsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            BlockWindowsError::OpenDisk(e) => write!(f, "BlockWindows disk open failed: {e}"),
        }
    }
}

/// Builds and holds the list of Windows virtio-blk devices.
#[derive(Default)]
pub struct BlockWindowsBuilder {
    pub list: VecDeque<Arc<Mutex<BlockWindows>>>,
}

impl BlockWindowsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a `BlockWindows` from `config` and append it to the device list.
    pub fn insert(&mut self, config: BlockWindowsConfig) -> Result<(), BlockWindowsError> {
        let dev =
            BlockWindows::new(config.block_id, &config.disk_image_path, config.is_disk_read_only)
                .map_err(BlockWindowsError::OpenDisk)?;
        self.list.push_back(Arc::new(Mutex::new(dev)));
        Ok(())
    }
}
