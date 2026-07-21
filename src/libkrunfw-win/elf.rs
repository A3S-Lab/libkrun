// Strict ELF64 validation shared by the libkrunfw Windows build and tests.

const ELF_HEADER_SIZE: usize = 64;
const ELF_PROGRAM_HEADER_SIZE: usize = 56;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u32 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const GUEST_LOAD_ALIGNMENT: u64 = 4096;

pub const MAX_ELF_GUEST_SPAN: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedLoadSegment {
    pub file_offset: usize,
    pub file_size: usize,
    pub destination_offset: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedElf {
    pub guest_load_addr: u64,
    pub entry_addr: u64,
    pub image_size: usize,
    pub segments: Vec<ValidatedLoadSegment>,
}

#[derive(Clone, Debug)]
struct LoadSegment {
    file_offset: usize,
    file_size: usize,
    mem_size: u64,
    guest_addr: u64,
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], String> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| "ELF field offset overflows usize".to_owned())?;
    bytes
        .get(offset..end)
        .ok_or_else(|| format!("ELF field at offset {offset} is truncated"))?
        .try_into()
        .map_err(|_| "ELF field has an unexpected size".to_owned())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

pub fn validate_elf(kernel: &[u8]) -> Result<ValidatedElf, String> {
    if kernel.len() < ELF_HEADER_SIZE {
        return Err("ELF image is shorter than its ELF64 header".to_owned());
    }
    if &kernel[0..4] != b"\x7fELF" {
        return Err("invalid ELF magic".to_owned());
    }
    if kernel[4] != ELFCLASS64 {
        return Err("kernel must use the ELF64 class".to_owned());
    }
    if kernel[5] != ELFDATA2LSB {
        return Err("kernel must use little-endian ELF encoding".to_owned());
    }
    if kernel[6] != EV_CURRENT as u8 || read_u32(kernel, 20)? != EV_CURRENT {
        return Err("kernel uses an unsupported ELF version".to_owned());
    }
    if read_u16(kernel, 16)? != ET_EXEC {
        return Err(
            "kernel ELF must be ET_EXEC; shared objects (ET_DYN) are not kernels".to_owned(),
        );
    }
    if read_u16(kernel, 18)? != EM_X86_64 {
        return Err("kernel ELF must target x86_64".to_owned());
    }
    if usize::from(read_u16(kernel, 52)?) != ELF_HEADER_SIZE {
        return Err("kernel ELF has an unexpected ELF header size".to_owned());
    }

    let raw_entry = read_u64(kernel, 24)?;
    if raw_entry == 0 {
        return Err("kernel ELF entry point is zero".to_owned());
    }
    let program_header_offset = usize::try_from(read_u64(kernel, 32)?)
        .map_err(|_| "ELF program header offset does not fit in usize".to_owned())?;
    let program_header_size = usize::from(read_u16(kernel, 54)?);
    let program_header_count = usize::from(read_u16(kernel, 56)?);
    if program_header_size != ELF_PROGRAM_HEADER_SIZE {
        return Err(format!(
            "kernel ELF program header size must be {ELF_PROGRAM_HEADER_SIZE} bytes"
        ));
    }
    if program_header_count == 0 {
        return Err("kernel ELF has no program headers".to_owned());
    }
    let table_size = program_header_count
        .checked_mul(program_header_size)
        .ok_or_else(|| "ELF program header table size overflows usize".to_owned())?;
    let table_end = program_header_offset
        .checked_add(table_size)
        .ok_or_else(|| "ELF program header table range overflows usize".to_owned())?;
    if table_end > kernel.len() {
        return Err("ELF program header table is truncated".to_owned());
    }

    let mut load_segments = Vec::new();
    let mut translated_entry = None;
    for index in 0..program_header_count {
        let offset = program_header_offset + index * program_header_size;
        if read_u32(kernel, offset)? != PT_LOAD {
            continue;
        }

        let flags = read_u32(kernel, offset + 4)?;
        let file_offset_u64 = read_u64(kernel, offset + 8)?;
        let virt_addr = read_u64(kernel, offset + 16)?;
        let guest_addr = read_u64(kernel, offset + 24)?;
        let file_size_u64 = read_u64(kernel, offset + 32)?;
        let mem_size = read_u64(kernel, offset + 40)?;
        let alignment = read_u64(kernel, offset + 48)?;

        if file_size_u64 > mem_size {
            return Err(format!(
                "PT_LOAD segment {index} file size exceeds its memory size"
            ));
        }
        if mem_size == 0 {
            continue;
        }
        if guest_addr == 0 {
            return Err(format!("PT_LOAD segment {index} has a zero guest address"));
        }
        if alignment > 1 {
            if !alignment.is_power_of_two() {
                return Err(format!(
                    "PT_LOAD segment {index} alignment is not a power of two"
                ));
            }
            if virt_addr % alignment != file_offset_u64 % alignment {
                return Err(format!(
                    "PT_LOAD segment {index} violates ELF offset/address alignment"
                ));
            }
        }

        let file_offset = usize::try_from(file_offset_u64)
            .map_err(|_| format!("PT_LOAD segment {index} offset does not fit in usize"))?;
        let file_size = usize::try_from(file_size_u64)
            .map_err(|_| format!("PT_LOAD segment {index} size does not fit in usize"))?;
        let file_end = file_offset
            .checked_add(file_size)
            .ok_or_else(|| format!("PT_LOAD segment {index} file range overflows usize"))?;
        if file_end > kernel.len() {
            return Err(format!("PT_LOAD segment {index} exceeds the ELF image"));
        }
        let guest_end = guest_addr
            .checked_add(mem_size)
            .ok_or_else(|| format!("PT_LOAD segment {index} guest range overflows u64"))?;
        let virt_end = virt_addr
            .checked_add(mem_size)
            .ok_or_else(|| format!("PT_LOAD segment {index} virtual range overflows u64"))?;

        if flags & PF_X != 0 && raw_entry >= virt_addr && raw_entry < virt_end {
            let entry_offset = raw_entry - virt_addr;
            if entry_offset >= file_size_u64 {
                return Err(format!(
                    "kernel ELF entry point falls in non-file-backed memory of executable PT_LOAD segment {index}"
                ));
            }
            let candidate = guest_addr
                .checked_add(entry_offset)
                .ok_or_else(|| "translated ELF entry point overflows u64".to_owned())?;
            if candidate >= guest_end {
                return Err("translated ELF entry point is outside its PT_LOAD segment".to_owned());
            }
            if translated_entry.replace(candidate).is_some() {
                return Err(
                    "kernel ELF entry point maps through multiple executable PT_LOAD segments"
                        .to_owned(),
                );
            }
        }

        load_segments.push(LoadSegment {
            file_offset,
            file_size,
            mem_size,
            guest_addr,
        });
    }

    if load_segments.is_empty() {
        return Err("kernel ELF contains no non-empty PT_LOAD segments".to_owned());
    }
    let entry_addr = translated_entry.ok_or_else(|| {
        "kernel ELF entry point is not in a file-backed executable PT_LOAD segment".to_owned()
    })?;

    let guest_load_addr = load_segments
        .iter()
        .map(|segment| segment.guest_addr)
        .min()
        .ok_or_else(|| "kernel ELF has no guest load address".to_owned())?;
    if guest_load_addr % GUEST_LOAD_ALIGNMENT != 0 {
        return Err(format!(
            "kernel ELF guest load address must be {GUEST_LOAD_ALIGNMENT}-byte aligned"
        ));
    }
    let guest_end = load_segments
        .iter()
        .map(|segment| {
            segment
                .guest_addr
                .checked_add(segment.mem_size)
                .ok_or_else(|| "PT_LOAD guest range overflows u64".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .ok_or_else(|| "kernel ELF has no guest end address".to_owned())?;
    let guest_span = guest_end - guest_load_addr;
    if guest_span == 0 || guest_span > MAX_ELF_GUEST_SPAN {
        return Err(format!(
            "kernel ELF guest span {guest_span} exceeds the {MAX_ELF_GUEST_SPAN} byte limit"
        ));
    }
    let image_size = usize::try_from(guest_span)
        .map_err(|_| "kernel ELF guest span does not fit in usize".to_owned())?;

    let mut ranges: Vec<_> = load_segments
        .iter()
        .map(|segment| {
            let end = segment.guest_addr + segment.mem_size;
            (segment.guest_addr, end)
        })
        .collect();
    ranges.sort_unstable_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err("kernel ELF has overlapping PT_LOAD guest ranges".to_owned());
        }
    }

    let mut segments = Vec::with_capacity(load_segments.len());
    for segment in load_segments {
        let destination_offset = usize::try_from(segment.guest_addr - guest_load_addr)
            .map_err(|_| "PT_LOAD destination offset does not fit in usize".to_owned())?;
        let destination_end = destination_offset
            .checked_add(segment.file_size)
            .ok_or_else(|| "PT_LOAD destination range overflows usize".to_owned())?;
        if destination_end > image_size {
            return Err("PT_LOAD destination exceeds the flattened guest image".to_owned());
        }
        segments.push(ValidatedLoadSegment {
            file_offset: segment.file_offset,
            file_size: segment.file_size,
            destination_offset,
        });
    }

    Ok(ValidatedElf {
        guest_load_addr,
        entry_addr,
        image_size,
        segments,
    })
}

#[cfg(test)]
pub fn valid_test_elf() -> Vec<u8> {
    const PROGRAM_HEADER_OFFSET: usize = ELF_HEADER_SIZE;
    const DATA_OFFSET: usize = 256;
    const VIRTUAL_ADDRESS: u64 = 0xffff_ffff_8100_0000;
    const GUEST_ADDRESS: u64 = 0x0100_0000;

    let mut elf = vec![0_u8; DATA_OFFSET + 4];
    elf[0..4].copy_from_slice(b"\x7fELF");
    elf[4] = ELFCLASS64;
    elf[5] = ELFDATA2LSB;
    elf[6] = EV_CURRENT as u8;
    elf[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
    elf[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
    elf[20..24].copy_from_slice(&EV_CURRENT.to_le_bytes());
    elf[24..32].copy_from_slice(&(VIRTUAL_ADDRESS + 2).to_le_bytes());
    elf[32..40].copy_from_slice(&(PROGRAM_HEADER_OFFSET as u64).to_le_bytes());
    elf[52..54].copy_from_slice(&(ELF_HEADER_SIZE as u16).to_le_bytes());
    elf[54..56].copy_from_slice(&(ELF_PROGRAM_HEADER_SIZE as u16).to_le_bytes());
    elf[56..58].copy_from_slice(&1_u16.to_le_bytes());

    let header = PROGRAM_HEADER_OFFSET;
    elf[header..header + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
    elf[header + 4..header + 8].copy_from_slice(&5_u32.to_le_bytes());
    elf[header + 8..header + 16].copy_from_slice(&(DATA_OFFSET as u64).to_le_bytes());
    elf[header + 16..header + 24].copy_from_slice(&VIRTUAL_ADDRESS.to_le_bytes());
    elf[header + 24..header + 32].copy_from_slice(&GUEST_ADDRESS.to_le_bytes());
    elf[header + 32..header + 40].copy_from_slice(&4_u64.to_le_bytes());
    elf[header + 40..header + 48].copy_from_slice(&8_u64.to_le_bytes());
    elf[header + 48..header + 56].copy_from_slice(&1_u64.to_le_bytes());
    elf[DATA_OFFSET..].copy_from_slice(&[1, 2, 3, 4]);
    elf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_et_exec_and_translates_executable_entry() {
        let validated = validate_elf(&valid_test_elf()).unwrap();
        assert_eq!(validated.guest_load_addr, 0x0100_0000);
        assert_eq!(validated.entry_addr, 0x0100_0002);
        assert_eq!(validated.image_size, 8);
        assert_eq!(validated.segments.len(), 1);
        assert_eq!(validated.segments[0].file_offset, 256);
        assert_eq!(validated.segments[0].file_size, 4);
        assert_eq!(validated.segments[0].destination_offset, 0);
    }

    #[test]
    fn rejects_et_dyn_shared_object() {
        let mut elf = valid_test_elf();
        elf[16..18].copy_from_slice(&3_u16.to_le_bytes());
        let error = validate_elf(&elf).unwrap_err();
        assert!(error.contains("ET_EXEC"));
        assert!(error.contains("ET_DYN"));
    }

    #[test]
    fn rejects_truncated_and_overflowing_program_header_tables() {
        let mut truncated = valid_test_elf();
        truncated.truncate(80);
        assert!(validate_elf(&truncated)
            .unwrap_err()
            .contains("program header table is truncated"));

        let mut overflowing = valid_test_elf();
        overflowing[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        let error = validate_elf(&overflowing).unwrap_err();
        assert!(error.contains("program header") && error.contains("overflow"));
    }

    #[test]
    fn rejects_entry_outside_executable_file_backed_segment() {
        let mut not_executable = valid_test_elf();
        not_executable[68..72].copy_from_slice(&4_u32.to_le_bytes());
        assert!(validate_elf(&not_executable)
            .unwrap_err()
            .contains("executable PT_LOAD"));

        let mut in_bss = valid_test_elf();
        let entry = 0xffff_ffff_8100_0006_u64;
        in_bss[24..32].copy_from_slice(&entry.to_le_bytes());
        assert!(validate_elf(&in_bss)
            .unwrap_err()
            .contains("non-file-backed memory"));
    }

    #[test]
    fn rejects_unaligned_final_guest_load_address() {
        let mut unaligned = valid_test_elf();
        unaligned[88..96].copy_from_slice(&1_u64.to_le_bytes());
        let error = validate_elf(&unaligned).unwrap_err();
        assert!(error.contains("guest load address"));
        assert!(error.contains("4096-byte aligned"));
    }

    #[test]
    fn rejects_excessive_guest_span() {
        let mut elf = valid_test_elf();
        let excessive = MAX_ELF_GUEST_SPAN + 4096;
        elf[104..112].copy_from_slice(&excessive.to_le_bytes());
        assert!(validate_elf(&elf).unwrap_err().contains("guest span"));
    }
}
