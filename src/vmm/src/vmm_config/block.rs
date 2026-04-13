use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};

use devices::virtio::{
    block::{ImageType, SyncMode},
    Block, CacheType,
};

#[derive(Debug)]
pub enum BlockConfigError {
    /// Failed to create the block device.
    CreateBlockDevice(std::io::Error),
}

impl fmt::Display for BlockConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::BlockConfigError::*;
        match *self {
            CreateBlockDevice(ref e) => write!(f, "Cannot create block device: {e:?}"),
        }
    }
}

type Result<T> = std::result::Result<T, BlockConfigError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDeviceConfig {
    pub block_id: String,
    pub cache_type: CacheType,
    pub disk_image_path: String,
    pub disk_image_format: ImageType,
    pub is_disk_read_only: bool,
    pub direct_io: bool,
    pub sync_mode: SyncMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockRootConfig {
    pub device: String,
    pub fstype: Option<String>,
    pub options: Option<String>,
}

#[derive(Default)]
pub struct BlockBuilder {
    pub list: VecDeque<Arc<Mutex<Block>>>,
}

impl BlockBuilder {
    pub fn new() -> Self {
        Self {
            list: VecDeque::<Arc<Mutex<Block>>>::new(),
        }
    }

    pub fn insert(&mut self, config: BlockDeviceConfig) -> Result<()> {
        let block_dev = Arc::new(Mutex::new(Self::create_block(config)?));
        self.list.push_back(block_dev);
        Ok(())
    }

    pub fn create_block(config: BlockDeviceConfig) -> Result<Block> {
        devices::virtio::Block::new(
            config.block_id,
            None,
            config.cache_type,
            config.disk_image_path,
            config.disk_image_format,
            config.is_disk_read_only,
            config.direct_io,
            config.sync_mode,
        )
        .map_err(BlockConfigError::CreateBlockDevice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devices::virtio::block::{ImageType, SyncMode};
    use devices::virtio::CacheType;

    #[test]
    fn test_block_config_error_debug() {
        let io_error = std::io::Error::new(std::io::ErrorKind::Other, "test error");
        let error = BlockConfigError::CreateBlockDevice(io_error);
        let debug_str = format!("{:?}", error);
        assert!(debug_str.contains("CreateBlockDevice"));
    }

    #[test]
    fn test_block_config_error_display() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let error = BlockConfigError::CreateBlockDevice(io_error);
        let display_str = format!("{}", error);
        assert!(display_str.contains("Cannot create block device"));
    }

    #[test]
    fn test_block_device_config_clone() {
        let config = BlockDeviceConfig {
            block_id: "drive0".to_string(),
            cache_type: CacheType::Unsafe,
            disk_image_path: "/path/to/image".to_string(),
            disk_image_format: ImageType::Raw,
            is_disk_read_only: false,
            direct_io: true,
            sync_mode: SyncMode::Fsync,
        };
        let cloned = config.clone();
        assert_eq!(config.block_id, cloned.block_id);
        assert_eq!(config.cache_type, cloned.cache_type);
        assert_eq!(config.disk_image_path, cloned.disk_image_path);
        assert_eq!(config.disk_image_format, cloned.disk_image_format);
        assert_eq!(config.is_disk_read_only, cloned.is_disk_read_only);
        assert_eq!(config.direct_io, cloned.direct_io);
        assert_eq!(config.sync_mode, cloned.sync_mode);
    }

    #[test]
    fn test_block_device_config_eq() {
        let config1 = BlockDeviceConfig {
            block_id: "drive0".to_string(),
            cache_type: CacheType::Unsafe,
            disk_image_path: "/path/to/image".to_string(),
            disk_image_format: ImageType::Raw,
            is_disk_read_only: false,
            direct_io: true,
            sync_mode: SyncMode::Fsync,
        };
        let config2 = BlockDeviceConfig {
            block_id: "drive0".to_string(),
            cache_type: CacheType::Unsafe,
            disk_image_path: "/path/to/image".to_string(),
            disk_image_format: ImageType::Raw,
            is_disk_read_only: false,
            direct_io: true,
            sync_mode: SyncMode::Fsync,
        };
        assert_eq!(config1, config2);
    }

    #[test]
    fn test_block_device_config_neq() {
        let config1 = BlockDeviceConfig {
            block_id: "drive0".to_string(),
            cache_type: CacheType::Unsafe,
            disk_image_path: "/path/to/image".to_string(),
            disk_image_format: ImageType::Raw,
            is_disk_read_only: false,
            direct_io: true,
            sync_mode: SyncMode::Fsync,
        };
        let config2 = BlockDeviceConfig {
            block_id: "drive1".to_string(),
            cache_type: CacheType::Unsafe,
            disk_image_path: "/path/to/image".to_string(),
            disk_image_format: ImageType::Raw,
            is_disk_read_only: false,
            direct_io: true,
            sync_mode: SyncMode::Fsync,
        };
        assert_ne!(config1, config2);
    }

    #[test]
    fn test_block_root_config_default() {
        let config = BlockRootConfig::default();
        assert_eq!(config.device, "");
        assert!(config.fstype.is_none());
        assert!(config.options.is_none());
    }

    #[test]
    fn test_block_root_config_with_values() {
        let config = BlockRootConfig {
            device: "/dev/sda1".to_string(),
            fstype: Some("ext4".to_string()),
            options: Some("rw".to_string()),
        };
        assert_eq!(config.device, "/dev/sda1");
        assert_eq!(config.fstype, Some("ext4".to_string()));
        assert_eq!(config.options, Some("rw".to_string()));
    }

    #[test]
    fn test_block_builder_new() {
        let builder = BlockBuilder::new();
        assert!(builder.list.is_empty());
    }

    #[test]
    fn test_block_builder_default() {
        let builder = BlockBuilder::default();
        assert!(builder.list.is_empty());
    }
}
