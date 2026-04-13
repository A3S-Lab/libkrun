// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Implements platform specific functionality.
//! Supported platforms: x86_64 and aarch64.

use std::result;

#[derive(Default)]
pub struct ArchMemoryInfo {
    #[cfg(target_arch = "x86_64")]
    pub ram_below_gap: u64,
    #[cfg(target_arch = "x86_64")]
    pub ram_above_gap: u64,
    #[cfg(target_arch = "aarch64")]
    pub ram_start_addr: u64,
    pub ram_last_addr: u64,
    pub shm_start_addr: u64,
    pub page_size: usize,
    #[cfg(target_arch = "aarch64")]
    pub fdt_addr: u64,
    pub initrd_addr: u64,
    pub firmware_addr: u64,
}

/// Module for aarch64 related functionality.
#[cfg(target_arch = "aarch64")]
pub mod aarch64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::{
    arch_memory_regions, configure_system, layout::CMDLINE_MAX_SIZE, layout::IRQ_BASE,
    layout::IRQ_MAX, layout::RESET_VECTOR, Error, MMIO_MEM_START,
};

/// Module for riscv64 related functionality.
#[cfg(target_arch = "riscv64")]
pub mod riscv64;

#[cfg(target_arch = "riscv64")]
pub use riscv64::{
    arch_memory_regions, configure_system, layout::CMDLINE_MAX_SIZE, layout::IRQ_BASE,
    layout::IRQ_MAX, layout::RESET_VECTOR, Error, MMIO_MEM_START,
};

/// Module for x86_64 related functionality.
#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use crate::x86_64::{
    arch_memory_regions, configure_system, layout::CMDLINE_MAX_SIZE, layout::FIRMWARE_SIZE,
    layout::FIRMWARE_START, layout::IRQ_BASE, layout::IRQ_MAX, layout::MMIO_MEM_START,
    layout::RESET_VECTOR, Error,
};

/// Type for returning public functions outcome.
pub type Result<T> = result::Result<T, Error>;

/// Type for passing information about the initrd in the guest memory.
pub struct InitrdConfig {
    /// Load address of initrd in guest memory
    pub address: vm_memory::GuestAddress,
    /// Size of initrd in guest memory
    pub size: usize,
}

/// Default (smallest) memory page size for the supported architectures.
pub const PAGE_SIZE: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arch_memory_info_default() {
        let info = ArchMemoryInfo::default();
        assert_eq!(info.ram_last_addr, 0);
        assert_eq!(info.shm_start_addr, 0);
        assert_eq!(info.page_size, 0);
        assert_eq!(info.initrd_addr, 0);
        assert_eq!(info.firmware_addr, 0);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_arch_memory_info_x86_64() {
        let mut info = ArchMemoryInfo::default();
        info.ram_below_gap = 0x1000;
        info.ram_above_gap = 0x2000;
        info.ram_last_addr = 0xFFFF_FFFF;
        info.shm_start_addr = 0x1_0000_0000;
        info.page_size = 4096;
        info.initrd_addr = 0x2000_0000;
        info.firmware_addr = 0xFFFC_0000;

        assert_eq!(info.ram_below_gap, 0x1000);
        assert_eq!(info.ram_above_gap, 0x2000);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn test_arch_memory_info_aarch64() {
        let mut info = ArchMemoryInfo::default();
        info.ram_start_addr = 0x4000_0000;
        info.ram_last_addr = 0x7F_FFFF_FFFF;
        info.shm_start_addr = 0x1_0000_0000;
        info.page_size = 4096;
        info.fdt_addr = 0x4000_0000;
        info.initrd_addr = 0x4_0000_0000;
        info.firmware_addr = 0x0;

        assert_eq!(info.ram_start_addr, 0x4000_0000);
        assert_eq!(info.fdt_addr, 0x4000_0000);
    }

    #[test]
    fn test_initrd_config() {
        use vm_memory::GuestAddress;

        let initrd = InitrdConfig {
            address: GuestAddress(0x2000_0000),
            size: 0x5000,
        };

        assert_eq!(initrd.address, GuestAddress(0x2000_0000));
        assert_eq!(initrd.size, 0x5000);
    }

    #[test]
    fn test_initrd_config_default() {
        let initrd = InitrdConfig {
            address: vm_memory::GuestAddress(0),
            size: 0,
        };

        assert_eq!(initrd.address, vm_memory::GuestAddress(0));
        assert_eq!(initrd.size, 0);
    }

    #[test]
    fn test_page_size_constant() {
        assert_eq!(PAGE_SIZE, 4096);
    }

    #[test]
    fn test_arch_memory_info_clone() {
        let mut info = ArchMemoryInfo::default();
        info.ram_last_addr = 0xFFFF_FFFF;
        info.shm_start_addr = 0x1_0000_0000;
        info.page_size = 4096;

        let cloned = info.clone();
        assert_eq!(info.ram_last_addr, cloned.ram_last_addr);
        assert_eq!(info.shm_start_addr, cloned.shm_start_addr);
        assert_eq!(info.page_size, cloned.page_size);
    }
}
