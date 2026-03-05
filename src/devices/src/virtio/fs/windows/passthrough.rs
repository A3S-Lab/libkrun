// Windows passthrough filesystem implementation
// Phase 1: Core data structures and basic read-only operations

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, UNIX_EPOCH};

use super::super::filesystem::{
    Context, DirEntry, Entry, ExportTable, Extensions, FileSystem, FsOptions, GetxattrReply,
    ListxattrReply, OpenOptions, SetattrValid, ZeroCopyReader, ZeroCopyWriter,
};
use super::super::bindings;

type Inode = u64;
type Handle = u64;

const ROOT_INODE: Inode = 1;

// Windows doesn't have DT_ constants in libc, so define them here
// These match the Linux values for compatibility
const DT_UNKNOWN: u8 = 0;
const DT_REG: u8 = 8;
const DT_DIR: u8 = 4;

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

/// Inode data tracking file handles and metadata
struct InodeData {
    inode: Inode,
    path: PathBuf,
    refcount: AtomicU64,
}

/// Handle data for open files/directories
struct HandleData {
    inode: Inode,
    path: PathBuf,
}

/// Windows passthrough filesystem
pub struct PassthroughFs {
    cfg: Config,
    root_dir: PathBuf,
    next_inode: AtomicU64,
    next_handle: AtomicU64,
    inodes: RwLock<BTreeMap<Inode, Arc<InodeData>>>,
    handles: RwLock<BTreeMap<Handle, Arc<HandleData>>>,
    path_to_inode: RwLock<BTreeMap<PathBuf, Inode>>,
}

impl PassthroughFs {
    pub fn new(cfg: Config) -> io::Result<PassthroughFs> {
        let root_dir = PathBuf::from(&cfg.root_dir);

        // Verify root directory exists
        if !root_dir.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Root directory does not exist: {}", cfg.root_dir),
            ));
        }

        if !root_dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Root path is not a directory: {}", cfg.root_dir),
            ));
        }

        let mut inodes = BTreeMap::new();
        let mut path_to_inode = BTreeMap::new();

        // Create root inode
        let root_inode_data = Arc::new(InodeData {
            inode: ROOT_INODE,
            path: root_dir.clone(),
            refcount: AtomicU64::new(1),
        });
        inodes.insert(ROOT_INODE, root_inode_data);
        path_to_inode.insert(root_dir.clone(), ROOT_INODE);

        Ok(PassthroughFs {
            cfg,
            root_dir,
            next_inode: AtomicU64::new(ROOT_INODE + 1),
            next_handle: AtomicU64::new(1),
            inodes: RwLock::new(inodes),
            handles: RwLock::new(BTreeMap::new()),
            path_to_inode: RwLock::new(path_to_inode),
        })
    }

    /// Allocate a new inode number
    fn allocate_inode(&self) -> Inode {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }

    /// Allocate a new handle number
    fn allocate_handle(&self) -> Handle {
        self.next_handle.fetch_add(1, Ordering::SeqCst)
    }

    /// Get or create inode for a path
    fn get_or_create_inode(&self, path: &Path) -> io::Result<Inode> {
        // Check if inode already exists
        {
            let path_map = self.path_to_inode.read().unwrap();
            if let Some(&inode) = path_map.get(path) {
                // Increment refcount
                let inodes = self.inodes.read().unwrap();
                if let Some(inode_data) = inodes.get(&inode) {
                    inode_data.refcount.fetch_add(1, Ordering::SeqCst);
                    return Ok(inode);
                }
            }
        }

        // Create new inode
        let inode = self.allocate_inode();
        let inode_data = Arc::new(InodeData {
            inode,
            path: path.to_path_buf(),
            refcount: AtomicU64::new(1),
        });

        let mut inodes = self.inodes.write().unwrap();
        let mut path_map = self.path_to_inode.write().unwrap();

        inodes.insert(inode, inode_data);
        path_map.insert(path.to_path_buf(), inode);

        Ok(inode)
    }

    /// Get path for an inode
    fn get_path(&self, inode: Inode) -> io::Result<PathBuf> {
        let inodes = self.inodes.read().unwrap();
        inodes
            .get(&inode)
            .map(|data| data.path.clone())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }

    /// Convert Windows metadata to POSIX stat64
    fn metadata_to_stat(&self, metadata: &Metadata, inode: Inode) -> bindings::stat64 {
        let mut st: bindings::stat64 = unsafe { std::mem::zeroed() };

        st.st_ino = inode as u16;  // Windows stat uses u16 for st_ino
        st.st_nlink = 1;
        st.st_mode = if metadata.is_dir() {
            (libc::S_IFDIR | 0o755) as u16
        } else if metadata.is_file() {
            (libc::S_IFREG | 0o644) as u16
        } else {
            (libc::S_IFREG | 0o644) as u16
        };

        st.st_size = metadata.len() as i64;
        // Windows stat doesn't have st_blksize and st_blocks fields

        // Convert Windows file times to Unix timestamps
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                st.st_mtime = duration.as_secs() as i64;
                // Windows stat doesn't have nanosecond precision fields
            }
        }

        if let Ok(accessed) = metadata.accessed() {
            if let Ok(duration) = accessed.duration_since(UNIX_EPOCH) {
                st.st_atime = duration.as_secs() as i64;
            }
        }

        if let Ok(created) = metadata.created() {
            if let Ok(duration) = created.duration_since(UNIX_EPOCH) {
                st.st_ctime = duration.as_secs() as i64;
            }
        }

        // Windows doesn't have uid/gid, use defaults
        st.st_uid = 1000;
        st.st_gid = 1000;

        st
    }

    /// Convert CStr to PathBuf
    fn cstr_to_path(&self, name: &CStr) -> io::Result<PathBuf> {
        let name_str = name.to_str().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Invalid UTF-8 in filename")
        })?;
        Ok(PathBuf::from(name_str))
    }
}

// FileSystem trait implementation will be added in next step
// This file is getting large, so I'll split the implementation
// FileSystem trait implementation for PassthroughFs
// Phase 1: Basic read-only operations

impl FileSystem for PassthroughFs {
    type Inode = u64;
    type Handle = u64;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        log::info!(
            "virtiofs(windows): initializing with root_dir={}",
            self.cfg.root_dir
        );

        // Return supported options
        // For now, we support basic read-only operations
        let mut opts = FsOptions::empty();
        opts.insert(FsOptions::ASYNC_READ);
        opts.insert(FsOptions::PARALLEL_DIROPS);
        opts.insert(FsOptions::BIG_WRITES);

        // Only enable features that are also supported by the client
        Ok(opts & capable)
    }

    fn destroy(&self) {
        log::info!("virtiofs(windows): destroying filesystem");
    }

    fn lookup(&self, _ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<Entry> {
        let parent_path = self.get_path(parent)?;
        let name_path = self.cstr_to_path(name)?;
        let full_path = parent_path.join(&name_path);

        // Check if file exists
        let metadata = fs::metadata(&full_path)?;

        // Get or create inode
        let inode = self.get_or_create_inode(&full_path)?;

        // Convert metadata to stat
        let st = self.metadata_to_stat(&metadata, inode);

        Ok(Entry {
            inode,
            generation: 0,
            attr: st,
            attr_flags: 0,
            attr_timeout: self.cfg.attr_timeout,
            entry_timeout: self.cfg.entry_timeout,
        })
    }

    fn forget(&self, _ctx: Context, inode: Self::Inode, count: u64) {
        let inodes = self.inodes.read().unwrap();
        if let Some(inode_data) = inodes.get(&inode) {
            let old_count = inode_data.refcount.fetch_sub(count, Ordering::SeqCst);
            if old_count <= count {
                // Refcount reached zero, can remove inode
                // But we'll keep it for now to avoid complexity
                log::debug!("virtiofs(windows): inode {} refcount reached zero", inode);
            }
        }
    }

    fn batch_forget(&self, _ctx: Context, requests: Vec<(Self::Inode, u64)>) {
        for (inode, count) in requests {
            self.forget(_ctx, inode, count);
        }
    }

    fn getattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _handle: Option<Self::Handle>,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let path = self.get_path(inode)?;
        let metadata = fs::metadata(&path)?;
        let st = self.metadata_to_stat(&metadata, inode);
        Ok((st, self.cfg.attr_timeout))
    }

    fn opendir(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        let path = self.get_path(inode)?;

        // Verify it's a directory
        let metadata = fs::metadata(&path)?;
        if !metadata.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }

        // Allocate handle
        let handle = self.allocate_handle();
        let handle_data = Arc::new(HandleData {
            inode,
            path: path.clone(),
        });

        let mut handles = self.handles.write().unwrap();
        handles.insert(handle, handle_data);

        Ok((Some(handle), OpenOptions::empty()))
    }

    fn releasedir(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
        handle: Self::Handle,
    ) -> io::Result<()> {
        let mut handles = self.handles.write().unwrap();
        handles.remove(&handle);
        Ok(())
    }

    fn readdir<F>(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _handle: Self::Handle,
        _size: u32,
        offset: u64,
        mut add_entry: F,
    ) -> io::Result<()>
    where
        F: FnMut(DirEntry) -> io::Result<usize>,
    {
        let path = self.get_path(inode)?;

        // Read directory entries
        let entries = fs::read_dir(&path)?;

        // Collect entries into a vector so we can index by offset
        let mut dir_entries: Vec<_> = entries.collect::<Result<Vec<_>, _>>()?;

        // Sort for consistent ordering
        dir_entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        // Add "." and ".." entries
        if offset == 0 {
            let dot_entry = DirEntry {
                ino: inode,
                offset: 1,
                type_: DT_DIR as u32,
                name: b".",
            };
            add_entry(dot_entry)?;
        }

        if offset <= 1 {
            // Get parent inode (or self for root)
            let parent_inode = if inode == ROOT_INODE {
                ROOT_INODE
            } else {
                // Try to get parent path
                if let Some(parent_path) = path.parent() {
                    self.get_or_create_inode(parent_path).unwrap_or(ROOT_INODE)
                } else {
                    ROOT_INODE
                }
            };

            let dotdot_entry = DirEntry {
                ino: parent_inode,
                offset: 2,
                type_: DT_DIR as u32,
                name: b"..",
            };
            add_entry(dotdot_entry)?;
        }

        // Add regular entries
        let start_idx = if offset > 2 { (offset - 2) as usize } else { 0 };

        for (idx, entry) in dir_entries.iter().enumerate().skip(start_idx) {
            let entry_path = entry.path();
            let entry_name = entry.file_name();
            let entry_name_bytes = entry_name.to_string_lossy().as_bytes().to_vec();

            // Get or create inode for this entry
            let entry_inode = self.get_or_create_inode(&entry_path).unwrap_or(0);

            // Determine entry type
            let entry_type = if let Ok(metadata) = entry.metadata() {
                if metadata.is_dir() {
                    DT_DIR
                } else if metadata.is_file() {
                    DT_REG
                } else {
                    DT_UNKNOWN
                }
            } else {
                DT_UNKNOWN
            };

            let dir_entry = DirEntry {
                ino: entry_inode,
                offset: (idx + 3) as u64, // +3 for "." and ".."
                type_: entry_type as u32,
                name: &entry_name_bytes,
            };

            // Try to add entry, stop if buffer is full
            match add_entry(dir_entry) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(libc::ENOSPC) => {
                    // Buffer full, stop here
                    break;
                }
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    // Stub implementations for other required methods
    // These will return ENOSYS for now

    fn statfs(&self, _ctx: Context, _inode: Self::Inode) -> io::Result<bindings::statvfs64> {
        // TODO: Implement statfs
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
        // TODO: Implement mkdir
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn rmdir(&self, _ctx: Context, _parent: Self::Inode, _name: &CStr) -> io::Result<()> {
        // TODO: Implement rmdir
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn open(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        // TODO: Implement open
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
        // TODO: Implement release
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
        // TODO: Implement create
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn unlink(&self, _ctx: Context, _parent: Self::Inode, _name: &CStr) -> io::Result<()> {
        // TODO: Implement unlink
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
        // TODO: Implement read
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
        // TODO: Implement write
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
        // TODO: Implement setattr
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
        // TODO: Implement rename
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
        // TODO: Implement mknod
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn link(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _newparent: Self::Inode,
        _newname: &CStr,
    ) -> io::Result<Entry> {
        // TODO: Implement link
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
        // TODO: Implement symlink
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn readlink(&self, _ctx: Context, _inode: Self::Inode) -> io::Result<Vec<u8>> {
        // TODO: Implement readlink
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn flush(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _handle: Self::Handle,
        _lock_owner: u64,
    ) -> io::Result<()> {
        // TODO: Implement flush
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn fsync(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _datasync: bool,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        // TODO: Implement fsync
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn fsyncdir(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _datasync: bool,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        // TODO: Implement fsyncdir
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn access(&self, _ctx: Context, _inode: Self::Inode, _mask: u32) -> io::Result<()> {
        // TODO: Implement access
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
        // Extended attributes not supported on Windows
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }

    fn getxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _name: &CStr,
        _size: u32,
    ) -> io::Result<GetxattrReply> {
        // Extended attributes not supported on Windows
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }

    fn listxattr(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _size: u32,
    ) -> io::Result<ListxattrReply> {
        // Extended attributes not supported on Windows
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
    }

    fn removexattr(&self, _ctx: Context, _inode: Self::Inode, _name: &CStr) -> io::Result<()> {
        // Extended attributes not supported on Windows
        Err(io::Error::from_raw_os_error(libc::ENOTSUP))
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
        // TODO: Implement fallocate
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
        // TODO: Implement lseek
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn copyfilerange(
        &self,
        _ctx: Context,
        _inode_src: Self::Inode,
        _handle_src: Self::Handle,
        _offset_src: u64,
        _inode_dst: Self::Inode,
        _handle_dst: Self::Handle,
        _offset_dst: u64,
        _length: u64,
        _flags: u64,
    ) -> io::Result<usize> {
        // TODO: Implement copy_file_range
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }
}
