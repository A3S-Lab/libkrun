use std::collections::HashSet;
use std::ffi::CString;

use tempfile::TempDir;

use super::*;

fn context() -> Context {
    Context {
        uid: 0,
        gid: 0,
        pid: 0,
    }
}

#[test]
fn readdir_cookies_survive_removing_previously_returned_entries() {
    const ENTRY_COUNT: usize = 5_000;
    const PAGE_ENTRIES: usize = 1_024;

    let root = TempDir::new().expect("temporary virtio-fs root");
    for index in 0..ENTRY_COUNT {
        std::fs::write(root.path().join(format!("entry-{index:04}")), b"")
            .expect("create directory entry");
    }

    let fs = PassthroughFs::new(Config {
        root_dir: root.path().to_string_lossy().into_owned(),
        ..Config::default()
    })
    .expect("create passthrough filesystem");
    fs.init(FsOptions::empty()).expect("initialize filesystem");
    let handle = fs
        .opendir(context(), fuse::ROOT_ID, 0)
        .expect("open root directory")
        .0
        .expect("directory handle");

    let mut offset = 0;
    let mut seen = HashSet::new();
    for _ in 0..ENTRY_COUNT * 2 {
        let mut accepted = Vec::new();
        fs.readdir(context(), fuse::ROOT_ID, handle, 4096, offset, |entry| {
            if accepted.len() == PAGE_ENTRIES {
                return Ok(0);
            }
            accepted.push((entry.name.to_vec(), entry.offset));
            Ok(1)
        })
        .expect("read directory page");

        if accepted.is_empty() {
            break;
        }
        for (name, next_offset) in accepted {
            let name = CString::new(name).expect("entry name");
            assert!(
                seen.insert(name.to_bytes().to_vec()),
                "readdir returned an entry twice: {}",
                name.to_string_lossy()
            );
            fs.unlink(context(), fuse::ROOT_ID, &name)
                .expect("remove returned entry");
            offset = next_offset;
        }
    }

    assert_eq!(seen.len(), ENTRY_COUNT, "readdir skipped unrelated entries");
    assert_eq!(
        std::fs::read_dir(root.path())
            .expect("read root directory")
            .count(),
        0,
        "returned-entry removal left files behind"
    );
    fs.releasedir(context(), fuse::ROOT_ID, 0, handle)
        .expect("release root directory");
}

#[test]
fn rewinding_readdir_refreshes_the_handle_snapshot() {
    let root = TempDir::new().expect("temporary virtio-fs root");
    std::fs::write(root.path().join("first"), b"").expect("create first entry");

    let fs = PassthroughFs::new(Config {
        root_dir: root.path().to_string_lossy().into_owned(),
        ..Config::default()
    })
    .expect("create passthrough filesystem");
    fs.init(FsOptions::empty()).expect("initialize filesystem");
    let handle = fs
        .opendir(context(), fuse::ROOT_ID, 0)
        .expect("open root directory")
        .0
        .expect("directory handle");

    let mut first_read = Vec::new();
    fs.readdir(context(), fuse::ROOT_ID, handle, 4096, 0, |entry| {
        first_read.push(entry.name.to_vec());
        Ok(1)
    })
    .expect("read initial snapshot");
    assert_eq!(first_read, [b"first".to_vec()]);

    std::fs::write(root.path().join("second"), b"").expect("create second entry");
    let mut rewound_read = HashSet::new();
    fs.readdir(context(), fuse::ROOT_ID, handle, 4096, 0, |entry| {
        rewound_read.insert(entry.name.to_vec());
        Ok(1)
    })
    .expect("read refreshed snapshot");
    assert_eq!(
        rewound_read,
        HashSet::from([b"first".to_vec(), b"second".to_vec()])
    );

    fs.releasedir(context(), fuse::ROOT_ID, 0, handle)
        .expect("release root directory");
}
