// libkrunfw.dll - Windows companion for libkrun.
//
// ELF parsing and raw metadata validation happen in build.rs. The build emits
// only bounded, validated copy descriptors and addresses for this runtime, so
// krunfw_get_kernel() never has to interpret untrusted kernel bytes.

use std::alloc::{alloc_zeroed, Layout};
use std::ffi::c_char;
use std::ptr;
use std::sync::OnceLock;

const GUEST_LOAD_ALIGNMENT: u64 = 4096;

static KERNEL_IMAGE: &[u8] = include_bytes!(env!("LIBKRUNFW_EMBEDDED_KERNEL_PATH"));

#[derive(Clone, Copy, Debug)]
struct EmbeddedLoadSegment {
    file_offset: usize,
    file_size: usize,
    destination_offset: usize,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum EmbeddedKernelSource {
    Elf {
        guest_load_addr: u64,
        entry_addr: u64,
        image_size: usize,
        segments: &'static [EmbeddedLoadSegment],
    },
    RawBundle {
        guest_load_addr: u64,
        entry_addr: u64,
    },
}

include!(concat!(env!("OUT_DIR"), "/kernel_source_generated.rs"));

struct PreparedKernel {
    guest_addr: u64,
    entry_addr: u64,
    size: usize,
    ptr: usize,
}

type PrepareResult = Result<PreparedKernel, &'static str>;
static PREPARED_KERNEL: OnceLock<PrepareResult> = OnceLock::new();

fn valid_guest_load_address(address: u64) -> bool {
    address != 0 && address & (GUEST_LOAD_ALIGNMENT - 1) == 0
}

fn allocate_page_aligned(size: usize) -> Result<*mut u8, &'static str> {
    if size == 0 {
        return Err("kernel image is empty");
    }
    let layout =
        Layout::from_size_align(size, 4096).map_err(|_| "kernel allocation size is invalid")?;
    let pointer = unsafe { alloc_zeroed(layout) };
    if pointer.is_null() {
        return Err("page-aligned kernel allocation failed");
    }
    Ok(pointer)
}

fn prepare_validated_elf(
    kernel_elf: &[u8],
    guest_load_addr: u64,
    entry_addr: u64,
    image_size: usize,
    segments: &[EmbeddedLoadSegment],
) -> PrepareResult {
    if !valid_guest_load_address(guest_load_addr) {
        return Err("generated ELF guest load address is not 4096-byte aligned");
    }
    if entry_addr == 0 || segments.is_empty() {
        return Err("generated ELF metadata is incomplete");
    }

    for segment in segments {
        let source_end = segment
            .file_offset
            .checked_add(segment.file_size)
            .ok_or("generated ELF source range overflows usize")?;
        if source_end > kernel_elf.len() {
            return Err("generated ELF source range exceeds embedded image");
        }
        let destination_end = segment
            .destination_offset
            .checked_add(segment.file_size)
            .ok_or("generated ELF destination range overflows usize")?;
        if destination_end > image_size {
            return Err("generated ELF destination range exceeds guest image");
        }
    }

    let pointer = allocate_page_aligned(image_size)?;
    for segment in segments {
        let source = &kernel_elf[segment.file_offset..segment.file_offset + segment.file_size];
        unsafe {
            ptr::copy_nonoverlapping(
                source.as_ptr(),
                pointer.add(segment.destination_offset),
                source.len(),
            );
        }
    }

    Ok(PreparedKernel {
        guest_addr: guest_load_addr,
        entry_addr,
        size: image_size,
        ptr: pointer as usize,
    })
}

fn prepare_validated_raw(
    raw_bundle: &[u8],
    guest_load_addr: u64,
    entry_addr: u64,
) -> PrepareResult {
    if !valid_guest_load_address(guest_load_addr) {
        return Err("generated raw bundle guest load address is not 4096-byte aligned");
    }
    if entry_addr == 0 {
        return Err("generated raw bundle metadata is incomplete");
    }
    let pointer = allocate_page_aligned(raw_bundle.len())?;
    unsafe {
        ptr::copy_nonoverlapping(raw_bundle.as_ptr(), pointer, raw_bundle.len());
    }
    Ok(PreparedKernel {
        guest_addr: guest_load_addr,
        entry_addr,
        size: raw_bundle.len(),
        ptr: pointer as usize,
    })
}

fn initialize_kernel() -> PrepareResult {
    match KERNEL_SOURCE {
        EmbeddedKernelSource::Elf {
            guest_load_addr,
            entry_addr,
            image_size,
            segments,
        } => prepare_validated_elf(
            KERNEL_IMAGE,
            guest_load_addr,
            entry_addr,
            image_size,
            segments,
        ),
        EmbeddedKernelSource::RawBundle {
            guest_load_addr,
            entry_addr,
        } => prepare_validated_raw(KERNEL_IMAGE, guest_load_addr, entry_addr),
    }
}

fn prepare_kernel() -> Option<&'static PreparedKernel> {
    PREPARED_KERNEL.get_or_init(initialize_kernel).as_ref().ok()
}

#[no_mangle]
/// Returns the process-lifetime kernel bundle and writes its guest metadata.
///
/// # Safety
///
/// Each non-null output pointer must be properly aligned and valid for one
/// write of its pointed-to type. Null output pointers are rejected safely.
pub unsafe extern "C" fn krunfw_get_kernel(
    guest_addr: *mut u64,
    entry_addr: *mut u64,
    size: *mut usize,
) -> *const c_char {
    if guest_addr.is_null() || entry_addr.is_null() || size.is_null() {
        return ptr::null();
    }

    let Some(kernel) = prepare_kernel() else {
        return ptr::null();
    };
    unsafe {
        *guest_addr = kernel.guest_addr;
        *entry_addr = kernel.entry_addr;
        *size = kernel.size;
    }
    kernel.ptr as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SEGMENT: EmbeddedLoadSegment = EmbeddedLoadSegment {
        file_offset: 2,
        file_size: 4,
        destination_offset: 1,
    };

    #[test]
    fn prepares_only_build_validated_elf_copy_descriptors() {
        let prepared = prepare_validated_elf(
            &[9, 9, 1, 2, 3, 4],
            0x0100_0000,
            0x0100_0002,
            8,
            &[TEST_SEGMENT],
        )
        .unwrap();
        assert_eq!(prepared.guest_addr, 0x0100_0000);
        assert_eq!(prepared.entry_addr, 0x0100_0002);
        assert_eq!(prepared.size, 8);
        let flattened =
            unsafe { std::slice::from_raw_parts(prepared.ptr as *const u8, prepared.size) };
        assert_eq!(flattened, &[0, 1, 2, 3, 4, 0, 0, 0]);
    }

    #[test]
    fn rejects_invalid_generated_descriptor_without_panicking() {
        let invalid = EmbeddedLoadSegment {
            file_offset: usize::MAX,
            file_size: 2,
            destination_offset: 0,
        };
        assert!(prepare_validated_elf(&[1, 2], 0x0100_0000, 0x0100_0001, 2, &[invalid]).is_err());
    }

    #[test]
    fn rejects_unaligned_generated_guest_address_without_panicking() {
        assert_eq!(
            prepare_validated_elf(&[1, 2, 3, 4], 1, 3, 4, &[TEST_SEGMENT]).err(),
            Some("generated ELF guest load address is not 4096-byte aligned")
        );
        assert_eq!(
            prepare_validated_raw(&[1, 2, 3, 4], 1, 3).err(),
            Some("generated raw bundle guest load address is not 4096-byte aligned")
        );
    }

    #[test]
    fn prepares_validated_raw_bundle_without_interpreting_bytes() {
        let raw = [1_u8, 2, 3, 4];
        let prepared = prepare_validated_raw(&raw, 0x0100_0000, 0x0100_0123).unwrap();
        assert_eq!(prepared.guest_addr, 0x0100_0000);
        assert_eq!(prepared.entry_addr, 0x0100_0123);
        assert_eq!(prepared.size, raw.len());
        let copied = unsafe { std::slice::from_raw_parts(prepared.ptr as *const u8, raw.len()) };
        assert_eq!(copied, raw);
    }

    #[test]
    fn exported_bundle_uses_generated_build_time_metadata() {
        let (expected_guest, expected_entry, expected_size) = match KERNEL_SOURCE {
            EmbeddedKernelSource::Elf {
                guest_load_addr,
                entry_addr,
                image_size,
                ..
            } => (guest_load_addr, entry_addr, image_size),
            EmbeddedKernelSource::RawBundle {
                guest_load_addr,
                entry_addr,
            } => (guest_load_addr, entry_addr, KERNEL_IMAGE.len()),
        };

        let mut exported_guest = 0;
        let mut exported_entry = 0;
        let mut exported_size = 0;
        let pointer = unsafe {
            krunfw_get_kernel(&mut exported_guest, &mut exported_entry, &mut exported_size)
        };
        assert!(!pointer.is_null());
        assert_eq!(exported_guest, expected_guest);
        assert_eq!(exported_entry, expected_entry);
        assert_eq!(exported_size, expected_size);
    }

    #[test]
    fn exported_function_rejects_null_outputs() {
        let pointer =
            unsafe { krunfw_get_kernel(ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
        assert!(pointer.is_null());
    }
}
