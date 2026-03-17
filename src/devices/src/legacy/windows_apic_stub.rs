// Windows APIC/IOAPIC stub device
// This is a minimal stub that handles APIC/IOAPIC MMIO accesses without crashing.
// It returns safe default values to allow the kernel to boot even when APIC is accessed.

use std::sync::{Arc, Mutex};

use crate::bus::BusDevice;

const IOAPIC_BASE: u64 = 0xfec00000;
const IOAPIC_SIZE: u64 = 0x1000;
const LAPIC_BASE: u64 = 0xfee00000;
const LAPIC_SIZE: u64 = 0x1000;

/// Stub APIC device that handles MMIO reads/writes without crashing
#[derive(Debug)]
pub struct ApicStub {
    base: u64,
}

impl ApicStub {
    pub fn new(base: u64) -> Self {
        ApicStub { base }
    }

    pub fn ioapic() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new(IOAPIC_BASE)))
    }

    pub fn lapic() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new(LAPIC_BASE)))
    }

    pub fn ioapic_range() -> (u64, u64) {
        (IOAPIC_BASE, IOAPIC_SIZE)
    }

    pub fn lapic_range() -> (u64, u64) {
        (LAPIC_BASE, LAPIC_SIZE)
    }
}

impl BusDevice for ApicStub {
    fn read(&mut self, _base: u64, offset: u64, data: &mut [u8]) {
        // Return zeros for all reads
        // This is safe - the kernel will see the APIC as disabled/not present
        for byte in data.iter_mut() {
            *byte = 0;
        }
        log::debug!(
            "APIC stub read at base=0x{:x}, offset=0x{:x}, len={}",
            self.base,
            offset,
            data.len()
        );
    }

    fn write(&mut self, _base: u64, offset: u64, data: &[u8]) {
        // Silently ignore all writes
        // This prevents crashes when the kernel tries to configure APIC
        log::debug!(
            "APIC stub write at base=0x{:x}, offset=0x{:x}, len={}, data={:02x?}",
            self.base,
            offset,
            data.len(),
            data
        );
    }
}
