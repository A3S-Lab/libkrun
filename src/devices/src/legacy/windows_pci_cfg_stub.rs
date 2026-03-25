// Copyright 2026
// SPDX-License-Identifier: Apache-2.0

use crate::BusDevice;
use std::sync::atomic::{AtomicU64, Ordering};

const CONFIG_DATA_PORT_OFFSET: usize = 4;
const CONFIG_IO_WINDOW_SIZE: usize = 8;
const CONFIG_ENABLE_BIT: u32 = 1 << 31;

const ROOT_BUS: u8 = 0;
const ROOT_DEVICE: u8 = 0;
const ROOT_FUNCTION: u8 = 0;

// A minimal synthetic PCI host bridge is enough for Linux to accept direct
// configuration mechanism #1 probing during early boot.
const ROOT_VENDOR_ID: u16 = 0x8086;
const ROOT_DEVICE_ID: u16 = 0x1237;
const PCI_CLASS_HOST_BRIDGE: u8 = 0x06;
const PCI_SUBCLASS_HOST_BRIDGE: u8 = 0x00;

#[derive(Debug, Clone)]
pub struct PciConfigIoStub {
    config_address: u32,
    root_bridge_config: [u8; 256],
}

impl Default for PciConfigIoStub {
    fn default() -> Self {
        Self::new()
    }
}

impl PciConfigIoStub {
    fn debug_log(message: impl AsRef<str>) {
        use std::io::Write;

        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(r"C:\Users\18770\.a3s\libkrun-whpx-io-current.log")
        {
            let _ = writeln!(file, "{}", message.as_ref());
        }
    }

    pub fn new() -> Self {
        let mut root_bridge_config = [0xff; 256];
        root_bridge_config[0x00..0x02].copy_from_slice(&ROOT_VENDOR_ID.to_le_bytes());
        root_bridge_config[0x02..0x04].copy_from_slice(&ROOT_DEVICE_ID.to_le_bytes());
        root_bridge_config[0x08] = 0x00;
        root_bridge_config[0x09] = 0x00;
        root_bridge_config[0x0a] = PCI_SUBCLASS_HOST_BRIDGE;
        root_bridge_config[0x0b] = PCI_CLASS_HOST_BRIDGE;
        root_bridge_config[0x0e] = 0x00;

        Self {
            config_address: 0,
            root_bridge_config,
        }
    }

    fn selected_bdf(&self) -> Option<(u8, u8, u8)> {
        if self.config_address & CONFIG_ENABLE_BIT == 0 {
            return None;
        }

        Some((
            ((self.config_address >> 16) & 0xff) as u8,
            ((self.config_address >> 11) & 0x1f) as u8,
            ((self.config_address >> 8) & 0x07) as u8,
        ))
    }

    fn selected_config(&self) -> Option<&[u8; 256]> {
        match self.selected_bdf() {
            Some((ROOT_BUS, ROOT_DEVICE, ROOT_FUNCTION)) => Some(&self.root_bridge_config),
            _ => None,
        }
    }

    fn selected_config_mut(&mut self) -> Option<&mut [u8; 256]> {
        match self.selected_bdf() {
            Some((ROOT_BUS, ROOT_DEVICE, ROOT_FUNCTION)) => Some(&mut self.root_bridge_config),
            _ => None,
        }
    }

    fn config_data_index(&self, offset: usize) -> Option<usize> {
        let port_offset = offset.checked_sub(CONFIG_DATA_PORT_OFFSET)?;
        Some(((self.config_address & 0xfc) as usize) + port_offset)
    }

    fn read_byte(&self, offset: usize) -> u8 {
        if offset >= CONFIG_IO_WINDOW_SIZE {
            return 0xff;
        }

        if offset < CONFIG_DATA_PORT_OFFSET {
            return self.config_address.to_le_bytes()[offset];
        }

        let Some(index) = self.config_data_index(offset) else {
            return 0xff;
        };

        self.selected_config()
            .and_then(|cfg| cfg.get(index).copied())
            .unwrap_or(0xff)
    }

    fn write_byte(&mut self, offset: usize, value: u8) {
        if offset >= CONFIG_IO_WINDOW_SIZE {
            return;
        }

        if offset < CONFIG_DATA_PORT_OFFSET {
            let mut bytes = self.config_address.to_le_bytes();
            bytes[offset] = value;
            self.config_address = u32::from_le_bytes(bytes);
            return;
        }

        let Some(index) = self.config_data_index(offset) else {
            return;
        };

        if let Some(cfg) = self.selected_config_mut() {
            if index < cfg.len() && Self::root_bridge_byte_is_writable(index) {
                cfg[index] = value;
            }
        }
    }

    fn root_bridge_byte_is_writable(index: usize) -> bool {
        !matches!(index, 0x00..=0x03 | 0x08..=0x0b | 0x0e)
    }
}

impl BusDevice for PciConfigIoStub {
    fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
        static READ_COUNT: AtomicU64 = AtomicU64::new(0);
        let base = offset as usize;
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = self.read_byte(base + i);
        }
        let count = READ_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if count <= 64 || count % 256 == 0 {
            Self::debug_log(format!(
                "[PCICFG-R] count={} off=0x{:x} len={} addr=0x{:08x} bdf={:?} data={:02x?}",
                count,
                offset,
                data.len(),
                self.config_address,
                self.selected_bdf(),
                data
            ));
        }
    }

    fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
        static WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
        let base = offset as usize;
        for (i, byte) in data.iter().enumerate() {
            self.write_byte(base + i, *byte);
        }
        let count = WRITE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if count <= 64 || count % 256 == 0 {
            Self::debug_log(format!(
                "[PCICFG-W] count={} off=0x{:x} len={} addr=0x{:08x} bdf={:?} data={:02x?}",
                count,
                offset,
                data.len(),
                self.config_address,
                self.selected_bdf(),
                data
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CONFIG_ENABLE_BIT, PCI_CLASS_HOST_BRIDGE, PCI_SUBCLASS_HOST_BRIDGE, PciConfigIoStub,
    };
    use crate::BusDevice;

    #[test]
    fn config_address_round_trips() {
        let mut stub = PciConfigIoStub::new();
        stub.write(0, 0, &0x8000_0000_u32.to_le_bytes());

        let mut data = [0u8; 4];
        stub.read(0, 0, &mut data);
        assert_eq!(u32::from_le_bytes(data), 0x8000_0000);
    }

    #[test]
    fn disabled_config_space_reads_as_all_ones() {
        let mut stub = PciConfigIoStub::new();
        let mut data = [0u8; 4];

        stub.read(0, 4, &mut data);

        assert_eq!(data, [0xff; 4]);
    }

    #[test]
    fn root_bridge_reports_host_bridge_class() {
        let mut stub = PciConfigIoStub::new();
        stub.write(0, 0, &(CONFIG_ENABLE_BIT | 0x08).to_le_bytes());

        let mut data = [0u8; 4];
        stub.read(0, 4, &mut data);

        assert_eq!(data[1], 0x00);
        assert_eq!(data[2], PCI_SUBCLASS_HOST_BRIDGE);
        assert_eq!(data[3], PCI_CLASS_HOST_BRIDGE);
    }

    #[test]
    fn unknown_devices_read_as_all_ones() {
        let mut stub = PciConfigIoStub::new();
        let absent_device_addr = CONFIG_ENABLE_BIT | (1 << 11);
        stub.write(0, 0, &absent_device_addr.to_le_bytes());

        let mut data = [0u8; 2];
        stub.read(0, 4, &mut data);

        assert_eq!(data, [0xff; 2]);
    }

    #[test]
    fn root_bridge_command_register_is_writable() {
        let mut stub = PciConfigIoStub::new();
        stub.write(0, 0, &(CONFIG_ENABLE_BIT | 0x04).to_le_bytes());
        stub.write(0, 4, &[0x07, 0x00]);

        let mut data = [0u8; 2];
        stub.read(0, 4, &mut data);

        assert_eq!(data, [0x07, 0x00]);
    }
}
