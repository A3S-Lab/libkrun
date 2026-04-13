#[derive(Clone, Debug)]
pub struct FsDeviceConfig {
    pub fs_id: String,
    pub shared_dir: String,
    pub shm_size: Option<usize>,
    #[cfg(target_os = "macos")]
    pub no_fsync: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_device_config_clone() {
        let config = FsDeviceConfig {
            fs_id: "myfs".to_string(),
            shared_dir: "/shared".to_string(),
            shm_size: Some(256 * 1024 * 1024),
            no_fsync: false,
        };
        let cloned = config.clone();
        assert_eq!(config.fs_id, cloned.fs_id);
        assert_eq!(config.shared_dir, cloned.shared_dir);
        assert_eq!(config.shm_size, cloned.shm_size);
        assert_eq!(config.no_fsync, cloned.no_fsync);
    }

    #[test]
    fn test_fs_device_config_eq() {
        let config1 = FsDeviceConfig {
            fs_id: "myfs".to_string(),
            shared_dir: "/shared".to_string(),
            shm_size: Some(256 * 1024 * 1024),
            no_fsync: false,
        };
        let config2 = FsDeviceConfig {
            fs_id: "myfs".to_string(),
            shared_dir: "/shared".to_string(),
            shm_size: Some(256 * 1024 * 1024),
            no_fsync: false,
        };
        assert_eq!(config1, config2);
    }

    #[test]
    fn test_fs_device_config_neq() {
        let config1 = FsDeviceConfig {
            fs_id: "fs1".to_string(),
            shared_dir: "/shared".to_string(),
            shm_size: None,
            no_fsync: true,
        };
        let config2 = FsDeviceConfig {
            fs_id: "fs2".to_string(),
            shared_dir: "/shared".to_string(),
            shm_size: None,
            no_fsync: true,
        };
        assert_ne!(config1, config2);
    }

    #[test]
    fn test_fs_device_config_debug() {
        let config = FsDeviceConfig {
            fs_id: "myfs".to_string(),
            shared_dir: "/tmp/share".to_string(),
            shm_size: Some(1024),
            no_fsync: true,
        };
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("myfs"));
        assert!(debug_str.contains("/tmp/share"));
        assert!(debug_str.contains("no_fsync"));
    }

    #[test]
    fn test_fs_device_config_no_shm_size() {
        let config = FsDeviceConfig {
            fs_id: "fs0".to_string(),
            shared_dir: "/tmp".to_string(),
            shm_size: None,
            no_fsync: false,
        };
        assert!(config.shm_size.is_none());
    }
}
