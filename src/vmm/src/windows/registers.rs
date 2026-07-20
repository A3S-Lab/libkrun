// Copyright 2026 A3S Lab
// SPDX-License-Identifier: Apache-2.0

use std::{mem, ptr, slice};

use windows::Win32::System::Hypervisor::{
    WHvGetVirtualProcessorRegisters as raw_get_virtual_processor_registers,
    WHvSetVirtualProcessorRegisters as raw_set_virtual_processor_registers, WHV_PARTITION_HANDLE,
    WHV_REGISTER_NAME, WHV_REGISTER_VALUE,
};

// WinHvPlatformDefs.h declares WHV_UINT128 with DECLSPEC_ALIGN(16). The
// generated windows-rs type currently loses that alignment, which also leaves
// WHV_REGISTER_VALUE aligned to only 8 bytes. Recent WinHvPlatform builds use
// aligned SIMD loads and stores for these arrays, so an 8-byte-aligned Rust
// array can crash inside WinHvPlatform.dll instead of returning an HRESULT.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct AlignedRegisterValue {
    value: WHV_REGISTER_VALUE,
}

impl Default for AlignedRegisterValue {
    fn default() -> Self {
        Self {
            value: WHV_REGISTER_VALUE::default(),
        }
    }
}

fn is_aligned(value: *const WHV_REGISTER_VALUE) -> bool {
    (value as usize) % mem::align_of::<AlignedRegisterValue>() == 0
}

/// Calls `WHvGetVirtualProcessorRegisters` with a 16-byte-aligned value array.
///
/// # Safety
///
/// `register_names` and `register_values` must reference arrays containing at
/// least `register_count` elements, as required by WinHvPlatform.
pub(crate) unsafe fn get_virtual_processor_registers(
    partition: WHV_PARTITION_HANDLE,
    vp_index: u32,
    register_names: *const WHV_REGISTER_NAME,
    register_count: u32,
    register_values: *mut WHV_REGISTER_VALUE,
) -> windows::core::Result<()> {
    if register_count == 0 || is_aligned(register_values) {
        return raw_get_virtual_processor_registers(
            partition,
            vp_index,
            register_names,
            register_count,
            register_values,
        );
    }

    let count = register_count as usize;
    let mut aligned_values = vec![AlignedRegisterValue::default(); count];
    raw_get_virtual_processor_registers(
        partition,
        vp_index,
        register_names,
        register_count,
        aligned_values.as_mut_ptr().cast(),
    )?;
    ptr::copy_nonoverlapping(
        aligned_values.as_ptr().cast::<WHV_REGISTER_VALUE>(),
        register_values,
        count,
    );
    Ok(())
}

/// Calls `WHvSetVirtualProcessorRegisters` with a 16-byte-aligned value array.
///
/// # Safety
///
/// `register_names` and `register_values` must reference arrays containing at
/// least `register_count` elements, as required by WinHvPlatform.
pub(crate) unsafe fn set_virtual_processor_registers(
    partition: WHV_PARTITION_HANDLE,
    vp_index: u32,
    register_names: *const WHV_REGISTER_NAME,
    register_count: u32,
    register_values: *const WHV_REGISTER_VALUE,
) -> windows::core::Result<()> {
    if register_count == 0 || is_aligned(register_values) {
        return raw_set_virtual_processor_registers(
            partition,
            vp_index,
            register_names,
            register_count,
            register_values,
        );
    }

    let values = slice::from_raw_parts(register_values, register_count as usize);
    let aligned_values: Vec<_> = values
        .iter()
        .copied()
        .map(|value| AlignedRegisterValue { value })
        .collect();
    raw_set_virtual_processor_registers(
        partition,
        vp_index,
        register_names,
        register_count,
        aligned_values.as_ptr().cast(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_register_value_preserves_layout() {
        assert_eq!(
            mem::size_of::<AlignedRegisterValue>(),
            mem::size_of::<WHV_REGISTER_VALUE>()
        );
        assert_eq!(mem::align_of::<AlignedRegisterValue>(), 16);

        let values = [AlignedRegisterValue::default(); 2];
        assert!(is_aligned(values.as_ptr().cast()));
    }
}
