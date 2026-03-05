// Copyright 2026 Red Hat, Inc.
// SPDX-License-Identifier: Apache-2.0
//
// Minimal Intel 8254 PIT (Programmable Interval Timer) emulation.
// Handles I/O ports 0x40–0x43 (counter 0, counter 1, counter 2, control).
//
// The kernel programs counter 0 for IRQ 0 (periodic timer) and counter 2 for
// TSC calibration. We simulate counting-down values based on wall-clock time.
// IRQ 0 injection is handled externally via a timer thread (see builder.rs).

use std::time::Instant;

use crate::bus::BusDevice;

const PIT_CLOCK_HZ: u64 = 1_193_182;

struct Counter {
    /// Reload value; 0 is treated as 65536 per the 8254 spec.
    reload: u32,
    /// Access mode: 1 = LSB only, 2 = MSB only, 3 = LSB then MSB.
    access: u8,
    /// Write state for access mode 3 (true = waiting for MSB byte).
    write_hi: bool,
    /// Read state for access mode 3 (true = return MSB next).
    read_hi: bool,
    /// Wall-clock reference for computing the current count.
    start: Instant,
}

impl Counter {
    fn new() -> Self {
        Counter {
            reload: 0, // 0 = 65536
            access: 3,
            write_hi: false,
            read_hi: false,
            start: Instant::now(),
        }
    }

    /// Returns the simulated current counter value.
    fn current_count(&self) -> u16 {
        let reload = if self.reload == 0 {
            65536u64
        } else {
            self.reload as u64
        };
        let ticks = self.start.elapsed().as_micros() as u64 * PIT_CLOCK_HZ / 1_000_000;
        ((reload - (ticks % reload)) & 0xffff) as u16
    }

    fn write_byte(&mut self, val: u8) {
        match self.access {
            1 => {
                self.reload = val as u32;
                self.write_hi = false;
                self.start = Instant::now();
            }
            2 => {
                self.reload = (val as u32) << 8;
                self.write_hi = false;
                self.start = Instant::now();
            }
            3 => {
                if !self.write_hi {
                    self.reload = (self.reload & 0xff00) | val as u32;
                    self.write_hi = true;
                } else {
                    self.reload = (self.reload & 0x00ff) | ((val as u32) << 8);
                    self.write_hi = false;
                    self.start = Instant::now();
                }
            }
            _ => {}
        }
    }

    fn read_byte(&mut self) -> u8 {
        let count = self.current_count();
        match self.access {
            1 => count as u8,
            2 => (count >> 8) as u8,
            3 => {
                if !self.read_hi {
                    self.read_hi = true;
                    count as u8
                } else {
                    self.read_hi = false;
                    (count >> 8) as u8
                }
            }
            _ => 0,
        }
    }

    fn configure(&mut self, access: u8) {
        self.access = access;
        self.write_hi = false;
        self.read_hi = false;
    }
}

/// Minimal 8254 PIT emulation.
///
/// Register this device on the I/O bus at base 0x40 with size 0x4.
/// Periodic IRQ 0 injection must be started separately (see builder.rs).
pub struct Pit {
    counters: [Counter; 3],
}

impl Pit {
    pub fn new() -> Self {
        Pit {
            counters: [Counter::new(), Counter::new(), Counter::new()],
        }
    }
}

impl BusDevice for Pit {
    fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
        if data.len() != 1 {
            return;
        }
        let idx = (offset as usize).min(2);
        data[0] = self.counters[idx].read_byte();
    }

    fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
        if data.len() != 1 {
            return;
        }
        let val = data[0];
        match offset {
            0 | 1 | 2 => {
                let idx = offset as usize;
                self.counters[idx].write_byte(val);
            }
            3 => {
                // Control word: bits[7:6] = counter select, bits[5:4] = access mode.
                let counter_sel = (val >> 6) & 0x3;
                let access = (val >> 4) & 0x3;
                if counter_sel < 3 && access != 0 {
                    // access == 0 is a latch command; just ignore for simplicity.
                    self.counters[counter_sel as usize].configure(access);
                }
            }
            _ => {}
        }
    }
}
