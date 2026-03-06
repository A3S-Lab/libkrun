//! Raw FFI bindings for `krun.dll` on Windows (WHPX backend).
//!
//! This crate exposes the Windows-specific subset of the libkrun C API as
//! defined in `include/libkrun_windows.h`.  All symbols are imported from
//! `krun.dll` at runtime; the import library (`krun.lib`) is required at
//! link time.
//!
//! # Safety
//! All functions are `unsafe` because they cross the FFI boundary and
//! accept raw pointers.  Callers are responsible for ensuring:
//!  - Pointer arguments are valid, non-null where required, and
//!    null-terminated where the API specifies a C string.
//!  - `ctx_id` values are obtained from `krun_create_ctx()` and have not
//!    yet been passed to `krun_start_enter()` or `krun_free_ctx()`.
//!
//! # Minimum usage (Windows)
//! ```no_run
//! use a3s_libkrun_sys::*;
//! use std::ffi::CString;
//!
//! unsafe {
//!     let ctx = krun_create_ctx();
//!     assert!(ctx >= 0, "krun_create_ctx failed: {ctx}");
//!     let ctx_id = ctx as u32;
//!
//!     krun_set_vm_config(ctx_id, 2, 512);
//!
//!     let kernel = CString::new("C:\\vms\\vmlinux").unwrap();
//!     let cmdline = CString::new("console=ttyS0 root=/dev/vda").unwrap();
//!     krun_set_kernel(ctx_id, kernel.as_ptr(), KRUN_KERNEL_FORMAT_ELF,
//!                     std::ptr::null(), cmdline.as_ptr());
//!
//!     let disk = CString::new("C:\\vms\\root.img").unwrap();
//!     let id   = CString::new("root").unwrap();
//!     krun_add_disk(ctx_id, id.as_ptr(), disk.as_ptr(), false);
//!
//!     krun_start_enter(ctx_id); // does not return on success
//! }
//! ```

#![cfg(windows)]
#![allow(non_camel_case_types, non_upper_case_globals, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void};

// ---------------------------------------------------------------------------
// Constants — must mirror libkrun_windows.h exactly
// ---------------------------------------------------------------------------

// Logging
pub const KRUN_LOG_TARGET_DEFAULT: c_int = -1;

pub const KRUN_LOG_LEVEL_OFF: u32   = 0;
pub const KRUN_LOG_LEVEL_ERROR: u32 = 1;
pub const KRUN_LOG_LEVEL_WARN: u32  = 2;
pub const KRUN_LOG_LEVEL_INFO: u32  = 3;
pub const KRUN_LOG_LEVEL_DEBUG: u32 = 4;
pub const KRUN_LOG_LEVEL_TRACE: u32 = 5;

pub const KRUN_LOG_STYLE_AUTO: u32   = 0;
pub const KRUN_LOG_STYLE_ALWAYS: u32 = 1;
pub const KRUN_LOG_STYLE_NEVER: u32  = 2;

pub const KRUN_LOG_OPTION_NO_ENV: u32 = 1;

// Disk formats
pub const KRUN_DISK_FORMAT_RAW: u32   = 0;
pub const KRUN_DISK_FORMAT_QCOW2: u32 = 1;
pub const KRUN_DISK_FORMAT_VMDK: u32  = 2;

// Kernel formats
pub const KRUN_KERNEL_FORMAT_RAW: u32        = 0;
pub const KRUN_KERNEL_FORMAT_ELF: u32        = 1;
pub const KRUN_KERNEL_FORMAT_PE_GZ: u32      = 2;
pub const KRUN_KERNEL_FORMAT_IMAGE_BZ2: u32  = 3;
pub const KRUN_KERNEL_FORMAT_IMAGE_GZ: u32   = 4;
pub const KRUN_KERNEL_FORMAT_IMAGE_ZSTD: u32 = 5;

// TSI feature flags
pub const KRUN_TSI_HIJACK_INET: u32 = 1 << 0;
pub const KRUN_TSI_HIJACK_UNIX: u32 = 1 << 1;

// virtio-net feature flags
pub const NET_FEATURE_CSUM: u32       = 1 << 0;
pub const NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
pub const NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
pub const NET_FEATURE_GUEST_TSO6: u32 = 1 << 8;
pub const NET_FEATURE_GUEST_UFO: u32  = 1 << 10;
pub const NET_FEATURE_HOST_TSO4: u32  = 1 << 11;
pub const NET_FEATURE_HOST_TSO6: u32  = 1 << 12;
pub const NET_FEATURE_HOST_UFO: u32   = 1 << 14;

// ---------------------------------------------------------------------------
// FFI declarations — extern "C" block mirrors libkrun_windows.h
// ---------------------------------------------------------------------------

#[link(name = "krun")]
extern "C" {
    // --- Logging ---

    /// Sets the log level.  Level is one of `KRUN_LOG_LEVEL_*`.
    pub fn krun_set_log_level(level: u32) -> i32;

    /// Initialises logging.
    ///
    /// `target_fd`: CRT fd, or `KRUN_LOG_TARGET_DEFAULT` (-1) for stderr.
    /// On Windows only -1, 1 (stdout), and 2 (stderr) are accepted; other
    /// values return `-EINVAL`.
    pub fn krun_init_log(
        target_fd: c_int,
        level: u32,
        style: u32,
        options: u32,
    ) -> i32;

    // --- Context lifecycle ---

    /// Creates a configuration context.  Returns context ID (≥ 0) or < 0 on error.
    pub fn krun_create_ctx() -> i32;

    /// Frees a configuration context created by `krun_create_ctx`.
    pub fn krun_free_ctx(ctx_id: u32) -> i32;

    // --- VM configuration ---

    /// Sets the number of vCPUs and RAM size in MiB.
    pub fn krun_set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> i32;

    /// Sets the kernel image to load.
    ///
    /// **REQUIRED on Windows** — must be called before `krun_start_enter`.
    /// Use `KRUN_KERNEL_FORMAT_ELF` for a raw ELF vmlinux.
    ///
    /// `initramfs` and `cmdline` may be null.
    pub fn krun_set_kernel(
        ctx_id: u32,
        kernel_path: *const c_char,
        kernel_format: u32,
        initramfs: *const c_char,
        cmdline: *const c_char,
    ) -> i32;

    /// Sets the firmware image path.
    pub fn krun_set_firmware(ctx_id: u32, firmware_path: *const c_char) -> i32;

    /// Configures split-IRQCHIP mode.
    pub fn krun_split_irqchip(ctx_id: u32, enable: bool) -> i32;

    // --- Root filesystem / virtiofs ---

    /// Exposes a host directory as the guest root via virtio-fs.
    pub fn krun_set_root(ctx_id: u32, root_path: *const c_char) -> i32;

    /// Adds a virtio-fs device with the given tag.
    pub fn krun_add_virtiofs(
        ctx_id: u32,
        c_tag: *const c_char,
        c_path: *const c_char,
    ) -> i32;

    /// Adds a virtio-fs device with an explicit DAX window size (bytes).
    pub fn krun_add_virtiofs2(
        ctx_id: u32,
        c_tag: *const c_char,
        c_path: *const c_char,
        shm_size: u64,
    ) -> i32;

    /// No longer supported.  Always returns `-EINVAL`.
    pub fn krun_set_mapped_volumes(
        ctx_id: u32,
        mapped_volumes: *const *const c_char,
    ) -> i32;

    // --- Block devices (Windows) ---

    /// Adds a raw virtio-blk disk image.
    ///
    /// `block_id`: device identifier (e.g. `"root"`).
    /// `disk_path`: path to the raw disk image on the host.
    pub fn krun_add_disk(
        ctx_id: u32,
        block_id: *const c_char,
        disk_path: *const c_char,
        read_only: bool,
    ) -> i32;

    // --- Networking (Windows) ---

    /// Adds a virtio-net device backed by an optional TCP socket.
    ///
    /// `c_mac`: pointer to a 6-byte MAC address.
    /// `c_tcp_addr`: `"host:port"` string, or null for a disconnected device.
    pub fn krun_add_net_tcp(
        ctx_id: u32,
        c_iface_id: *const c_char,
        c_mac: *const u8,
        c_tcp_addr: *const c_char,
    ) -> i32;

    /// Configures host→guest TCP port forwarding for the TSI backend.
    ///
    /// `port_map`: null-terminated array of `"host_port:guest_port"` strings.
    /// Pass null to expose all listening guest ports; pass a pointer to a
    /// null entry to expose none.
    pub fn krun_set_port_map(
        ctx_id: u32,
        port_map: *const *const c_char,
    ) -> i32;

    // --- VSock / TSI ---

    /// Adds a vsock device with the specified TSI features.
    ///
    /// Call `krun_disable_implicit_vsock` first when using explicit control.
    /// `tsi_features`: bitmask of `KRUN_TSI_HIJACK_*`; use 0 for plain vsock.
    pub fn krun_add_vsock(ctx_id: u32, tsi_features: u32) -> i32;

    /// Maps guest vsock `port` to a Windows Named Pipe (`\\.\pipe\<name>`).
    ///
    /// `c_pipe_name`: pipe name without the `\\.\pipe\` prefix.
    pub fn krun_add_vsock_port_windows(
        ctx_id: u32,
        port: u32,
        c_pipe_name: *const c_char,
    ) -> i32;

    /// Disables the implicit vsock device created by libkrun automatically.
    pub fn krun_disable_implicit_vsock(ctx_id: u32) -> i32;

    // --- Console ---

    /// Redirects implicit console output to a file.
    pub fn krun_set_console_output(
        ctx_id: u32,
        c_filepath: *const c_char,
    ) -> i32;

    /// Disables the implicit console device.
    pub fn krun_disable_implicit_console(ctx_id: u32) -> i32;

    /// Sets the `console=` kernel command-line argument.
    pub fn krun_set_kernel_console(
        ctx_id: u32,
        console_id: *const c_char,
    ) -> i32;

    /// Adds a virtio-console device (auto-configured single-port).
    pub fn krun_add_virtio_console_default(
        ctx_id: u32,
        input_fd: c_int,
        output_fd: c_int,
        err_fd: c_int,
    ) -> i32;

    /// Adds a legacy serial device (ttyS0, ttyS1, …).
    pub fn krun_add_serial_console_default(
        ctx_id: u32,
        input_fd: c_int,
        output_fd: c_int,
    ) -> i32;

    /// Adds a multi-port virtio-console device.  Returns the console_id (≥ 0).
    pub fn krun_add_virtio_console_multiport(ctx_id: u32) -> i32;

    /// Adds a TTY port to a multi-port virtio-console device.
    pub fn krun_add_console_port_tty(
        ctx_id: u32,
        console_id: u32,
        name: *const c_char,
        tty_fd: c_int,
    ) -> i32;

    /// Adds a bidirectional I/O port to a multi-port virtio-console device.
    pub fn krun_add_console_port_inout(
        ctx_id: u32,
        console_id: u32,
        name: *const c_char,
        input_fd: c_int,
        output_fd: c_int,
    ) -> i32;

    // --- Workload execution ---

    /// Sets the working directory inside the guest.
    pub fn krun_set_workdir(
        ctx_id: u32,
        workdir_path: *const c_char,
    ) -> i32;

    /// Sets the executable path, arguments, and environment.
    ///
    /// `argv` and `envp` are null-terminated arrays of C strings, or null.
    /// When `envp` is null the host environment is inherited.
    pub fn krun_set_exec(
        ctx_id: u32,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> i32;

    /// Sets environment variables for the guest executable.
    pub fn krun_set_env(
        ctx_id: u32,
        envp: *const *const c_char,
    ) -> i32;

    /// Sets resource limits passed to the guest init process.
    pub fn krun_set_rlimits(
        ctx_id: u32,
        c_rlimits: *const *const c_char,
    ) -> i32;

    /// Stores a UID to drop privileges to.  On Windows this is a no-op.
    pub fn krun_setuid(ctx_id: u32, uid: u32) -> i32;

    /// Stores a GID to drop privileges to.  On Windows this is a no-op.
    pub fn krun_setgid(ctx_id: u32, gid: u32) -> i32;

    // --- Firmware metadata ---

    /// Sets SMBIOS OEM strings exposed to the guest.
    pub fn krun_set_smbios_oem_strings(
        ctx_id: u32,
        oem_strings: *const *const c_char,
    ) -> i32;

    // --- Sound ---

    /// Enables or disables the virtio-snd device.
    pub fn krun_set_snd_device(ctx_id: u32, enable: bool) -> i32;

    // --- Shutdown ---

    /// Returns a Windows event HANDLE (as i32) for signalling graceful shutdown.
    pub fn krun_get_shutdown_eventfd(ctx_id: u32) -> i32;

    // --- Nested virtualisation stubs ---

    /// Always returns `-EINVAL` on Windows.
    pub fn krun_set_nested_virt(ctx_id: u32, enabled: bool) -> i32;

    /// Always returns `-EOPNOTSUPP` on Windows.
    pub fn krun_check_nested_virt() -> i32;

    // NOTE: krun_get_max_vcpus() is NOT exported from krun.dll.
    //       It is compiled only for Linux and macOS.

    // --- GPU / display stubs (return -ENOTSUP without the gpu feature) ---

    pub fn krun_set_gpu_options(ctx_id: u32, virgl_flags: u32) -> i32;

    pub fn krun_set_gpu_options2(
        ctx_id: u32,
        virgl_flags: u32,
        shm_size: u64,
    ) -> i32;

    pub fn krun_set_display_backend(
        ctx_id: u32,
        vtable: *const c_void,
        vtable_size: usize,
    ) -> i32;

    pub fn krun_add_display(ctx_id: u32, width: u32, height: u32) -> i32;

    pub fn krun_display_set_refresh_rate(
        ctx_id: u32,
        display_id: u32,
        refresh_rate: u32,
    ) -> i32;

    pub fn krun_display_set_edid(
        ctx_id: u32,
        display_id: u32,
        edid_blob: *const u8,
        blob_size: usize,
    ) -> i32;

    pub fn krun_display_set_physical_size(
        ctx_id: u32,
        display_id: u32,
        width_mm: u16,
        height_mm: u16,
    ) -> i32;

    pub fn krun_display_set_dpi(
        ctx_id: u32,
        display_id: u32,
        dpi: u32,
    ) -> i32;

    // --- Input device stubs (return -ENOTSUP without the input feature) ---

    pub fn krun_add_input_device(
        ctx_id: u32,
        config_backend: *const c_void,
        config_backend_size: usize,
        event_provider_backend: *const c_void,
        event_provider_backend_size: usize,
    ) -> i32;

    pub fn krun_add_input_device_fd(ctx_id: u32, input_fd: c_int) -> i32;

    // --- Start ---

    /// Starts the microVM.  Does not return on success; calls `exit()` with
    /// the workload's exit code.  Returns `-EINVAL` if configuration is invalid.
    ///
    /// **PREREQUISITE**: `krun_set_kernel()` must have been called.
    pub fn krun_start_enter(ctx_id: u32) -> i32;
}
