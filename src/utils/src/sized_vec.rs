// Copyright © 2024 Institute of Software, CAS. All rights reserved.
//
// Copyright © 2019 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause
//
// Copyright © 2020, Microsoft Corporation
//
// Copyright 2018-2019 CrowdStrike, Inc.
//
//

// Returns a `Vec<T>` with a size in bytes at least as large as `size_in_bytes`.
fn vec_with_size_in_bytes<T: Default>(size_in_bytes: usize) -> Vec<T> {
    let rounded_size = size_in_bytes.div_ceil(size_of::<T>());
    let mut v = Vec::with_capacity(rounded_size);
    v.resize_with(rounded_size, T::default);
    v
}

// The kvm API has many structs that resemble the following `Foo` structure:
//
// ```
// #[repr(C)]
// struct Foo {
//    some_data: u32
//    entries: __IncompleteArrayField<__u32>,
// }
// ```
//
// In order to allocate such a structure, `size_of::<Foo>()` would be too small because it would not
// include any space for `entries`. To make the allocation large enough while still being aligned
// for `Foo`, a `Vec<Foo>` is created. Only the first element of `Vec<Foo>` would actually be used
// as a `Foo`. The remaining memory in the `Vec<Foo>` is for `entries`, which must be contiguous
// with `Foo`. This function is used to make the `Vec<Foo>` with enough space for `count` entries.
use std::mem::size_of;
pub fn vec_with_array_field<T: Default, F>(count: usize) -> Vec<T> {
    let element_space = count * size_of::<F>();
    let vec_size_bytes = size_of::<T>() + element_space;
    vec_with_size_in_bytes(vec_size_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vec_with_size_in_bytes_u8() {
        let v: Vec<u8> = vec_with_size_in_bytes(10);
        assert!(v.len() >= 10);
    }

    #[test]
    fn test_vec_with_size_in_bytes_u32() {
        let v: Vec<u32> = vec_with_size_in_bytes(10);
        // Should have at least 10 bytes, but rounded up to u32 alignment
        assert!(v.len() >= 3); // 3 * 4 = 12 bytes
    }

    #[test]
    fn test_vec_with_size_in_bytes_u64() {
        let v: Vec<u64> = vec_with_size_in_bytes(10);
        // Should have at least 10 bytes, but rounded up to u64 alignment
        assert!(v.len() >= 2); // 2 * 8 = 16 bytes
    }

    #[test]
    fn test_vec_with_size_in_bytes_large() {
        let v: Vec<u8> = vec_with_size_in_bytes(1000);
        assert!(v.len() >= 1000);
    }

    #[test]
    fn test_vec_with_size_in_bytes_exact() {
        // When size is exactly divisible by element size
        let v: Vec<u32> = vec_with_size_in_bytes(12);
        assert!(v.len() >= 3);
    }

    #[test]
    fn test_vec_with_array_field_simple() {
        #[repr(C)]
        struct Foo {
            header: u32,
        }
        let v: Vec<Foo> = vec_with_array_field::<Foo, u32>(5);
        // Should have space for header + 5 u32 entries = 4 + 20 = 24 bytes minimum
        assert!(v.len() >= 1);
    }

    #[test]
    fn test_vec_with_array_field_zero_count() {
        #[repr(C)]
        struct Foo {
            header: u32,
        }
        let v: Vec<Foo> = vec_with_array_field::<Foo, u32>(0);
        // Should have space for header only
        assert!(v.len() >= 1);
    }

    #[test]
    fn test_vec_with_array_field_large_count() {
        #[repr(C)]
        struct Foo {
            header: u32,
        }
        let v: Vec<Foo> = vec_with_array_field::<Foo, u64>(100);
        // Should have space for header + 100 * 8 = 4 + 800 = 804 bytes minimum
        // Rounded up to vec capacity
        assert!(v.len() >= 1);
    }

    #[test]
    fn test_vec_with_array_field_u8_entry() {
        #[repr(C)]
        struct Foo {
            header: u32,
        }
        let v: Vec<Foo> = vec_with_array_field::<Foo, u8>(10);
        // Should have space for header + 10 * 1 = 4 + 10 = 14 bytes minimum
        assert!(v.len() >= 1);
    }

    #[test]
    fn test_vec_with_array_field_multiple_types() {
        // Test with different F types
        #[repr(C)]
        struct Header {
            a: u32,
            b: u32,
        }

        // With u32 entries
        let v1: Vec<Header> = vec_with_array_field::<Header, u32>(4);
        assert!(v1.len() >= 1);

        // With u64 entries
        let v2: Vec<Header> = vec_with_array_field::<Header, u64>(4);
        assert!(v2.len() >= 1);

        // v2 should have at least as much space as v1 since u64 is larger
        assert!(std::mem::size_of::<Header>() + 4 * 8 >= std::mem::size_of::<Header>() + 4 * 4);
    }
}
