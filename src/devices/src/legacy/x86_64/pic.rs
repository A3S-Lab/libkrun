// Copyright 2026 Red Hat, Inc.
// SPDX-License-Identifier: Apache-2.0
//
// Minimal 8259A PIC (Programmable Interrupt Controller) stub.
//
// The Linux kernel initializes the 8259A during early boot before switching to
// APIC mode. This stub silently absorbs all initialization writes and returns
// 0x00 on reads (no IRQs pending in ISR/IRR).
//
// Register one instance at 0x20 (size 2) for the primary PIC and another at
// 0xA0 (size 2) for the secondary (cascaded) PIC.

use crate::bus::BusDevice;

/// No-op 8259A PIC stub.
pub struct Pic {
    regs: [u8; 2],
}

impl Pic {
    pub fn new() -> Self {
        Pic { regs: [0u8; 2] }
    }
}

impl BusDevice for Pic {
    fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
        if data.len() != 1 {
            return;
        }
        data[0] = if offset < 2 { self.regs[offset as usize] } else { 0 };
    }

    fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
        if data.len() != 1 {
            return;
        }
        if offset < 2 {
            self.regs[offset as usize] = data[0];
        }
    }
}
