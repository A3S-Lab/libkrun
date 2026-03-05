// Copyright 2024 The libkrun Authors.
// SPDX-License-Identifier: Apache-2.0

//! Builder for Windows virtio-net devices.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use devices::virtio::NetWindows;

/// Configuration for a single Windows virtio-net device.
pub struct NetWindowsConfig {
    /// Unique ID used to register the device with the MMIO manager.
    pub iface_id: String,
    /// 6-byte MAC address advertised to the guest.
    pub mac: [u8; 6],
    /// Optional TCP endpoint to connect to for packet I/O.
    ///
    /// When `None` the device drops all TX frames and never produces RX frames.
    pub tcp_addr: Option<SocketAddr>,
}

/// Errors that can occur when configuring a Windows net device.
#[derive(Debug)]
pub enum NetWindowsError {
    /// Failed to connect the TCP backend.
    ConnectBackend(io::Error),
    /// Failed to create the net device.
    CreateDevice(io::Error),
}

impl fmt::Display for NetWindowsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            NetWindowsError::ConnectBackend(e) => write!(f, "TCP backend connect failed: {e}"),
            NetWindowsError::CreateDevice(e) => write!(f, "NetWindows device creation failed: {e}"),
        }
    }
}

/// Builds and holds the list of Windows virtio-net devices.
#[derive(Default)]
pub struct NetWindowsBuilder {
    pub list: VecDeque<Arc<Mutex<NetWindows>>>,
}

impl NetWindowsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a `NetWindows` from `config` and append it to the device list.
    pub fn insert(&mut self, config: NetWindowsConfig) -> Result<(), NetWindowsError> {
        let backend = config
            .tcp_addr
            .map(std::net::TcpStream::connect)
            .transpose()
            .map_err(NetWindowsError::ConnectBackend)?;

        let dev = NetWindows::new(config.iface_id, config.mac, backend)
            .map_err(NetWindowsError::CreateDevice)?;

        self.list.push_back(Arc::new(Mutex::new(dev)));
        Ok(())
    }
}
