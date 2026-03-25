use std::sync::{Arc, Mutex, OnceLock};

use crate::bus::BusDevice;

fn windows_apic_debug_log(message: impl AsRef<str>) {
    static VALUE: OnceLock<bool> = OnceLock::new();
    if !*VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WINDOWS_VERBOSE_DEBUG")
            .or_else(|_| std::env::var("LIBKRUN_WINDOWS_IO_DEBUG"))
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }) {
        return;
    }
    use std::io::Write;

    for path in [r"C:\Users\18770\.a3s\libkrun-whpx-io-current.log", "tmp_whpx_io.log"] {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", message.as_ref());
        }
    }
}

const IOAPIC_BASE: u64 = 0xfec0_0000;
const IOAPIC_SIZE: u64 = 0x1000;
const LAPIC_BASE: u64 = 0xfee0_0000;
const LAPIC_SIZE: u64 = 0x1000;
const IOAPIC_NUM_PINS: usize = 24;

const LAPIC_ID: u64 = 0x20;
const LAPIC_VERSION: u64 = 0x30;
const LAPIC_TPR: u64 = 0x80;
const LAPIC_EOI: u64 = 0xB0;
const LAPIC_SPURIOUS: u64 = 0xF0;
const LAPIC_ISR_BASE: u64 = 0x100;
const LAPIC_TMR_BASE: u64 = 0x180;
const LAPIC_IRR_BASE: u64 = 0x200;
const LAPIC_ESR: u64 = 0x280;
const LAPIC_ICR_LOW: u64 = 0x300;
const LAPIC_ICR_HIGH: u64 = 0x310;
const LAPIC_TIMER_LVT: u64 = 0x320;
const LAPIC_TIMER_INITIAL: u64 = 0x380;
const LAPIC_TIMER_CURRENT: u64 = 0x390;
const LAPIC_TIMER_DIVIDE: u64 = 0x3e0;

const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;

const IOAPIC_ID: u8 = 0x00;
const IOAPIC_VER: u8 = 0x01;
const IOAPIC_ARB: u8 = 0x02;
const IOAPIC_REDTBL_BASE: u8 = 0x10;

const IOAPIC_LVT_DELIV_MODE_SHIFT: u64 = 8;
const IOAPIC_LVT_DEST_MODE_SHIFT: u64 = 11;
const IOAPIC_LVT_TRIGGER_MODE_SHIFT: u64 = 15;
const IOAPIC_LVT_MASKED_SHIFT: u64 = 16;
const IOAPIC_LVT_DEST_IDX_SHIFT: u64 = 56;
const IOAPIC_DM_MASK: u64 = 0x7;
const IOAPIC_VECTOR_MASK: u64 = 0xff;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoApicRoute {
    pub vector: u8,
    pub delivery_mode: u8,
    pub destination_mode_logical: bool,
    pub trigger_mode_level: bool,
    pub masked: bool,
    pub destination: u8,
}

#[derive(Debug)]
struct IoApicState {
    redirection_table: [u64; IOAPIC_NUM_PINS],
}

impl Default for IoApicState {
    fn default() -> Self {
        Self {
            redirection_table: [1 << IOAPIC_LVT_MASKED_SHIFT; IOAPIC_NUM_PINS],
        }
    }
}

fn ioapic_state() -> &'static Mutex<IoApicState> {
    static STATE: OnceLock<Mutex<IoApicState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(IoApicState::default()))
}

fn decode_route(entry: u64) -> IoApicRoute {
    IoApicRoute {
        vector: (entry & IOAPIC_VECTOR_MASK) as u8,
        delivery_mode: ((entry >> IOAPIC_LVT_DELIV_MODE_SHIFT) & IOAPIC_DM_MASK) as u8,
        destination_mode_logical: ((entry >> IOAPIC_LVT_DEST_MODE_SHIFT) & 1) != 0,
        trigger_mode_level: ((entry >> IOAPIC_LVT_TRIGGER_MODE_SHIFT) & 1) != 0,
        masked: ((entry >> IOAPIC_LVT_MASKED_SHIFT) & 1) != 0,
        destination: ((entry >> IOAPIC_LVT_DEST_IDX_SHIFT) & 0xff) as u8,
    }
}

pub fn query_route(irq: u32) -> Option<IoApicRoute> {
    let index = usize::try_from(irq).ok()?;
    let state = ioapic_state().lock().ok()?;
    state
        .redirection_table
        .get(index)
        .copied()
        .map(decode_route)
}

#[derive(Debug)]
pub struct ApicStub {
    base: u64,
    spurious_vector: u32,
    tpr: u32,
    ioregsel: u8,
}

impl ApicStub {
    pub fn new(base: u64) -> Self {
        Self {
            base,
            spurious_vector: 0xff,
            tpr: 0,
            ioregsel: 0,
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
            LAPIC_ID => 0x0000_0000,
            LAPIC_VERSION => 0x0005_0014,
            LAPIC_TPR => self.tpr,
            LAPIC_EOI => 0,
            LAPIC_SPURIOUS => self.spurious_vector | 0x100,
            LAPIC_ISR_BASE..=0x170 => 0,
            LAPIC_TMR_BASE..=0x1f0 => 0,
            LAPIC_IRR_BASE..=0x270 => 0,
            LAPIC_ESR => 0,
            LAPIC_ICR_LOW | LAPIC_ICR_HIGH => 0,
            LAPIC_TIMER_LVT => 0x0001_0000,
            LAPIC_TIMER_INITIAL => 0,
            LAPIC_TIMER_CURRENT => 0,
            LAPIC_TIMER_DIVIDE => 0x0000_000b,
            _ => 0,
        }
    }

    fn read_ioapic_redirection_register(&self) -> u32 {
        let register_index = usize::from(self.ioregsel - IOAPIC_REDTBL_BASE);
        let pin = register_index / 2;
        let high = (register_index & 1) != 0;
        let state = ioapic_state().lock().unwrap();
        let entry = state.redirection_table[pin];
        if high {
            (entry >> 32) as u32
        } else {
            entry as u32
        }
    }

    fn write_ioapic_redirection_register(&self, value: u32) {
        let register_index = usize::from(self.ioregsel - IOAPIC_REDTBL_BASE);
        let pin = register_index / 2;
        let high = (register_index & 1) != 0;
        let mut state = ioapic_state().lock().unwrap();
        let entry = &mut state.redirection_table[pin];

        if high {
            *entry = (*entry & 0x0000_0000_ffff_ffff) | ((value as u64) << 32);
        } else {
            *entry = (*entry & 0xffff_ffff_0000_0000) | u64::from(value);
        }

        let route = decode_route(*entry);
        log::debug!(
            "IOAPIC route irq={} reg=0x{:02x} value=0x{:08x} -> vector=0x{:02x} delivery={} dest_mode={} trigger={} masked={} dest=0x{:02x}",
            pin,
            self.ioregsel,
            value,
            route.vector,
            route.delivery_mode,
            if route.destination_mode_logical {
                "logical"
            } else {
                "physical"
            },
            if route.trigger_mode_level {
                "level"
            } else {
                "edge"
            },
            route.masked,
            route.destination,
        );
        windows_apic_debug_log(format!(
            "[IOAPIC] irq={} reg=0x{:02x} value=0x{:08x} vector=0x{:02x} delivery={} dest_mode={} trigger={} masked={} dest=0x{:02x}",
            pin,
            self.ioregsel,
            value,
            route.vector,
            route.delivery_mode,
            if route.destination_mode_logical {
                "logical"
            } else {
                "physical"
            },
            if route.trigger_mode_level {
                "level"
            } else {
                "edge"
            },
            route.masked,
            route.destination,
        ));
    }

    fn read_ioapic_register(&self, offset: u64) -> u32 {
        match offset {
            IOREGSEL => self.ioregsel as u32,
            IOWIN => match self.ioregsel {
                IOAPIC_ID => 0x0000_0000,
                IOAPIC_VER => 0x0017_0020,
                IOAPIC_ARB => 0x0000_0000,
                IOAPIC_REDTBL_BASE..=0x3f => self.read_ioapic_redirection_register(),
                _ => {
                    log::warn!(
                        "IOAPIC: read from unknown register index 0x{:02x}, returning 0xFFFFFFFF",
                        self.ioregsel
                    );
                    0xffff_ffff
                }
            },
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
            self.read_lapic_register(offset)
        } else {
            self.read_ioapic_register(offset)
        };

        let bytes_to_copy = data.len().min(4);
        for (i, slot) in data.iter_mut().take(bytes_to_copy).enumerate() {
            *slot = ((value >> (i * 8)) & 0xff) as u8;
        }

        if self.base == LAPIC_BASE {
            log::trace!(
                "LAPIC read at offset=0x{:x}, len={}, value=0x{:08x}",
                offset,
                data.len(),
                value
            );
        } else {
            log::trace!(
                "IOAPIC read at offset=0x{:x}, len={}, value=0x{:08x} (ioregsel=0x{:02x})",
                offset,
                data.len(),
                value,
                self.ioregsel
            );
        }
    }

    fn write(&mut self, _base: u64, offset: u64, data: &[u8]) {
        let mut value: u32 = 0;
        for (i, &byte) in data.iter().enumerate().take(4) {
            value |= (byte as u32) << (i * 8);
        }

        if self.base == LAPIC_BASE {
            match offset {
                LAPIC_TPR => self.tpr = value & 0xff,
                LAPIC_EOI => log::trace!("LAPIC EOI write"),
                LAPIC_SPURIOUS => self.spurious_vector = value,
                LAPIC_ICR_LOW | LAPIC_ICR_HIGH => {
                    log::trace!("LAPIC ICR write at offset=0x{:x}: 0x{:08x}", offset, value)
                }
                LAPIC_TIMER_INITIAL => {
                    log::trace!("LAPIC Timer Initial Count write: 0x{:08x}", value)
                }
                LAPIC_TIMER_DIVIDE => {
                    log::trace!("LAPIC Timer Divide Config write: 0x{:08x}", value)
                }
                _ => log::trace!(
                    "LAPIC write at offset=0x{:x}, len={}, value=0x{:08x}",
                    offset,
                    data.len(),
                    value
                ),
            }
        } else {
            match offset {
                IOREGSEL => {
                    self.ioregsel = (value & 0xff) as u8;
                    log::trace!("IOAPIC IOREGSEL write: 0x{:02x}", self.ioregsel);
                }
                IOWIN if (IOAPIC_REDTBL_BASE..=0x3f).contains(&self.ioregsel) => {
                    self.write_ioapic_redirection_register(value);
                }
                IOWIN => {
                    log::trace!(
                        "IOAPIC IOWIN write: reg=0x{:02x}, value=0x{:08x}",
                        self.ioregsel,
                        value
                    );
                }
                _ => log::warn!(
                    "IOAPIC write at unknown offset=0x{:x}, len={}, value=0x{:08x}",
                    offset,
                    data.len(),
                    value
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::BusDevice;

    use super::{query_route, ApicStub, IoApicRoute, IOAPIC_REDTBL_BASE, IOREGSEL, IOWIN};

    #[test]
    fn ioapic_redirection_table_is_persistent() {
        let mut stub = ApicStub::new(super::IOAPIC_BASE);

        stub.write(0, IOREGSEL, &[IOAPIC_REDTBL_BASE, 0, 0, 0]);
        stub.write(0, IOWIN, &[0x31, 0x08, 0x00, 0x00]);
        stub.write(0, IOREGSEL, &[IOAPIC_REDTBL_BASE + 1, 0, 0, 0]);
        stub.write(0, IOWIN, &[0x00, 0x00, 0x00, 0x02]);

        assert_eq!(
            query_route(0).unwrap(),
            IoApicRoute {
                vector: 0x31,
                delivery_mode: 0,
                destination_mode_logical: true,
                trigger_mode_level: false,
                masked: false,
                destination: 0x02,
            }
        );

        let mut low = [0u8; 4];
        let mut high = [0u8; 4];
        stub.write(0, IOREGSEL, &[IOAPIC_REDTBL_BASE, 0, 0, 0]);
        stub.read(0, IOWIN, &mut low);
        stub.write(0, IOREGSEL, &[IOAPIC_REDTBL_BASE + 1, 0, 0, 0]);
        stub.read(0, IOWIN, &mut high);

        assert_eq!(u32::from_le_bytes(low), 0x0000_0831);
        assert_eq!(u32::from_le_bytes(high), 0x0200_0000);
    }
}
