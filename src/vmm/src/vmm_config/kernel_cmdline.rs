// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::{Display, Formatter, Result};

#[cfg(target_os = "linux")]
pub const DEFAULT_KERNEL_CMDLINE: &str = "reboot=k panic=-1 panic_print=0 nomodule console=hvc0 \
                                          rootfstype=virtiofs rw quiet no-kvmapf";
#[cfg(target_os = "macos")]
pub const DEFAULT_KERNEL_CMDLINE: &str = "reboot=k panic=-1 panic_print=0 nomodule console=hvc0 \
                                           rootfstype=virtiofs rw quiet no-kvmapf";
#[cfg(target_os = "windows")]
pub const DEFAULT_KERNEL_CMDLINE: &str =
    "reboot=k panic=-1 panic_print=0 nomodule console=ttyS0,115200 earlyprintk=serial,ttyS0,115200 \
     rootfstype=virtiofs rw no-kvmapf lpj=1000000 tsc=reliable \
     i8042.noaux i8042.nomux i8042.nopnp";

/// Strongly typed data structure used to configure the boot source of the
/// microvm.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KernelCmdlineConfig {
    pub prolog: Option<String>,
    pub krun_env: Option<String>,
    pub epilog: Option<String>,
}

/// Errors associated with actions on `KernelCmdlineConfig`.
#[derive(Debug)]
pub enum KernelCmdlineConfigError {
    /// The kernel command line is invalid.
    InvalidKernelCommandLine(String),
}

impl Display for KernelCmdlineConfigError {
    fn fmt(&self, f: &mut Formatter) -> Result {
        use self::KernelCmdlineConfigError::*;
        match *self {
            InvalidKernelCommandLine(ref e) => {
                write!(f, "The kernel command line is invalid: {}", e.as_str())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kernel_cmdline_config_default() {
        let config = KernelCmdlineConfig::default();
        assert!(config.prolog.is_none());
        assert!(config.krun_env.is_none());
        assert!(config.epilog.is_none());
    }

    #[test]
    fn test_kernel_cmdline_config_clone() {
        let config = KernelCmdlineConfig {
            prolog: Some("earlyprintk=ttyS0".to_string()),
            krun_env: Some("HOME=/".to_string()),
            epilog: Some("quiet".to_string()),
        };
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn test_kernel_cmdline_config_eq() {
        let config1 = KernelCmdlineConfig {
            prolog: Some("console=ttyS0".to_string()),
            krun_env: None,
            epilog: Some("quiet".to_string()),
        };
        let config2 = KernelCmdlineConfig {
            prolog: Some("console=ttyS0".to_string()),
            krun_env: None,
            epilog: Some("quiet".to_string()),
        };
        assert_eq!(config1, config2);
    }

    #[test]
    fn test_kernel_cmdline_config_neq() {
        let config1 = KernelCmdlineConfig {
            prolog: Some("console=ttyS0".to_string()),
            krun_env: None,
            epilog: None,
        };
        let config2 = KernelCmdlineConfig {
            prolog: Some("console=ttyS1".to_string()),
            krun_env: None,
            epilog: None,
        };
        assert_ne!(config1, config2);
    }

    #[test]
    fn test_kernel_cmdline_config_error_debug() {
        let error = KernelCmdlineConfigError::InvalidKernelCommandLine("bad cmdline".to_string());
        let debug_str = format!("{:?}", error);
        assert!(debug_str.contains("InvalidKernelCommandLine"));
        assert!(debug_str.contains("bad cmdline"));
    }

    #[test]
    fn test_kernel_cmdline_config_error_display() {
        let error = KernelCmdlineConfigError::InvalidKernelCommandLine("empty command".to_string());
        let display_str = format!("{}", error);
        assert!(display_str.contains("The kernel command line is invalid"));
        assert!(display_str.contains("empty command"));
    }

    #[test]
    fn test_default_kernel_cmdline_not_empty() {
        // Verify that the default kernel cmdline is not empty for all platforms
        assert!(!DEFAULT_KERNEL_CMDLINE.is_empty());
        assert!(DEFAULT_KERNEL_CMDLINE.contains("reboot=k"));
        assert!(DEFAULT_KERNEL_CMDLINE.contains("panic="));
    }

    #[test]
    fn test_kernel_cmdline_config_with_all_fields() {
        let config = KernelCmdlineConfig {
            prolog: Some("earlyprintk".to_string()),
            krun_env: Some("DEBUG=1".to_string()),
            epilog: Some("quiet splash".to_string()),
        };
        assert!(config.prolog.is_some());
        assert!(config.krun_env.is_some());
        assert!(config.epilog.is_some());
    }
}
