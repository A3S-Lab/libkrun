// Windows passthrough filesystem implementation (stub)
// TODO: Implement full Windows filesystem passthrough

use std::ffi::CStr;
use std::io;
use std::time::Duration;

use super::super::filesystem::{
    Context, DirEntry, Entry, ExportTable, Extensions, FileSystem, FsOptions, GetxattrReply,
    ListxattrReply, OpenOptions, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::super::bindings;

/// Configuration for Windows passthrough filesystem
#[derive(Debug, Clone)]
pub struct Config {
    pub entry_timeout: Duration,
    pub attr_timeout: Duration,
    pub root_dir: String,
    pub export_fsid: u64,
    pub export_table: Option<ExportTable>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            entry_timeout: Duration::from_secs(5),
            attr_timeout: Duration::from_secs(5),
            root_dir: String::new(),
            export_fsid: 0,
            export_table: None,
        }
    }
}

/// Windows passthrough filesystem (stub implementation)
pub struct PassthroughFs {
    _cfg: Config,
}

impl PassthroughFs {
    pub fn new(cfg: Config) -> io::Result<PassthroughFs> {
        log::warn!("Windows virtiofs passthrough is not yet implemented");
        Ok(PassthroughFs { _cfg: cfg })
    }
}

impl FileSystem for PassthroughFs {
    type Inode = u64;
    type Handle = u64;

    fn init(&self, _capable: FsOptions) -> io::Result<FsOptions> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn destroy(&self) {}

    fn statfs(&self, _ctx: Context, _inode: Self::Inode) -> io::Result<bindings::statvfs64> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn lookup(&self, _ctx: Context, _parent: Self::Inode, _name: &CStr) -> io::Result<Entry> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn forget(&self, _ctx: Context, _inode: Self::Inode, _count: u64) {}

    fn batch_forget(&self, _ctx: Context, _requests: Vec<(Self::Inode, u64)>) {}

    fn opendir(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn releasedir(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn mkdir(
        &self,
        _ctx: Context,
        _parent: Self::Inode,
        _name: &CStr,
        _mode: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn rmdir(&self, _ctx: Context, _parent: Self::Inode, _name: &CStr) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn readdir<F>(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _size: u32,
        _offset: u64,
        _add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn open(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn release(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
        _handle: Self::Handle,
        _flush: bool,
        _flock_release: bool,
        _lock_owner: Option<u64>,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn create(
        &self,
        _ctx: Context,
        _parent: Self::Inode,
        _name: &CStr,
        _mode: u32,
        _flags: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<(Entry, Option<Self::Handle>, OpenOptions)> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn unlink(&self, _ctx: Context, _parent: Self::Inode, _name: &CStr) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _w: W,
        _size: u32,
        _offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _r: R,
        _size: u32,
        _offset: u64,
        _lock_owner: Option<u64>,
        _delayed_write: bool,
        _kill_priv: bool,
        _flags: u32,
    ) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn getattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Option<Self::Handle>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn setattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _attr: bindings::stat64,
        _handle: Option<Self::Handle>,
        _valid: SetattrValid,
    ) -> io::Result<(bindings::stat64, Duration)> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn rename(
        &self,
        _ctx: Context,
        _olddir: Self::Inode,
        _oldname: &CStr,
        _newdir: Self::Inode,
        _newname: &CStr,
        _flags: u32,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn mknod(
        &self,
        _ctx: Context,
        _parent: Self::Inode,
        _name: &CStr,
        _mode: u32,
        _rdev: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn link(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _newparent: Self::Inode,
        _newname: &CStr,
    ) -> io::Result<Entry> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn symlink(
        &self,
        _ctx: Context,
        _linkname: &CStr,
        _parent: Self::Inode,
        _name: &CStr,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn readlink(&self, _ctx: Context, _inode: Self::Inode) -> io::Result<Vec<u8>> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn flush(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _lock_owner: u64,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn fsync(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _datasync: bool,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn fsyncdir(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _datasync: bool,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn access(&self, _ctx: Context, _inode: Self::Inode, _mask: u32) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn setxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _name: &CStr,
        _value: &[u8],
        _flags: u32,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn getxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _name: &CStr,
        _size: u32,
    ) -> io::Result<GetxattrReply> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn listxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _size: u32,
    ) -> io::Result<ListxattrReply> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn removexattr(&self, _ctx: Context, _inode: Self::Inode, _name: &CStr) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn fallocate(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _mode: u32,
        _offset: u64,
        _length: u64,
    ) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn lseek(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _offset: u64,
        _whence: u32,
    ) -> io::Result<u64> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn ioctl(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _flags: u32,
        _cmd: u32,
        _arg: u64,
        _in_size: u32,
        _out_size: u32,
        _exit_code: &std::sync::Arc<std::sync::atomic::AtomicI32>,
    ) -> io::Result<Vec<u8>> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }
}
