// Copyright 2024, Red Hat Inc. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub enum KernelFormat {
    // Raw image, ready to be loaded into the VM.
    #[default]
    Raw,
    // ELF image, need to locale sections be loaded.
    Elf,
    // Raw image compressed with GZIP, embedded into a PE file.
    PeGz,
    // ELF image compressed with BZIP2, embedded into an Image file.
    ImageBz2,
    // ELF image compressed with GZIP, embedded into an Image file.
    ImageGz,
    // ELF image compressed with ZSTD, embedded into an Image file.
    ImageZstd,
}

/// Data structure holding the attributes read from the `libkrunfw` kernel config.
#[derive(Clone, Debug, Default)]
pub struct ExternalKernel {
    pub path: PathBuf,
    pub format: KernelFormat,
    pub initramfs_path: Option<PathBuf>,
    pub initramfs_size: u64,
    pub cmdline: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kernel_format_default() {
        let format = KernelFormat::default();
        assert!(matches!(format, KernelFormat::Raw));
    }

    #[test]
    fn test_kernel_format_variants() {
        assert!(matches!(KernelFormat::Raw, KernelFormat::Raw));
        assert!(matches!(KernelFormat::Elf, KernelFormat::Elf));
        assert!(matches!(KernelFormat::PeGz, KernelFormat::PeGz));
        assert!(matches!(KernelFormat::ImageBz2, KernelFormat::ImageBz2));
        assert!(matches!(KernelFormat::ImageGz, KernelFormat::ImageGz));
        assert!(matches!(KernelFormat::ImageZstd, KernelFormat::ImageZstd));
    }

    #[test]
    fn test_kernel_format_clone() {
        let format = KernelFormat::ImageGz;
        let cloned = format.clone();
        assert!(matches!(cloned, KernelFormat::ImageGz));
    }

    #[test]
    fn test_kernel_format_debug() {
        let format = KernelFormat::PeGz;
        let debug_str = format!("{:?}", format);
        assert_eq!(debug_str, "PeGz");
    }

    #[test]
    fn test_external_kernel_default() {
        let kernel = ExternalKernel::default();
        assert!(kernel.path.as_os_str().is_empty());
        assert!(matches!(kernel.format, KernelFormat::Raw));
        assert!(kernel.initramfs_path.is_none());
        assert_eq!(kernel.initramfs_size, 0);
        assert!(kernel.cmdline.is_none());
    }

    #[test]
    fn test_external_kernel_clone() {
        let kernel = ExternalKernel {
            path: PathBuf::from("/boot/vmlinuz"),
            format: KernelFormat::ImageGz,
            initramfs_path: Some(PathBuf::from("/boot/initrd")),
            initramfs_size: 0x5000,
            cmdline: Some("console=ttyS0".to_string()),
        };
        let cloned = kernel.clone();
        assert_eq!(kernel.path, cloned.path);
        assert_eq!(kernel.format, cloned.format);
        assert_eq!(kernel.initramfs_path, cloned.initramfs_path);
        assert_eq!(kernel.initramfs_size, cloned.initramfs_size);
        assert_eq!(kernel.cmdline, cloned.cmdline);
    }

    #[test]
    fn test_external_kernel_with_cmdline() {
        let kernel = ExternalKernel {
            path: PathBuf::from("/kernel"),
            format: KernelFormat::Elf,
            initramfs_path: None,
            initramfs_size: 0,
            cmdline: Some("quiet".to_string()),
        };
        assert_eq!(kernel.cmdline, Some("quiet".to_string()));
    }

    #[test]
    fn test_external_kernel_with_initramfs() {
        let kernel = ExternalKernel {
            path: PathBuf::from("/kernel"),
            format: KernelFormat::Raw,
            initramfs_path: Some(PathBuf::from("/initramfs.cpio")),
            initramfs_size: 0x1234,
            cmdline: None,
        };
        assert!(kernel.initramfs_path.is_some());
        assert_eq!(kernel.initramfs_size, 0x1234);
    }
}
