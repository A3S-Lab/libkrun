use std::collections::BTreeMap;

use arch::ArchMemoryInfo;
use vm_memory::GuestAddress;
use vmm_sys_util::align_upwards;

#[derive(Debug)]
pub enum Error {
    DuplicatedGpuRegion,
    OutOfSpace,
}

#[derive(Clone)]
pub struct ShmRegion {
    pub guest_addr: GuestAddress,
    pub size: usize,
}

pub struct ShmManager {
    next_guest_addr: u64,
    page_size: usize,
    fs_regions: BTreeMap<usize, ShmRegion>,
    gpu_region: Option<ShmRegion>,
}

impl ShmManager {
    pub fn new(info: &ArchMemoryInfo) -> ShmManager {
        Self {
            next_guest_addr: info.shm_start_addr,
            page_size: info.page_size,
            fs_regions: BTreeMap::new(),
            gpu_region: None,
        }
    }

    pub fn regions(&self) -> Vec<(GuestAddress, usize)> {
        let mut regions: Vec<(GuestAddress, usize)> = Vec::new();

        for region in self.fs_regions.iter() {
            regions.push((region.1.guest_addr, region.1.size));
        }

        if let Some(region) = &self.gpu_region {
            regions.push((region.guest_addr, region.size));
        }

        regions
    }

    #[cfg(not(any(feature = "tee", feature = "nitro")))]
    pub fn fs_region(&self, index: usize) -> Option<&ShmRegion> {
        self.fs_regions.get(&index)
    }

    #[cfg(feature = "gpu")]
    pub fn gpu_region(&self) -> Option<&ShmRegion> {
        self.gpu_region.as_ref()
    }

    fn create_region(&mut self, size: usize) -> Result<ShmRegion, Error> {
        let size = align_upwards!(size, self.page_size);

        let region = ShmRegion {
            guest_addr: GuestAddress(self.next_guest_addr),
            size,
        };

        if let Some(addr) = self.next_guest_addr.checked_add(size as u64) {
            self.next_guest_addr = addr;
            Ok(region)
        } else {
            Err(Error::OutOfSpace)
        }
    }

    pub fn create_gpu_region(&mut self, size: usize) -> Result<(), Error> {
        if self.gpu_region.is_some() {
            Err(Error::DuplicatedGpuRegion)
        } else {
            self.gpu_region = Some(self.create_region(size)?);
            Ok(())
        }
    }

    #[cfg(not(feature = "tee"))]
    pub fn create_fs_region(&mut self, index: usize, size: usize) -> Result<(), Error> {
        let region = self.create_region(size)?;
        self.fs_regions.insert(index, region);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_arch_info() -> ArchMemoryInfo {
        ArchMemoryInfo {
            shm_start_addr: 0x1_0000_0000,
            page_size: 4096,
            ..Default::default()
        }
    }

    #[test]
    fn test_shm_manager_new() {
        let info = create_test_arch_info();
        let manager = ShmManager::new(&info);

        assert!(manager.fs_regions.is_empty());
        assert!(manager.gpu_region.is_none());
    }

    #[test]
    fn test_shm_region_clone() {
        let region = ShmRegion {
            guest_addr: GuestAddress(0x1000),
            size: 4096,
        };
        let cloned = region.clone();
        assert_eq!(region.guest_addr, cloned.guest_addr);
        assert_eq!(region.size, cloned.size);
    }

    #[test]
    fn test_shm_manager_regions_empty() {
        let info = create_test_arch_info();
        let manager = ShmManager::new(&info);

        let regions = manager.regions();
        assert!(regions.is_empty());
    }

    #[test]
    fn test_create_region() {
        let info = create_test_arch_info();
        let mut manager = ShmManager::new(&info);

        let region = manager.create_region(4096).unwrap();
        assert_eq!(region.guest_addr, GuestAddress(0x1_0000_0000));
        assert_eq!(region.size, 4096);
    }

    #[test]
    fn test_create_region_aligned() {
        let info = create_test_arch_info();
        let mut manager = ShmManager::new(&info);

        // Request size that is not page-aligned
        let region = manager.create_region(100).unwrap();
        // Should be aligned up to page size
        assert!(region.size >= 100);
        assert_eq!(region.size % 4096, 0);
    }

    #[test]
    fn test_create_region_out_of_space() {
        let info = create_test_arch_info();
        let mut manager = ShmManager::new(&info);

        // Set next_guest_addr close to overflow
        manager.next_guest_addr = u64::MAX - 100;

        let result = manager.create_region(4096);
        assert!(matches!(result, Err(Error::OutOfSpace)));
    }

    #[test]
    fn test_create_gpu_region() {
        let info = create_test_arch_info();
        let mut manager = ShmManager::new(&info);

        let result = manager.create_gpu_region(8192);
        assert!(result.is_ok());

        let gpu_region = manager.gpu_region.as_ref();
        assert!(gpu_region.is_some());
        assert_eq!(gpu_region.unwrap().size, 8192);
    }

    #[test]
    fn test_create_gpu_region_duplicated() {
        let info = create_test_arch_info();
        let mut manager = ShmManager::new(&info);

        manager.create_gpu_region(4096).unwrap();
        let result = manager.create_gpu_region(4096);
        assert!(matches!(result, Err(Error::DuplicatedGpuRegion)));
    }

    #[test]
    fn test_regions_with_gpu() {
        let info = create_test_arch_info();
        let mut manager = ShmManager::new(&info);

        manager.create_gpu_region(4096).unwrap();

        let regions = manager.regions();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].0, GuestAddress(0x1_0000_0000));
        assert_eq!(regions[0].1, 4096);
    }

    #[test]
    fn test_error_debug() {
        let err = Error::DuplicatedGpuRegion;
        assert_eq!(format!("{:?}", err), "DuplicatedGpuRegion");

        let err = Error::OutOfSpace;
        assert_eq!(format!("{:?}", err), "OutOfSpace");
    }
}
