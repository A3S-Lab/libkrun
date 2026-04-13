// Copyright 2025, Red Hat Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct FirmwareConfig {
    pub path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_firmware_config_default() {
        let config = FirmwareConfig::default();
        assert!(config.path.as_os_str().is_empty());
    }

    #[test]
    fn test_firmware_config_clone() {
        let config = FirmwareConfig {
            path: PathBuf::from("/usr/share/krun/firmware.bin"),
        };
        let cloned = config.clone();
        assert_eq!(config.path, cloned.path);
    }

    #[test]
    fn test_firmware_config_with_path() {
        let config = FirmwareConfig {
            path: PathBuf::from("/firmware/ovmf.bin"),
        };
        assert_eq!(config.path.to_str(), Some("/firmware/ovmf.bin"));
    }

    #[test]
    fn test_firmware_config_debug() {
        let config = FirmwareConfig {
            path: PathBuf::from("/test/firmware"),
        };
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("FirmwareConfig"));
        assert!(debug_str.contains("/test/firmware"));
    }
}
