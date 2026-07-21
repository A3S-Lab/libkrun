// Copyright 2019 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

pub use vmm_sys_util::{errno, tempfile};
#[cfg(target_os = "linux")]
pub use vmm_sys_util::{eventfd, ioctl};
#[cfg(not(target_os = "windows"))]
pub use vmm_sys_util::{tempdir, terminal};

pub mod byte_order;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::epoll;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::epoll;
#[cfg(target_os = "macos")]
pub use macos::eventfd;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub use windows::epoll;
#[cfg(target_os = "windows")]
pub use windows::eventfd;
#[cfg(not(target_os = "windows"))]
pub mod pollable_channel;
#[cfg(target_arch = "x86_64")]
pub mod rand;
#[cfg(target_os = "linux")]
pub mod signal;
pub mod sized_vec;
pub mod sm;
pub mod syscall;
pub mod time;
pub mod worker_message;

/// Append a Windows diagnostic message when an explicit log directory is set.
///
/// File logging is disabled by default. Set `LIBKRUN_WINDOWS_DEBUG_LOG_DIR`
/// before the process starts to opt in. Callers provide only a file name; path
/// separators and other non-normal components are rejected.
#[cfg(target_os = "windows")]
pub fn windows_debug_log(file_name: &str, message: impl AsRef<str>) {
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
    let Some(directory) = DIRECTORY
        .get_or_init(|| std::env::var_os("LIBKRUN_WINDOWS_DEBUG_LOG_DIR").map(PathBuf::from))
        .as_deref()
    else {
        return;
    };
    let Some(path) = windows_debug_log_path(directory, file_name) else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{}", message.as_ref());
    }
}

#[cfg(target_os = "windows")]
fn windows_debug_log_path(
    directory: &std::path::Path,
    file_name: &str,
) -> Option<std::path::PathBuf> {
    use std::path::{Component, Path};

    let mut components = Path::new(file_name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) => Some(directory.join(name)),
        _ => None,
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_debug_log_tests {
    use super::windows_debug_log_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn diagnostic_path_is_scoped_to_the_opt_in_directory() {
        let directory = Path::new(r"C:\diagnostics");
        assert_eq!(
            windows_debug_log_path(directory, "whpx.log"),
            Some(PathBuf::from(r"C:\diagnostics\whpx.log"))
        );
        assert_eq!(windows_debug_log_path(directory, r"..\escape.log"), None);
        assert_eq!(windows_debug_log_path(directory, r"C:\escape.log"), None);
    }
}
