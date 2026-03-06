// Windows passthrough filesystem implementation
// Phase 1: Core data structures and basic read-only operations (completed)
// Phase 2: File read operations (completed)
// Phase 3: Write operations (completed)
// Phase 4: Advanced features (completed)

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
const DT_LNK: u8 = 10;

// libc on MSVC doesn't export S_IFLNK; define it ourselves (Linux ABI value).
const S_IFLNK: u32 = 0o120_000;

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

        st.st_ino = inode; // u64 — no truncation
        st.st_nlink = 1;

        let ft = metadata.file_type();
        st.st_mode = if ft.is_dir() {
            (libc::S_IFDIR | 0o755) as u32
        } else if ft.is_symlink() {
            (S_IFLNK | 0o777) as u32
        } else {
            (libc::S_IFREG | 0o644) as u32
        };

        st.st_size = metadata.len() as i64;
        // Approximate block count (512-byte blocks, same as Linux convention)
        st.st_blksize = 4096;
        st.st_blocks = metadata.len().div_ceil(512);

        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                st.st_mtime = duration.as_secs() as i64;
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

        // Windows doesn't have uid/gid; expose as root-owned, world-readable
        st.st_uid = 0;
        st.st_gid = 0;

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

        // Use symlink_metadata (= lstat) so symlinks appear as S_IFLNK to the guest.
        let metadata = fs::symlink_metadata(&full_path)?;

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
        let metadata = fs::symlink_metadata(&path)?;
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

            // Determine entry type using symlink_metadata to detect S_IFLNK
            let entry_type = if let Ok(metadata) = fs::symlink_metadata(&entry_path) {
                let ft = metadata.file_type();
                if ft.is_dir() {
                    DT_DIR
                } else if ft.is_symlink() {
                    DT_LNK
                } else {
                    DT_REG
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

    fn statfs(&self, _ctx: Context, inode: Self::Inode) -> io::Result<bindings::statvfs64> {
        let path = self.get_path(inode)?;

        // Get disk space information using Windows API
        use std::os::windows::ffi::OsStrExt;
        use std::ffi::OsStr;

        let path_wide: Vec<u16> = OsStr::new(&path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        unsafe {
            use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
            use windows::core::PCWSTR;

            if GetDiskFreeSpaceExW(
                PCWSTR(path_wide.as_ptr()),
                Some(&mut free_bytes_available),
                Some(&mut total_bytes),
                Some(&mut total_free_bytes),
            ).is_err() {
                return Err(io::Error::last_os_error());
            }
        }

        let mut st: bindings::statvfs64 = unsafe { std::mem::zeroed() };

        // Block size (use 4KB)
        st.f_bsize = 4096;
        st.f_frsize = 4096;

        // Total blocks
        st.f_blocks = total_bytes / 4096;

        // Free blocks
        st.f_bfree = total_free_bytes / 4096;
        st.f_bavail = free_bytes_available / 4096;

        // Inode information (synthetic)
        st.f_files = 1000000; // Arbitrary large number
        st.f_ffree = 1000000;

        // Filesystem ID
        st.f_fsid = self.cfg.export_fsid;

        // Max filename length
        st.f_namemax = 255;

        Ok(st)
    }

    fn mkdir(
        &self,
        _ctx: Context,
        parent: Self::Inode,
        name: &CStr,
        _mode: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        let parent_path = self.get_path(parent)?;
        let name_str = name.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let new_path = parent_path.join(name_str);

        // Create the directory
        fs::create_dir(&new_path)?;

        // Get or create inode for the new directory
        let inode = self.get_or_create_inode(&new_path)?;

        // Get metadata
        let metadata = fs::metadata(&new_path)?;
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

    fn rmdir(&self, _ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let parent_path = self.get_path(parent)?;
        let name_str = name.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let dir_path = parent_path.join(name_str);

        // Remove the directory
        fs::remove_dir(&dir_path)?;

        // Remove from inode tracking
        let inode_opt = self.path_to_inode.write().unwrap().remove(&dir_path);
        if let Some(inode) = inode_opt {
            self.inodes.write().unwrap().remove(&inode);
        }

        Ok(())
    }

    fn open(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        let path = self.get_path(inode)?;

        // Verify the file exists and is a regular file
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() {
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }

        // Create a new handle
        let handle = self.next_handle.fetch_add(1, Ordering::SeqCst);

        // Store handle data
        let handle_data = Arc::new(HandleData {
            inode,
            path: path.clone(),
        });

        self.handles.write().unwrap().insert(handle, handle_data);

        // Determine open options based on flags
        let mut opts = OpenOptions::empty();

        // Check for direct I/O flag (O_DIRECT)
        const O_DIRECT: u32 = 0x4000;
        if flags & O_DIRECT != 0 {
            opts |= OpenOptions::DIRECT_IO;
        }

        // Check for keep cache flag
        const O_SYNC: u32 = 0x101000;
        if flags & O_SYNC == 0 {
            opts |= OpenOptions::KEEP_CACHE;
        }

        Ok((Some(handle), opts))
    }

    fn release(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        _flags: u32,
        handle: Self::Handle,
        _flush: bool,
        _flock_release: bool,
        _lock_owner: Option<u64>,
    ) -> io::Result<()> {
        // Remove the handle from our tracking
        self.handles.write().unwrap().remove(&handle);
        Ok(())
    }

    fn create(
        &self,
        _ctx: Context,
        parent: Self::Inode,
        name: &CStr,
        _mode: u32,
        flags: u32,
        _umask: u32,
        _extensions: Extensions,
    ) -> io::Result<(Entry, Option<Self::Handle>, OpenOptions)> {
        let parent_path = self.get_path(parent)?;
        let name_str = name.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let new_path = parent_path.join(name_str);

        // Create the file
        use std::fs::File;
        File::create(&new_path)?;

        // Get or create inode for the new file
        let inode = self.get_or_create_inode(&new_path)?;

        // Create a handle for the new file
        let handle = self.next_handle.fetch_add(1, Ordering::SeqCst);

        // Store handle data
        let handle_data = Arc::new(HandleData {
            inode,
            path: new_path.clone(),
        });

        self.handles.write().unwrap().insert(handle, handle_data);

        // Get metadata
        let metadata = fs::metadata(&new_path)?;
        let st = self.metadata_to_stat(&metadata, inode);

        // Determine open options based on flags
        let mut opts = OpenOptions::empty();

        const O_DIRECT: u32 = 0x4000;
        if flags & O_DIRECT != 0 {
            opts |= OpenOptions::DIRECT_IO;
        }

        const O_SYNC: u32 = 0x101000;
        if flags & O_SYNC == 0 {
            opts |= OpenOptions::KEEP_CACHE;
        }

        Ok((
            Entry {
                inode,
                generation: 0,
                attr: st,
                attr_flags: 0,
                attr_timeout: self.cfg.attr_timeout,
                entry_timeout: self.cfg.entry_timeout,
            },
            Some(handle),
            opts,
        ))
    }

    fn unlink(&self, _ctx: Context, parent: Self::Inode, name: &CStr) -> io::Result<()> {
        let parent_path = self.get_path(parent)?;
        let name_str = name.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let file_path = parent_path.join(name_str);

        // Remove the file
        fs::remove_file(&file_path)?;

        // Remove from inode tracking
        let inode_opt = self.path_to_inode.write().unwrap().remove(&file_path);
        if let Some(inode) = inode_opt {
            self.inodes.write().unwrap().remove(&inode);
        }

        Ok(())
    }

    fn read<W: io::Write + ZeroCopyWriter>(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        handle: Self::Handle,
        mut w: W,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _flags: u32,
    ) -> io::Result<usize> {
        // Get the path from the handle
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;

        let path = &handle_data.path;

        // Open the file for reading
        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};

        let mut file = File::open(path)?;

        // Seek to the requested offset
        file.seek(SeekFrom::Start(offset))?;

        // Read data into a buffer
        let mut buffer = vec![0u8; size as usize];
        let bytes_read = file.read(&mut buffer)?;

        // Write to the output writer
        if bytes_read > 0 {
            w.write_all(&buffer[..bytes_read])?;
        }

        Ok(bytes_read)
    }

    fn write<R: io::Read + ZeroCopyReader>(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        handle: Self::Handle,
        mut r: R,
        size: u32,
        offset: u64,
        _lock_owner: Option<u64>,
        _delayed_write: bool,
        _kill_priv: bool,
        _flags: u32,
    ) -> io::Result<usize> {
        // Get the path from the handle
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;

        let path = &handle_data.path;

        // Open the file for writing
        use std::fs::OpenOptions as StdOpenOptions;
        use std::io::{Seek, SeekFrom, Write};

        let mut file = StdOpenOptions::new()
            .write(true)
            .open(path)?;

        // Seek to the requested offset
        file.seek(SeekFrom::Start(offset))?;

        // Read data from the input reader and write to file
        let mut buffer = vec![0u8; size as usize];
        let bytes_read = r.read(&mut buffer)?;

        if bytes_read > 0 {
            file.write_all(&buffer[..bytes_read])?;
        }

        Ok(bytes_read)
    }

    fn setattr(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        attr: bindings::stat64,
        _handle: Option<Self::Handle>,
        valid: SetattrValid,
    ) -> io::Result<(bindings::stat64, Duration)> {
        let path = self.get_path(inode)?;

        // Handle size changes (truncate)
        if valid.contains(SetattrValid::SIZE) {
            use std::fs::OpenOptions as StdOpenOptions;
            let file = StdOpenOptions::new()
                .write(true)
                .open(&path)?;
            file.set_len(attr.st_size as u64)?;
        }

        // Handle time changes
        if valid.contains(SetattrValid::ATIME) || valid.contains(SetattrValid::MTIME) {
            use std::fs::File;
            use std::time::UNIX_EPOCH;

            let file = File::open(&path)?;

            // Windows doesn't support setting atime/mtime separately via std::fs
            // We would need to use Windows API (SetFileTime) for full support
            // For now, just update the modification time if MTIME is set
            if valid.contains(SetattrValid::MTIME) {
                let mtime = UNIX_EPOCH + Duration::from_secs(attr.st_mtime as u64);
                file.set_modified(mtime)?;
            }
        }

        // Note: Windows doesn't support POSIX permissions (mode) or ownership (uid/gid)
        // These would require mapping to Windows ACLs, which is complex
        // For now, we ignore MODE, UID, GID changes

        // Get updated metadata
        let metadata = fs::metadata(&path)?;
        let st = self.metadata_to_stat(&metadata, inode);

        Ok((st, self.cfg.attr_timeout))
    }

    fn rename(
        &self,
        _ctx: Context,
        olddir: Self::Inode,
        oldname: &CStr,
        newdir: Self::Inode,
        newname: &CStr,
        _flags: u32,
    ) -> io::Result<()> {
        let olddir_path = self.get_path(olddir)?;
        let newdir_path = self.get_path(newdir)?;

        let oldname_str = oldname.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let newname_str = newname.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;

        let old_path = olddir_path.join(oldname_str);
        let new_path = newdir_path.join(newname_str);

        // Perform the rename
        fs::rename(&old_path, &new_path)?;

        // Update inode tracking
        let mut path_to_inode = self.path_to_inode.write().unwrap();
        if let Some(inode) = path_to_inode.remove(&old_path) {
            path_to_inode.insert(new_path.clone(), inode);

            // Update the path in InodeData
            if let Some(inode_data) = self.inodes.write().unwrap().get_mut(&inode) {
                // We need to update the path, but InodeData.path is not mutable
                // For now, we'll remove and re-insert with updated path
                let new_inode_data = Arc::new(InodeData {
                    inode,
                    path: new_path,
                    refcount: AtomicU64::new(inode_data.refcount.load(Ordering::SeqCst)),
                });
                self.inodes.write().unwrap().insert(inode, new_inode_data);
            }
        }

        Ok(())
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
        linkname: &CStr,
        parent: Self::Inode,
        name: &CStr,
        _extensions: Extensions,
    ) -> io::Result<Entry> {
        let parent_path = self.get_path(parent)?;
        let name_str = name.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let link_path = parent_path.join(name_str);

        let target_str = linkname.to_str().map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let target_path = Path::new(target_str);

        // Create symbolic link using std::os::windows::fs::symlink_file or symlink_dir
        // We need to determine if target is a file or directory
        use std::os::windows::fs::{symlink_file, symlink_dir};

        // Try to determine if target is a directory
        let is_dir = if target_path.is_absolute() {
            target_path.is_dir()
        } else {
            parent_path.join(target_path).is_dir()
        };

        if is_dir {
            symlink_dir(target_path, &link_path)?;
        } else {
            symlink_file(target_path, &link_path)?;
        }

        // Get or create inode for the symlink
        let inode = self.get_or_create_inode(&link_path)?;

        // Get metadata
        let metadata = fs::symlink_metadata(&link_path)?;
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

    fn readlink(&self, _ctx: Context, inode: Self::Inode) -> io::Result<Vec<u8>> {
        let path = self.get_path(inode)?;

        // Read the symlink target
        let target = fs::read_link(&path)?;

        // Convert to bytes
        let target_str = target.to_string_lossy();
        Ok(target_str.as_bytes().to_vec())
    }

    fn flush(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        handle: Self::Handle,
        _lock_owner: u64,
    ) -> io::Result<()> {
        // Get the path from the handle
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;

        let path = &handle_data.path;

        // Open the file and sync it
        use std::fs::File;
        let file = File::open(path)?;
        file.sync_all()?;

        Ok(())
    }

    fn fsync(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        datasync: bool,
        handle: Self::Handle,
    ) -> io::Result<()> {
        // Get the path from the handle
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;

        let path = &handle_data.path;

        // Open the file and sync it
        use std::fs::File;
        let file = File::open(path)?;

        if datasync {
            // Sync only data, not metadata
            file.sync_data()?;
        } else {
            // Sync both data and metadata
            file.sync_all()?;
        }

        Ok(())
    }

    fn fsyncdir(
        &self,
        _ctx: Context,
        inode: Self::Inode,
        _datasync: bool,
        _handle: Self::Handle,
    ) -> io::Result<()> {
        // Windows doesn't require explicit directory sync
        // Directory metadata is updated automatically
        // Just verify the directory exists
        let path = self.get_path(inode)?;
        let metadata = fs::metadata(&path)?;

        if !metadata.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }

        Ok(())
    }

    fn access(&self, _ctx: Context, inode: Self::Inode, mask: u32) -> io::Result<()> {
        let path = self.get_path(inode)?;

        // Check if file exists
        let metadata = fs::metadata(&path)?;

        // Windows doesn't have POSIX permissions, so we do basic checks
        // R_OK (4), W_OK (2), X_OK (1), F_OK (0)
        const R_OK: u32 = 4;
        const W_OK: u32 = 2;
        const X_OK: u32 = 1;

        // Check read access
        if mask & R_OK != 0 {
            // On Windows, if we can get metadata, we can read
            // More sophisticated check would use Windows ACLs
        }

        // Check write access
        if mask & W_OK != 0 {
            if metadata.permissions().readonly() {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
        }

        // Check execute access
        if mask & X_OK != 0 {
            // On Windows, check if it's a directory or has .exe/.bat/.cmd extension
            if !metadata.is_dir() {
                if let Some(ext) = path.extension() {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    if ext_str != "exe" && ext_str != "bat" && ext_str != "cmd" {
                        return Err(io::Error::from_raw_os_error(libc::EACCES));
                    }
                } else {
                    return Err(io::Error::from_raw_os_error(libc::EACCES));
                }
            }
        }

        Ok(())
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
        handle: Self::Handle,
        _mode: u32,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        // Get the path from the handle
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;

        let path = &handle_data.path;

        // Open the file and set its length
        use std::fs::OpenOptions as StdOpenOptions;

        let file = StdOpenOptions::new()
            .write(true)
            .open(path)?;

        let new_size = offset + length;
        file.set_len(new_size)?;

        Ok(())
    }

    fn lseek(
        &self,
        _ctx: Context,
        _inode: Self::Inode,
        handle: Self::Handle,
        offset: u64,
        whence: u32,
    ) -> io::Result<u64> {
        // Get the path from the handle
        let handles = self.handles.read().unwrap();
        let handle_data = handles
            .get(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;

        let path = &handle_data.path;

        // Open the file
        use std::fs::File;
        use std::io::{Seek, SeekFrom};

        let mut file = File::open(path)?;

        // SEEK_SET = 0, SEEK_CUR = 1, SEEK_END = 2
        // SEEK_DATA = 3, SEEK_HOLE = 4 (not supported on Windows)
        const SEEK_SET: u32 = 0;
        const SEEK_CUR: u32 = 1;
        const SEEK_END: u32 = 2;

        let seek_from = match whence {
            SEEK_SET => SeekFrom::Start(offset),
            SEEK_CUR => SeekFrom::Current(offset as i64),
            SEEK_END => SeekFrom::End(offset as i64),
            _ => return Err(io::Error::from_raw_os_error(libc::EINVAL)),
        };

        let new_offset = file.seek(seek_from)?;
        Ok(new_offset)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use tempfile::TempDir;

    fn make_fs(root: &std::path::Path) -> PassthroughFs {
        let cfg = Config {
            root_dir: root.to_string_lossy().to_string(),
            ..Default::default()
        };
        PassthroughFs::new(cfg).expect("PassthroughFs::new")
    }

    fn ctx() -> Context {
        Context { uid: 0, gid: 0, pid: 0 }
    }

    #[test]
    fn test_virtiofs_windows_lookup_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, b"hello").unwrap();

        let fs = make_fs(dir.path());
        let name = CString::new("hello.txt").unwrap();
        let entry = fs.lookup(ctx(), ROOT_INODE, &name).expect("lookup");

        // st_ino must be a valid non-zero u64 (not truncated to u16)
        assert_ne!(entry.attr.st_ino, 0);
        assert_eq!(entry.attr.st_size, 5);
        // Regular file mode
        let mode = entry.attr.st_mode & 0xf000;
        assert_eq!(mode, (libc::S_IFREG as u32) & 0xf000);
    }

    #[test]
    fn test_virtiofs_windows_lookup_dir() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();

        let fs = make_fs(dir.path());
        let name = CString::new("subdir").unwrap();
        let entry = fs.lookup(ctx(), ROOT_INODE, &name).expect("lookup dir");

        let mode = entry.attr.st_mode & 0xf000;
        assert_eq!(mode, (libc::S_IFDIR as u32) & 0xf000);
    }

    #[test]
    fn test_virtiofs_windows_large_inode() {
        // Verify that inode numbers > 65535 are not truncated.
        // Allocate enough inodes to exceed u16::MAX.
        let dir = TempDir::new().unwrap();
        let fs = make_fs(dir.path());

        // Fast path: directly manipulate the counter.
        fs.next_inode.store(70_000, std::sync::atomic::Ordering::Relaxed);

        let path = dir.path().join("probe.txt");
        std::fs::write(&path, b"x").unwrap();
        let inode = fs.get_or_create_inode(&path).unwrap();
        assert_eq!(inode, 70_000, "inode should be 70000, not truncated to u16");

        let name = CString::new("probe.txt").unwrap();
        let entry = fs.lookup(ctx(), ROOT_INODE, &name).expect("lookup");
        assert_eq!(entry.attr.st_ino, 70_000, "st_ino must not be truncated");
    }

    #[test]
    fn test_virtiofs_windows_readdir_types() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"data").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let fs = make_fs(dir.path());
        let (_, _opts) = fs.opendir(ctx(), ROOT_INODE, 0).unwrap();
        let handle = 0u64;

        let mut entries: Vec<(u64, u32, String)> = Vec::new();
        fs.readdir(ctx(), ROOT_INODE, handle, 4096, 0, |e| {
            entries.push((e.ino, e.type_, String::from_utf8_lossy(e.name).to_string()));
            Ok(1)
        })
        .unwrap();

        let types: std::collections::HashMap<String, u32> =
            entries.into_iter().map(|(_, t, n)| (n, t)).collect();

        assert_eq!(types["subdir"], DT_DIR as u32, "subdir must be DT_DIR");
        assert_eq!(types["file.txt"], DT_REG as u32, "file.txt must be DT_REG");
    }
}
