// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod acpi;
pub(crate) mod interrupts;
pub(crate) mod registers;
pub mod stdin_reader;
pub mod vstate;
mod whpx_vcpu;

pub(crate) fn hyperv_enlightenments_enabled() -> bool {
    static VALUE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        hyperv_enlightenments_from_value(
            std::env::var("LIBKRUN_WINDOWS_HYPERV_ENLIGHTENMENTS")
                .ok()
                .as_deref(),
        )
    })
}

fn hyperv_enlightenments_from_value(value: Option<&str>) -> bool {
    value.map_or(true, |value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::hyperv_enlightenments_from_value;

    #[test]
    fn hyperv_enlightenments_are_boot_safe_by_default() {
        assert!(hyperv_enlightenments_from_value(None));
        assert!(hyperv_enlightenments_from_value(Some("true")));
        assert!(!hyperv_enlightenments_from_value(Some("false")));
    }
}
