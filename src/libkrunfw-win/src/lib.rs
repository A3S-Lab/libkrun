// libkrunfw.dll — Windows companion for libkrun
//
// Bundles a pre-built x86_64 Linux kernel as an ELF `vmlinux` and exports
// krunfw_get_kernel() so libkrun can discover and load it automatically
// without the caller needing to call krun_set_kernel().
//
// libkrun's kernel-bundle ABI expects a single contiguous, page-aligned host
// buffer laid out exactly as guest physical memory should look. The bundled
// kernel here is a normal ELF file, so on first use we parse its PT_LOAD
// segments, flatten them into one contiguous guest-physical image, and keep
// that prepared buffer alive for the lifetime of the process.

use std::alloc::{alloc_zeroed, Layout};
use std::ffi::c_char;
use std::sync::OnceLock;

static KERNEL_ELF: &[u8] = include_bytes!("../kernel/vmlinux");

const PT_LOAD: u32 = 1;

struct LoadSegment {
    file_offset: usize,
    file_size: usize,
    mem_size: usize,
    guest_addr: u64,
    virt_addr: u64,
}

struct PreparedKernel {
    guest_addr: u64,
    entry_addr: u64,
    size: usize,
    ptr: usize,
}

static PREPARED_KERNEL: OnceLock<PreparedKernel> = OnceLock::new();

fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
    let end = offset + 2;
    u16::from_le_bytes(bytes[offset..end].try_into().expect("short ELF u16"))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    let end = offset + 4;
    u32::from_le_bytes(bytes[offset..end].try_into().expect("short ELF u32"))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> u64 {
    let end = offset + 8;
    u64::from_le_bytes(bytes[offset..end].try_into().expect("short ELF u64"))
}

fn translate_entry_addr(seg: &LoadSegment, raw_entry: u64) -> Option<u64> {
    let virt_end = seg.virt_addr + seg.mem_size as u64;
    if seg.virt_addr != 0 && raw_entry >= seg.virt_addr && raw_entry < virt_end {
        Some(seg.guest_addr + (raw_entry - seg.virt_addr))
    } else {
        None
    }
}

fn parse_load_segments() -> (Vec<LoadSegment>, u64) {
    assert!(KERNEL_ELF.len() >= 64, "libkrunfw: ELF image too small");
    assert_eq!(
        &KERNEL_ELF[0..4],
        b"\x7FELF",
        "libkrunfw: invalid ELF magic"
    );
    assert_eq!(KERNEL_ELF[4], 2, "libkrunfw: expected ELF64 kernel");
    assert_eq!(KERNEL_ELF[5], 1, "libkrunfw: expected little-endian kernel");

    let entry = read_u64_le(KERNEL_ELF, 24);
    let phoff = read_u64_le(KERNEL_ELF, 32) as usize;
    let phentsize = read_u16_le(KERNEL_ELF, 54) as usize;
    let phnum = read_u16_le(KERNEL_ELF, 56) as usize;

    assert!(
        phentsize >= 56,
        "libkrunfw: unexpected ELF program header size"
    );

    let mut segments = Vec::new();
    for idx in 0..phnum {
        let off = phoff + idx * phentsize;
        let p_type = read_u32_le(KERNEL_ELF, off);
        if p_type != PT_LOAD {
            continue;
        }

        let file_offset = read_u64_le(KERNEL_ELF, off + 8) as usize;
        let virt_addr = read_u64_le(KERNEL_ELF, off + 16);
        let guest_addr = read_u64_le(KERNEL_ELF, off + 24);
        let file_size = read_u64_le(KERNEL_ELF, off + 32) as usize;
        let mem_size = read_u64_le(KERNEL_ELF, off + 40) as usize;

        if mem_size == 0 {
            continue;
        }

        assert!(
            file_offset + file_size <= KERNEL_ELF.len(),
            "libkrunfw: PT_LOAD segment exceeds ELF image"
        );

        segments.push(LoadSegment {
            file_offset,
            file_size,
            mem_size,
            guest_addr,
            virt_addr,
        });
    }

    assert!(
        !segments.is_empty(),
        "libkrunfw: ELF image does not contain any PT_LOAD segments"
    );

    (segments, entry)
}

fn prepare_kernel() -> &'static PreparedKernel {
    PREPARED_KERNEL.get_or_init(|| {
        let (segments, raw_entry) = parse_load_segments();

        let min_guest = segments
            .iter()
            .map(|seg| seg.guest_addr)
            .min()
            .expect("libkrunfw: missing PT_LOAD guest start");
        let max_guest = segments
            .iter()
            .map(|seg| seg.guest_addr + seg.mem_size as u64)
            .max()
            .expect("libkrunfw: missing PT_LOAD guest end");
        let image_size = (max_guest - min_guest) as usize;

        let layout = Layout::from_size_align(image_size, 4096)
            .expect("libkrunfw: failed to build allocation layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "libkrunfw: page-aligned allocation failed");

        for seg in &segments {
            let dst = unsafe { ptr.add((seg.guest_addr - min_guest) as usize) };
            let src = &KERNEL_ELF[seg.file_offset..seg.file_offset + seg.file_size];
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
            }
        }

        let entry_addr = segments
            .iter()
            .find_map(|seg| translate_entry_addr(seg, raw_entry))
            .unwrap_or(raw_entry);

        PreparedKernel {
            guest_addr: min_guest,
            entry_addr,
            size: image_size,
            ptr: ptr as usize,
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn krunfw_get_kernel(
    guest_addr: *mut u64,
    entry_addr: *mut u64,
    size: *mut usize,
) -> *const c_char {
    let kernel = prepare_kernel();
    *guest_addr = kernel.guest_addr;
    *entry_addr = kernel.entry_addr;
    *size = kernel.size;
    kernel.ptr as *const c_char
}

#[cfg(test)]
mod tests {
    use super::{translate_entry_addr, LoadSegment};

    #[test]
    fn entry_translation_is_lazy_for_non_matching_segments() {
        let seg = LoadSegment {
            file_offset: 0,
            file_size: 0,
            mem_size: 0x1000,
            guest_addr: 0x0010_0000,
            virt_addr: 0xffff_ffff_8100_0000,
        };

        assert_eq!(translate_entry_addr(&seg, 0x0010_0000), None);
    }
}
