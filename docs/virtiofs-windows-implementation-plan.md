# Virtiofs Windows Implementation Plan

## Executive Summary

Implementing virtiofs on Windows is a **2-4 week project** requiring:
1. Windows file system API adaptation
2. FUSE protocol implementation
3. Inode/handle management
4. Permission and security mapping

## Phase 1: Foundation (Days 1-3) ✅ START HERE

### Goal: Basic read-only filesystem with minimal operations

### Tasks:
1. ✅ Implement core data structures
   - InodeData: Track file handles and metadata
   - HandleData: Track open file handles
   - Inode/Handle maps

2. ✅ Implement basic operations:
   - `init()`: Initialize filesystem
   - `lookup()`: Look up file/directory by name
   - `getattr()`: Get file attributes
   - `opendir()`: Open directory
   - `readdir()`: Read directory entries
   - `releasedir()`: Close directory

3. ✅ Windows API mapping:
   - Use `std::fs` for basic operations
   - Map Windows file attributes to POSIX stat
   - Handle path conversion (Windows → POSIX)

### Success Criteria:
- Can mount virtiofs in guest
- Can list root directory
- Can read file metadata

## Phase 2: File Operations (Days 4-7)

### Goal: Read-only file access

### Tasks:
1. Implement file operations:
   - `open()`: Open file for reading
   - `read()`: Read file data
   - `release()`: Close file
   - `statfs()`: Get filesystem statistics

2. Implement zero-copy I/O:
   - `ZeroCopyReader` for efficient data transfer
   - Buffer management

### Success Criteria:
- Can read files from guest
- Performance is acceptable (>100 MB/s)

## Phase 3: Write Operations (Days 8-12)

### Goal: Full read-write filesystem

### Tasks:
1. Implement write operations:
   - `create()`: Create new file
   - `write()`: Write file data
   - `unlink()`: Delete file
   - `mkdir()`: Create directory
   - `rmdir()`: Remove directory
   - `rename()`: Rename file/directory

2. Implement attribute operations:
   - `setattr()`: Set file attributes
   - `chmod()`: Change permissions (map to Windows ACLs)
   - `chown()`: Change ownership (limited on Windows)

### Success Criteria:
- Can create/modify/delete files
- Can create/delete directories
- Basic permission handling works

## Phase 4: Advanced Features (Days 13-20)

### Goal: Production-ready filesystem

### Tasks:
1. Implement advanced operations:
   - `link()`: Hard links (if supported)
   - `symlink()`: Symbolic links
   - `readlink()`: Read symlink target
   - `fsync()`: Sync file data
   - `flush()`: Flush file data

2. Implement extended attributes (if needed):
   - `getxattr()`: Get extended attribute
   - `setxattr()`: Set extended attribute
   - `listxattr()`: List extended attributes
   - `removexattr()`: Remove extended attribute

3. Performance optimization:
   - Caching strategy
   - Batch operations
   - Async I/O

4. Error handling:
   - Proper error mapping (Windows → POSIX errno)
   - Recovery from failures
   - Logging and diagnostics

### Success Criteria:
- All common file operations work
- Performance is good (>500 MB/s for large files)
- Stable under stress testing

## Technical Challenges

### 1. Path Handling
**Challenge**: Windows uses backslashes, POSIX uses forward slashes
**Solution**: Convert paths at the boundary, use `PathBuf` internally

### 2. Permissions
**Challenge**: Windows ACLs vs POSIX permissions
**Solution**:
- Map basic permissions (read/write/execute)
- Ignore complex ACLs for now
- Use default permissions for new files

### 3. Inode Numbers
**Challenge**: Windows doesn't have stable inode numbers
**Solution**:
- Generate synthetic inodes
- Use file ID (GetFileInformationByHandle) as basis
- Maintain inode → path mapping

### 4. File Locking
**Challenge**: Different locking semantics
**Solution**:
- Use Windows file locking APIs
- Map POSIX lock types to Windows equivalents

### 5. Case Sensitivity
**Challenge**: Windows is case-insensitive by default
**Solution**:
- Preserve case in filenames
- Handle case-insensitive lookups
- Document limitations

## Implementation Strategy

### Minimal Viable Product (MVP)
Focus on Phase 1-2 first (read-only filesystem):
- Sufficient for many use cases (config files, read-only data)
- Faster to implement (1 week)
- Lower risk

### Full Implementation
Complete all phases for production use:
- Required for container workloads
- Needed for a3s box
- 2-4 weeks total

## Decision Point

**Question for user**: Which approach do you prefer?

**Option A: MVP First (1 week)**
- Implement read-only filesystem
- Test with real workloads
- Decide if write support is needed

**Option B: Full Implementation (2-4 weeks)**
- Implement complete filesystem
- Production-ready from start
- Higher upfront investment

**Recommendation**: Start with Option A (MVP), then evaluate based on a3s box requirements.

## Next Steps

If approved, I will:
1. Create task list for Phase 1
2. Implement core data structures
3. Implement basic operations (lookup, getattr, readdir)
4. Add smoke tests
5. Iterate based on feedback

---

*Created: 2026-03-05*
*Estimated effort: 2-4 weeks*
