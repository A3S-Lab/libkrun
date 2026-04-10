#[derive(Clone, Debug)]
pub struct FsDeviceConfig {
    pub fs_id: String,
    pub shared_dir: String,
    pub shm_size: Option<usize>,
    #[cfg(target_os = "macos")]
    pub no_fsync: bool,
}
