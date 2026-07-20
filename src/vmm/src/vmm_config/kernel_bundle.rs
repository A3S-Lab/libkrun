// Copyright 2020, Red Hat Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter, Result};

/// Data structure holding the attributes read from the `libkrunfw` kernel config.
#[derive(Clone, Debug, Default)]
pub struct KernelBundle {
    pub host_addr: u64,
    pub guest_addr: u64,
    pub entry_addr: u64,
    pub size: usize,
}

/// Structure used to specify the parameters for the `libkrunfw` kernel bundle.
#[derive(Debug)]
pub enum KernelBundleError {
    /// Guest address is not page-aligned.
    InvalidGuestAddress,
    /// Host address is zero or not page-aligned.
    InvalidHostAddress,
    /// Kernel size is zero or not a multiple of the page size.
    InvalidSize,
}

impl Display for KernelBundleError {
    fn fmt(&self, f: &mut Formatter) -> Result {
        use self::KernelBundleError::*;
        match *self {
            InvalidGuestAddress => write!(f, "Guest address is not page-aligned"),
            InvalidHostAddress => write!(f, "Host address is zero or not page-aligned"),
            InvalidSize => write!(f, "Kernel size is zero or not a multiple of the page size"),
        }
    }
}

/// Data structure holding the attributes read from the `libkrunfw` qboot config.
#[derive(Debug, Default)]
pub struct QbootBundle {
    pub host_addr: u64,
    pub size: usize,
}

/// Structure used to specify the parameters for the `libkrunfw` qboot bundle.
#[derive(Debug)]
pub enum QbootBundleError {
    /// Qboot binary is not 64K long.
    InvalidSize,
}

impl Display for QbootBundleError {
    fn fmt(&self, f: &mut Formatter) -> Result {
        use self::QbootBundleError::*;
        match *self {
            InvalidSize => write!(f, "qboot binary is not 64K long."),
        }
    }
}

/// Data structure holding the attributes read from the `libkrunfw` initrd config.
#[derive(Clone, Debug, Default)]
pub struct InitrdBundle {
    pub host_addr: u64,
    pub size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kernel_bundle_default() {
        let bundle = KernelBundle::default();
        assert_eq!(bundle.host_addr, 0);
        assert_eq!(bundle.guest_addr, 0);
        assert_eq!(bundle.entry_addr, 0);
        assert_eq!(bundle.size, 0);
    }

    #[test]
    fn test_kernel_bundle_clone() {
        let bundle = KernelBundle {
            host_addr: 0x1000,
            guest_addr: 0x2000,
            entry_addr: 0x3000,
            size: 4096,
        };
        let cloned = bundle.clone();
        assert_eq!(bundle.host_addr, cloned.host_addr);
        assert_eq!(bundle.guest_addr, cloned.guest_addr);
        assert_eq!(bundle.entry_addr, cloned.entry_addr);
        assert_eq!(bundle.size, cloned.size);
    }

    #[test]
    fn test_kernel_bundle_error_debug() {
        let error = KernelBundleError::InvalidGuestAddress;
        assert_eq!(format!("{:?}", error), "InvalidGuestAddress");

        let error = KernelBundleError::InvalidHostAddress;
        assert_eq!(format!("{:?}", error), "InvalidHostAddress");

        let error = KernelBundleError::InvalidSize;
        assert_eq!(format!("{:?}", error), "InvalidSize");
    }

    #[test]
    fn test_kernel_bundle_error_display() {
        let error = KernelBundleError::InvalidGuestAddress;
        assert!(format!("{}", error).contains("Guest address"));

        let error = KernelBundleError::InvalidHostAddress;
        assert!(format!("{}", error).contains("Host address"));

        let error = KernelBundleError::InvalidSize;
        assert!(format!("{}", error).contains("Kernel size"));
    }

    #[test]
    fn test_qboot_bundle_default() {
        let bundle = QbootBundle::default();
        assert_eq!(bundle.host_addr, 0);
        assert_eq!(bundle.size, 0);
    }

    #[test]
    fn test_qboot_bundle_error_debug() {
        let error = QbootBundleError::InvalidSize;
        assert_eq!(format!("{:?}", error), "InvalidSize");
    }

    #[test]
    fn test_qboot_bundle_error_display() {
        let error = QbootBundleError::InvalidSize;
        assert!(format!("{}", error).contains("64K"));
    }

    #[test]
    fn test_initrd_bundle_default() {
        let bundle = InitrdBundle::default();
        assert_eq!(bundle.host_addr, 0);
        assert_eq!(bundle.size, 0);
    }

    #[test]
    fn test_initrd_bundle_clone() {
        let bundle = InitrdBundle {
            host_addr: 0x5000,
            size: 8192,
        };
        let cloned = bundle.clone();
        assert_eq!(bundle.host_addr, cloned.host_addr);
        assert_eq!(bundle.size, cloned.size);
    }
}
