use vm_memory::Bytes;
use vm_memory::{GuestAddress, GuestMemoryError, GuestMemoryMmap};

const RSDP_ADDR: u64 = 0x000e_0000;
const RSDT_ADDR: u64 = 0x000e_0040;
const MADT_ADDR: u64 = 0x000e_0080;

const MADT_LOCAL_APIC_ADDR: u32 = 0xfee0_0000;
const MADT_IO_APIC_ADDR: u32 = 0xfec0_0000;
const MADT_PCAT_COMPAT: u32 = 1;

const OEM_ID: [u8; 6] = *b"LIBKRN";
const OEM_TABLE_ID: [u8; 8] = *b"LIBKRUN ";
const CREATOR_ID: [u8; 4] = *b"KRUN";

fn acpi_checksum(bytes: &[u8]) -> u8 {
    (0u8).wrapping_sub(bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte)))
}

fn append_u32_le(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn append_acpi_header(bytes: &mut Vec<u8>, signature: &[u8; 4], length: u32) {
    bytes.extend_from_slice(signature);
    append_u32_le(bytes, length);
    bytes.push(1);
    bytes.push(0);
    bytes.extend_from_slice(&OEM_ID);
    bytes.extend_from_slice(&OEM_TABLE_ID);
    append_u32_le(bytes, 1);
    bytes.extend_from_slice(&CREATOR_ID);
    append_u32_le(bytes, 1);
}

fn finalize_acpi_table(bytes: &mut [u8]) {
    bytes[9] = acpi_checksum(bytes);
}

fn build_rsdp(rsdt_addr: u32) -> [u8; 20] {
    let mut rsdp = [0u8; 20];
    rsdp[..8].copy_from_slice(b"RSD PTR ");
    rsdp[9..15].copy_from_slice(&OEM_ID);
    rsdp[15] = 0;
    rsdp[16..20].copy_from_slice(&rsdt_addr.to_le_bytes());
    rsdp[8] = acpi_checksum(&rsdp);
    rsdp
}

fn build_rsdt(madt_addr: u32) -> Vec<u8> {
    let length = 36 + 4;
    let mut rsdt = Vec::with_capacity(length as usize);
    append_acpi_header(&mut rsdt, b"RSDT", length);
    append_u32_le(&mut rsdt, madt_addr);
    finalize_acpi_table(&mut rsdt);
    rsdt
}

fn build_madt(num_cpus: u8) -> Vec<u8> {
    let lapic_entries_len = usize::from(num_cpus) * 8;
    let ioapic_entry_len = 12usize;
    let payload_len = 8usize + lapic_entries_len + ioapic_entry_len;
    let length = 36 + payload_len as u32;
    let mut madt = Vec::with_capacity(length as usize);

    append_acpi_header(&mut madt, b"APIC", length);
    append_u32_le(&mut madt, MADT_LOCAL_APIC_ADDR);
    append_u32_le(&mut madt, MADT_PCAT_COMPAT);

    for cpu_id in 0..num_cpus {
        madt.push(0);
        madt.push(8);
        madt.push(cpu_id);
        madt.push(cpu_id);
        append_u32_le(&mut madt, 1);
    }

    madt.push(1);
    madt.push(12);
    madt.push(num_cpus.saturating_add(1));
    madt.push(0);
    append_u32_le(&mut madt, MADT_IO_APIC_ADDR);
    append_u32_le(&mut madt, 0);

    finalize_acpi_table(&mut madt);
    madt
}

pub(crate) fn install_minimal_acpi_tables(
    guest_mem: &GuestMemoryMmap,
    num_cpus: u8,
) -> Result<(), GuestMemoryError> {
    let rsdp = build_rsdp(RSDT_ADDR as u32);
    let rsdt = build_rsdt(MADT_ADDR as u32);
    let madt = build_madt(num_cpus);

    guest_mem.write_slice(&rsdp, GuestAddress(RSDP_ADDR))?;
    guest_mem.write_slice(&rsdt, GuestAddress(RSDT_ADDR))?;
    guest_mem.write_slice(&madt, GuestAddress(MADT_ADDR))?;

    Ok(())
}

pub(crate) fn table_addresses() -> (u64, u64, u64) {
    (RSDP_ADDR, RSDT_ADDR, MADT_ADDR)
}
