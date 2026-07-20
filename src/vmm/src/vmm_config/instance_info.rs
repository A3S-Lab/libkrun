// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

/// The strongly typed that contains general information about the microVM.
#[derive(Clone, Debug)]
pub struct InstanceInfo {
    /// The ID of the microVM.
    pub id: String,
    /// Whether the microVM has been started.
    pub started: bool,
    /// The version of the VMM that runs the microVM.
    pub vmm_version: String,
    /// The name of the application that runs the microVM.
    pub app_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_info_debug() {
        let info = InstanceInfo {
            id: "test-vm".to_string(),
            started: false,
            vmm_version: "1.0.0".to_string(),
            app_name: "test-app".to_string(),
        };
        let debug_str = format!("{:?}", info);
        assert!(debug_str.contains("test-vm"));
        assert!(debug_str.contains("1.0.0"));
        assert!(debug_str.contains("test-app"));
    }

    #[test]
    fn test_instance_info_clone() {
        let info = InstanceInfo {
            id: "test-vm".to_string(),
            started: true,
            vmm_version: "1.0.0".to_string(),
            app_name: "test-app".to_string(),
        };
        let cloned = info.clone();
        assert_eq!(info.id, cloned.id);
        assert_eq!(info.started, cloned.started);
        assert_eq!(info.vmm_version, cloned.vmm_version);
        assert_eq!(info.app_name, cloned.app_name);
    }

    #[test]
    fn test_instance_info_default_started() {
        let info = InstanceInfo {
            id: "vm1".to_string(),
            started: true,
            vmm_version: "2.0.0".to_string(),
            app_name: "app1".to_string(),
        };
        assert!(info.started);
    }

    #[test]
    fn test_instance_info_empty_strings() {
        let info = InstanceInfo {
            id: String::new(),
            started: false,
            vmm_version: String::new(),
            app_name: String::new(),
        };
        assert_eq!(info.id, "");
        assert_eq!(info.vmm_version, "");
        assert_eq!(info.app_name, "");
        assert!(!info.started);
    }
}
