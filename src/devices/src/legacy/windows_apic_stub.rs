// Windows APIC/IOAPIC stub device
// This is a minimal stub that handles APIC/IOAPIC MMIO accesses without crashing.
// It returns safe default values to allow the kernel to boot even when APIC is accessed.

use std::sync::{Arc, Mutex};

use crate::bus::BusDevice;

const IOAPIC_BASE: u64 = 0xfec00000;
const IOAPIC_SIZE: u64 = 0x1000;
const LAPIC_BASE: u64 = 0xfee00000;
const LAPIC_SIZE: u64 = 0x1000;

// LAPIC register offsets
const LAPIC_ID: u64 = 0x20;
const LAPIC_VERSION: u64 = 0x30;
const LAPIC_TPR: u64 = 0x80;
const LAPIC_EOI: u64 = 0xB0;
const LAPIC_SPURIOUS: u64 = 0xF0;
const LAPIC_ISR_BASE: u64 = 0x100;  // In-Service Register (8 registers, 0x100-0x170)
const LAPIC_TMR_BASE: u64 = 0x180;  // Trigger Mode Register (8 registers)
const LAPIC_IRR_BASE: u64 = 0x200;  // Interrupt Request Register (8 registers)
const LAPIC_ESR: u64 = 0x280;       // Error Status Register
const LAPIC_ICR_LOW: u64 = 0x300;   // Interrupt Command Register (low)
const LAPIC_ICR_HIGH: u64 = 0x310;  // Interrupt Command Register (high)
const LAPIC_TIMER_LVT: u64 = 0x320; // Timer Local Vector Table
const LAPIC_TIMER_INITIAL: u64 = 0x380;
const LAPIC_TIMER_CURRENT: u64 = 0x390;
const LAPIC_TIMER_DIVIDE: u64 = 0x3E0;

// IOAPIC register offsets
const IOREGSEL: u64 = 0x00;  // Register Select (index)
const IOWIN: u64 = 0x10;     // Data Window (read/write selected register)

// IOAPIC register indices (written to IOREGSEL)
const IOAPIC_ID: u8 = 0x00;
const IOAPIC_VER: u8 = 0x01;
const IOAPIC_ARB: u8 = 0x02;
const IOAPIC_REDTBL_BASE: u8 = 0x10;  // Redirection table starts at 0x10

/// Stub APIC device that handles MMIO reads/writes without crashing
#[derive(Debug)]
pub struct ApicStub {
    base: u64,
    // LAPIC state
    spurious_vector: u32,
    tpr: u32,
    // IOAPIC state
    ioregsel: u8,  // Currently selected register index
}

impl ApicStub {
    pub fn new(base: u64) -> Self {
        ApicStub {
            base,
            spurious_vector: 0xFF,  // Default spurious vector
            tpr: 0,
            ioregsel: 0,  // Default to register 0
        }
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

    fn read_lapic_register(&self, offset: u64) -> u32 {
        match offset {
            LAPIC_ID => {
                // Return APIC ID 0 for BSP (Bootstrap Processor)
                0x00000000
            }
            LAPIC_VERSION => {
                // Version 0x14 (Pentium 4/Xeon), 24 LVT entries
                // Bit 0-7: Version (0x14)
                // Bit 16-23: Max LVT entry (0x05 = 6 entries)
                0x00050014
            }
            LAPIC_TPR => {
                // Task Priority Register
                self.tpr
            }
            LAPIC_EOI => {
                // EOI register - write-only, return 0
                0
            }
            LAPIC_SPURIOUS => {
                // Spurious Interrupt Vector Register
                // Bit 0-7: Spurious Vector (0xFF)
                // Bit 8: APIC Software Enable (1 = enabled)
                // Bit 9: Focus Processor Checking (0 = enabled)
                self.spurious_vector | 0x100  // APIC enabled
            }
            LAPIC_ISR_BASE..=0x170 => {
                // In-Service Register - all zeros (no interrupts in service)
                0
            }
            LAPIC_TMR_BASE..=0x1F0 => {
                // Trigger Mode Register - all zeros (edge-triggered)
                0
            }
            LAPIC_IRR_BASE..=0x270 => {
                // Interrupt Request Register - all zeros (no pending interrupts)
                0
            }
            LAPIC_ESR => {
                // Error Status Register - no errors
                0
            }
            LAPIC_ICR_LOW | LAPIC_ICR_HIGH => {
                // Interrupt Command Register - idle (bit 12 = 0)
                0
            }
            LAPIC_TIMER_LVT => {
                // Timer LVT - masked
                0x00010000  // Bit 16 = masked
            }
            LAPIC_TIMER_INITIAL => {
                // Timer Initial Count - 0 (timer not running)
                0
            }
            LAPIC_TIMER_CURRENT => {
                // Timer Current Count - 0
                0
            }
            LAPIC_TIMER_DIVIDE => {
                // Timer Divide Configuration - divide by 1
                0x0000000B
            }
            _ => {
                // Unknown register - return 0
                0
            }
        }
    }

    fn read_ioapic_register(&self, offset: u64) -> u32 {
        match offset {
            IOREGSEL => {
                // Return currently selected register index
                self.ioregsel as u32
            }
            IOWIN => {
                // Read from the register selected by IOREGSEL
                match self.ioregsel {
                    IOAPIC_ID => {
                        // IOAPIC ID register
                        // Bits 24-27: APIC ID (0 for single IOAPIC)
                        0x00000000
                    }
                    IOAPIC_VER => {
                        // IOAPIC Version register
                        // Bits 0-7: Version (0x20 = 82093AA)
                        // Bits 16-23: Maximum Redirection Entry (23 = 24 entries, 0-23)
                        0x00170020  // Version 0x20, 24 entries (0x17 = 23)
                    }
                    IOAPIC_ARB => {
                        // IOAPIC Arbitration ID (read-only, same as ID)
                        0x00000000
                    }
                    IOAPIC_REDTBL_BASE..=0x3F => {
                        // Redirection Table entries (0x10-0x3F = 24 entries * 2 registers each)
                        // Each entry is 64 bits, accessed as two 32-bit registers
                        // Return masked (bit 16 = 1) to indicate interrupt is disabled
                        0x00010000  // Masked
                    }
                    _ => {
                        // Unknown register index - return all 1s to indicate invalid/non-existent
                        log::warn!("IOAPIC: read from unknown register index 0x{:02x}, returning 0xFFFFFFFF", self.ioregsel);
                        0xFFFFFFFF
                    }
                }
            }
            _ => {
                log::warn!("IOAPIC: read from unknown offset 0x{:x}", offset);
                0
            }
        }
    }
}

impl BusDevice for ApicStub {
    fn read(&mut self, _base: u64, offset: u64, data: &mut [u8]) {
        let value = if self.base == LAPIC_BASE {
            // LAPIC read
            self.read_lapic_register(offset)
        } else {
            // IOAPIC read
            self.read_ioapic_register(offset)
        };

        // Write the value to the data buffer (little-endian)
        let bytes_to_copy = data.len().min(4);
        for i in 0..bytes_to_copy {
            data[i] = ((value >> (i * 8)) & 0xFF) as u8;
        }

        if self.base == LAPIC_BASE {
            log::trace!(
                "LAPIC read at offset=0x{:x}, len={}, value=0x{:08x}",
                offset,
                data.len(),
                value
            );
        } else {
            log::info!(
                "📥 IOAPIC read at offset=0x{:x}, len={}, value=0x{:08x} (ioregsel=0x{:02x})",
                offset,
                data.len(),
                value,
                self.ioregsel
            );
        }
    }

    fn write(&mut self, _base: u64, offset: u64, data: &[u8]) {
        // Parse the written value (little-endian)
        let mut value: u32 = 0;
        for (i, &byte) in data.iter().enumerate().take(4) {
            value |= (byte as u32) << (i * 8);
        }

        if self.base == LAPIC_BASE {
            // LAPIC write
            match offset {
                LAPIC_TPR => {
                    self.tpr = value & 0xFF;
                    log::debug!("LAPIC TPR write: 0x{:02x}", self.tpr);
                }
                LAPIC_EOI => {
                    log::debug!("LAPIC EOI write (interrupt acknowledged)");
                }
                LAPIC_SPURIOUS => {
                    self.spurious_vector = value;
                    let enabled = (value & 0x100) != 0;
                    log::debug!(
                        "LAPIC Spurious Vector write: 0x{:08x} (APIC {})",
                        value,
                        if enabled { "enabled" } else { "disabled" }
                    );
                }
                LAPIC_ICR_LOW | LAPIC_ICR_HIGH => {
                    log::debug!("LAPIC ICR write at offset=0x{:x}: 0x{:08x}", offset, value);
                }
                LAPIC_TIMER_INITIAL => {
                    log::debug!("LAPIC Timer Initial Count write: 0x{:08x}", value);
                }
                LAPIC_TIMER_DIVIDE => {
                    log::debug!("LAPIC Timer Divide Config write: 0x{:08x}", value);
                }
                _ => {
                    log::trace!(
                        "LAPIC write at offset=0x{:x}, len={}, value=0x{:08x}",
                        offset,
                        data.len(),
                        value
                    );
                }
            }
        } else {
            // IOAPIC write
            match offset {
                IOREGSEL => {
                    // Write to register select
                    self.ioregsel = (value & 0xFF) as u8;
                    log::info!(
                        "📤 IOAPIC IOREGSEL write: 0x{:02x} (selecting register)",
                        self.ioregsel
                    );
                }
                IOWIN => {
                    // Write to the register selected by IOREGSEL
                    log::info!(
                        "📤 IOAPIC IOWIN write: reg=0x{:02x}, value=0x{:08x}",
                        self.ioregsel,
                        value
                    );
                    // For now, just log the write. In a full implementation,
                    // we would store redirection table entries, etc.
                }
                _ => {
                    log::warn!(
                        "IOAPIC write at unknown offset=0x{:x}, len={}, value=0x{:08x}",
                        offset,
                        data.len(),
                        value
                    );
                }
            }
        }
    }
}
