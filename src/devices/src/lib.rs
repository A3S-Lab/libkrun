// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Portions Copyright 2017 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the THIRD-PARTY file.

//! Emulates virtual and hardware devices.

#[macro_use]
extern crate log;

use std::fmt;
use std::io;

mod bus;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub mod fdt;
pub mod legacy;
pub mod virtio;

pub use self::bus::{Bus, BusDevice, Error as BusError};

#[derive(Debug)]
pub enum Error {
    FailedReadingQueue {
        event_type: &'static str,
        underlying: io::Error,
    },
    FailedReadTap,
    FailedSignalingUsedQueue(io::Error),
    PayloadExpected,
    IoError(io::Error),
    NoAvailBuffers,
    SpuriousEvent,
}

/// Types of devices that can get attached to this platform.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Copy)]
pub enum DeviceType {
    /// Device Type: Virtio.
    Virtio(u32),
    /// Device Type: GPIO (PL061).
    #[cfg(target_arch = "aarch64")]
    Gpio,
    /// Device Type: Serial.
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    Serial,
    /// Device Type: RTC.
    #[cfg(target_arch = "aarch64")]
    RTC,
}

impl fmt::Display for DeviceType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_type_virtio() {
        let device = DeviceType::Virtio(1);
        assert_eq!(format!("{:?}", device), "Virtio(1)");
        assert_eq!(format!("{}", device), "Virtio(1)");
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_device_type_gpio() {
        let device = DeviceType::Gpio;
        assert_eq!(format!("{:?}", device), "Gpio");
    }

    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    #[test]
    fn test_device_type_serial() {
        let device = DeviceType::Serial;
        assert_eq!(format!("{:?}", device), "Serial");
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_device_type_rtc() {
        let device = DeviceType::RTC;
        assert_eq!(format!("{:?}", device), "RTC");
    }

    #[test]
    fn test_device_type_clone() {
        let device = DeviceType::Virtio(42);
        let cloned = device.clone();
        assert_eq!(device, cloned);
    }

    #[test]
    fn test_device_type_eq() {
        let device1 = DeviceType::Virtio(1);
        let device2 = DeviceType::Virtio(1);
        let device3 = DeviceType::Virtio(2);
        assert_eq!(device1, device2);
        assert_ne!(device1, device3);
    }

    #[test]
    fn test_error_debug() {
        let io_error = std::io::Error::new(std::io::ErrorKind::Other, "test");
        let error = Error::FailedReadingQueue {
            event_type: "RX",
            underlying: io_error,
        };
        assert_eq!(format!("{:?}", error), "FailedReadingQueue { event_type: \"RX\", underlying: Custom { kind: Other, error: \"\" } }");
    }

    #[test]
    fn test_error_display() {
        let io_error = std::io::Error::new(std::io::ErrorKind::Other, "test error");
        let error = Error::IoError(io_error);
        assert!(format!("{}", error).contains("test error"));
    }

    #[test]
    fn test_error_failed_read_tap() {
        let error = Error::FailedReadTap;
        assert_eq!(format!("{:?}", error), "FailedReadTap");
    }

    #[test]
    fn test_error_no_avail_buffers() {
        let error = Error::NoAvailBuffers;
        assert_eq!(format!("{:?}", error), "NoAvailBuffers");
    }

    #[test]
    fn test_error_spurious_event() {
        let error = Error::SpuriousEvent;
        assert_eq!(format!("{:?}", error), "SpuriousEvent");
    }

    #[test]
    fn test_error_payload_expected() {
        let error = Error::PayloadExpected;
        assert_eq!(format!("{:?}", error), "PayloadExpected");
    }

    #[test]
    fn test_error_failed_signaling_used_queue() {
        let io_error = std::io::Error::new(std::io::ErrorKind::Other, "signal error");
        let error = Error::FailedSignalingUsedQueue(io_error);
        let display = format!("{}", error);
        assert!(display.contains("FailedSignalingUsedQueue"));
    }
}
