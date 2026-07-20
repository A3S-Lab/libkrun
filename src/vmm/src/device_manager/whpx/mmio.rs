// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::{fmt, io};

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use devices::fdt::DeviceInfoForFDT;
#[cfg(target_arch = "aarch64")]
use devices::legacy::IrqChip;
use devices::{BusDevice, DeviceType};
use kernel::cmdline as kernel_cmdline;
use utils::eventfd::EventFd;

#[cfg(target_arch = "aarch64")]
use crate::vstate::Vm;

/// Errors for MMIO device manager.
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub enum Error {
    /// Failed to create MmioTransport
    CreateMmioTransport(devices::virtio::CreateMmioTransportError),
    /// Failed to perform an operation on the bus.
    BusError(devices::BusError),
    /// Appending to kernel command line failed.
    Cmdline(kernel_cmdline::Error),
    /// Failure in creating or cloning an event fd.
    EventFd(io::Error),
    /// No more IRQs are available.
    IrqsExhausted,
    /// Registering an IO Event failed.
    RegisterIoEvent,
    /// Registering an IRQ FD failed.
    RegisterIrqFd,
    /// The device couldn't be found
    DeviceNotFound,
    /// Failed to update the mmio device.
    UpdateFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::CreateMmioTransport(ref e) => {
                write!(f, "failed to create mmio transport for the device {e}")
            }
            Error::BusError(ref e) => write!(f, "failed to perform bus operation: {e}"),
            Error::Cmdline(ref e) => {
                write!(f, "unable to add device to kernel command line: {e}")
            }
            Error::EventFd(ref e) => write!(f, "failed to create or clone event descriptor: {e}"),
            Error::IrqsExhausted => write!(f, "no more IRQs are available"),
            Error::RegisterIoEvent => write!(f, "failed to register IO event"),
            Error::RegisterIrqFd => write!(f, "failed to register irqfd"),
            Error::DeviceNotFound => write!(f, "the device couldn't be found"),
            Error::UpdateFailed => write!(f, "failed to update the mmio device"),
        }
    }
}

impl From<devices::virtio::CreateMmioTransportError> for crate::device_manager::mmio::Error {
    fn from(e: devices::virtio::CreateMmioTransportError) -> Self {
        Self::CreateMmioTransport(e)
    }
}

type Result<T> = ::std::result::Result<T, Error>;

/// This represents the size of the mmio device specified to the kernel as a cmdline option
/// It has to be larger than 0x100 (the offset where the configuration space starts from
/// the beginning of the memory mapped device registers) + the size of the configuration space
/// Currently hardcoded to 4K.
const MMIO_LEN: u64 = 0x1000;

/// Manages the complexities of registering a MMIO device.
pub struct MMIODeviceManager {
    pub bus: devices::Bus,
    mmio_base: u64,
    irq: u32,
    last_irq: u32,
    id_to_dev_info: HashMap<(DeviceType, String), MMIODeviceInfo>,
}

impl MMIODeviceManager {
    /// Create a new DeviceManager handling mmio devices (virtio net, block).
    pub fn new(mmio_base: &mut u64, irq_interval: (u32, u32)) -> MMIODeviceManager {
        if cfg!(target_arch = "aarch64") {
            *mmio_base += MMIO_LEN;
        }

        MMIODeviceManager {
            mmio_base: *mmio_base,
            irq: irq_interval.0,
            last_irq: irq_interval.1,
            bus: devices::Bus::new(),
            id_to_dev_info: HashMap::new(),
        }
    }

    /// Register an already created MMIO device to be used via MMIO transport.
    pub fn register_mmio_device(
        &mut self,
        mut mmio_device: devices::virtio::MmioTransport,
        type_id: u32,
        device_id: String,
    ) -> Result<(u64, u32)> {
        if self.irq > self.last_irq {
            return Err(Error::IrqsExhausted);
        }

        let mut queue_evts: Vec<EventFd> = Vec::new();

        for queue_evt in mmio_device.locked_device().queue_events().iter() {
            queue_evts.push(queue_evt.try_clone().unwrap());
        }

        for (i, queue_evt) in queue_evts.drain(0..).enumerate() {
            mmio_device.register_queue_evt(queue_evt, i as u32);
        }

        let assigned_irq = if cfg!(target_os = "windows")
            && type_id == devices::virtio::TYPE_VSOCK
            && !matches!(
                std::env::var("LIBKRUN_WHPX_SHARE_VSOCK_IRQ_WITH_PREV").as_deref(),
                Ok("0")
            ) {
            // WHPX virtio-vsock still needs the old shared-IRQ behavior in the
            // current Windows bring-up path. Keep it as the default, but allow
            // opt-out experiments with LIBKRUN_WHPX_SHARE_VSOCK_IRQ_WITH_PREV=0.
            self.irq.saturating_sub(1)
        } else {
            self.irq
        };

        mmio_device.set_irq_line(assigned_irq);

        self.bus
            .insert(Arc::new(Mutex::new(mmio_device)), self.mmio_base, MMIO_LEN)
            .map_err(Error::BusError)?;
        let ret = (self.mmio_base, assigned_irq);
        self.id_to_dev_info.insert(
            (DeviceType::Virtio(type_id), device_id),
            MMIODeviceInfo {
                addr: self.mmio_base,
                len: MMIO_LEN,
                irq: assigned_irq,
            },
        );
        self.mmio_base += MMIO_LEN;
        self.irq += 1;

        Ok(ret)
    }

    /// Append a `virtio_mmio.device=<size>K@<base>:<irq>` entry to the kernel
    /// command line so that a Linux guest can discover the device.
    pub fn add_device_to_cmdline(
        &mut self,
        cmdline: &mut kernel_cmdline::Cmdline,
        mmio_base: u64,
        irq: u32,
    ) -> Result<()> {
        cmdline
            .insert(
                "virtio_mmio.device",
                &format!("{}K@0x{:08x}:{}", MMIO_LEN / 1024, mmio_base, irq),
            )
            .map_err(Error::Cmdline)
    }

    #[cfg(target_arch = "aarch64")]
    /// Register an early console at some MMIO address.
    pub fn register_mmio_serial(
        &mut self,
        _vm: &Vm,
        cmdline: &mut kernel_cmdline::Cmdline,
        intc: IrqChip,
        serial: Arc<Mutex<devices::legacy::Serial>>,
    ) -> Result<()> {
        if self.irq > self.last_irq {
            return Err(Error::IrqsExhausted);
        }

        {
            let mut serial = serial.lock().unwrap();
            serial.set_intc(intc);
            serial.set_irq_line(self.irq);
        }

        self.bus
            .insert(serial, self.mmio_base, MMIO_LEN)
            .map_err(Error::BusError)?;

        cmdline
            .insert(
                "earlycon",
                &format!("pl011,mmio32,0x{:08x}", self.mmio_base),
            )
            .map_err(Error::Cmdline)?;

        let ret = self.mmio_base;
        self.id_to_dev_info.insert(
            (DeviceType::Serial, DeviceType::Serial.to_string()),
            MMIODeviceInfo {
                addr: ret,
                len: MMIO_LEN,
                irq: self.irq,
            },
        );

        self.mmio_base += MMIO_LEN;
        self.irq += 1;

        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    /// Register a MMIO RTC device.
    pub fn register_mmio_rtc(&mut self, _vm: &Vm, _intc: IrqChip) -> Result<()> {
        if self.irq > self.last_irq {
            return Err(Error::IrqsExhausted);
        }

        // Attaching the RTC device.
        let rtc_evt = EventFd::new(utils::eventfd::EFD_NONBLOCK).map_err(Error::EventFd)?;
        let device = devices::legacy::RTC::new(rtc_evt.try_clone().map_err(Error::EventFd)?);

        self.bus
            .insert(Arc::new(Mutex::new(device)), self.mmio_base, MMIO_LEN)
            .map_err(Error::BusError)?;

        let ret = self.mmio_base;
        self.id_to_dev_info.insert(
            (DeviceType::RTC, "rtc".to_string()),
            MMIODeviceInfo {
                addr: ret,
                len: MMIO_LEN,
                irq: self.irq,
            },
        );

        self.mmio_base += MMIO_LEN;
        self.irq += 1;

        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    /// Register a GPIO
    pub fn register_mmio_gpio(
        &mut self,
        _vm: &Vm,
        intc: IrqChip,
        event_manager: &mut EventManager,
        shutdown_efd: EventFd,
    ) -> Result<()> {
        // Attaching the GPIO device.
        let gpio_evt = EventFd::new(utils::eventfd::EFD_NONBLOCK).map_err(Error::EventFd)?;
        let gpio = Arc::new(Mutex::new(devices::legacy::Gpio::new(
            shutdown_efd,
            gpio_evt.try_clone().map_err(Error::EventFd)?,
        )));

        event_manager.add_subscriber(gpio.clone()).unwrap();

        if self.irq > self.last_irq {
            return Err(Error::IrqsExhausted);
        }

        {
            let mut gpio = gpio.lock().unwrap();
            gpio.set_intc(intc);
            gpio.set_irq_line(self.irq);
        }

        self.bus
            .insert(gpio, self.mmio_base, MMIO_LEN)
            .map_err(Error::BusError)?;

        let ret = self.mmio_base;
        self.id_to_dev_info.insert(
            (DeviceType::Gpio, DeviceType::Gpio.to_string()),
            MMIODeviceInfo {
                addr: ret,
                len: MMIO_LEN,
                irq: self.irq,
            },
        );

        self.mmio_base += MMIO_LEN;
        self.irq += 1;

        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    /// Register a MMIO GIC device.
    pub fn register_mmio_gic(&mut self, _vm: &Vm, intc: IrqChip) -> Result<()> {
        let (mmio_addr, mmio_size) = {
            let intc = intc.lock().unwrap();
            (intc.get_mmio_addr(), intc.get_mmio_size())
        };

        // The in-kernel GIC reports a size of 0 to tell us we don't need to map
        // anything in the guest.
        if mmio_size != 0 {
            self.bus
                .insert(intc, mmio_addr, mmio_size)
                .map_err(Error::BusError)?;
        }

        Ok(())
    }

    /// Gets the information of the devices registered up to some point in time.
    pub fn get_device_info(&self) -> &HashMap<(DeviceType, String), MMIODeviceInfo> {
        &self.id_to_dev_info
    }

    /// Gets the specified device.
    pub fn get_device(
        &self,
        device_type: DeviceType,
        device_id: &str,
    ) -> Option<&Mutex<dyn BusDevice>> {
        if let Some(dev_info) = self
            .id_to_dev_info
            .get(&(device_type, device_id.to_string()))
        {
            if let Some((_, device)) = self.bus.get_device(dev_info.addr) {
                return Some(device);
            }
        }
        None
    }
}

/// Private structure for storing information about the MMIO device registered at some address on the bus.
#[derive(Clone, Debug)]
pub struct MMIODeviceInfo {
    addr: u64,
    #[cfg_attr(
        not(any(target_arch = "aarch64", target_arch = "riscv64")),
        allow(dead_code)
    )]
    irq: u32,
    #[cfg_attr(
        not(any(target_arch = "aarch64", target_arch = "riscv64")),
        allow(dead_code)
    )]
    len: u64,
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
impl DeviceInfoForFDT for MMIODeviceInfo {
    fn addr(&self) -> u64 {
        self.addr
    }
    fn irq(&self) -> u32 {
        self.irq
    }
    fn length(&self) -> u64 {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::cmdline as kernel_cmdline;

    #[test]
    fn test_error_messages() {
        let device_manager =
            MMIODeviceManager::new(&mut 0xd000_0000, (arch::IRQ_BASE, arch::IRQ_MAX));
        let mut cmdline = kernel_cmdline::Cmdline::new(4096);
        let e = Error::Cmdline(
            cmdline
                .insert(
                    "virtio_mmio=device",
                    &format!(
                        "{}K@0x{:08x}:{}",
                        MMIO_LEN / 1024,
                        device_manager.mmio_base,
                        device_manager.irq
                    ),
                )
                .unwrap_err(),
        );
        assert_eq!(
            format!("{}", e),
            format!(
                "unable to add device to kernel command line: {}",
                kernel_cmdline::Error::HasEquals
            ),
        );
        assert_eq!(
            format!("{}", Error::UpdateFailed),
            "failed to update the mmio device"
        );
        assert_eq!(
            format!("{}", Error::IrqsExhausted),
            "no more IRQs are available"
        );
    }
}
