/**
 * libkrun_windows.h — Windows (WHPX) public API for libkrun
 *
 * This header is the Windows-specific subset of libkrun.h.  It exposes only
 * the functions that are compiled and exported from krun.dll when built for
 * x86_64-pc-windows-msvc.  Functions that require Unix-specific features
 * (virtiofs DAX, TAP/unixstream/unixgram networking, passt/gvproxy, KVM
 * nested-virt, blk feature-gated disk helpers, etc.) are intentionally
 * absent.
 *
 * Key Windows differences from libkrun.h:
 *  - krun_set_kernel()       Required unless libkrunfw.dll is available
 *                            next to krun.dll to provide a bundled kernel.
 *  - krun_add_disk()         Windows-specific virtio-blk backend (raw file I/O).
 *  - krun_add_net_tcp()      Windows-specific virtio-net backed by a TCP socket.
 *  - krun_add_vsock_port_windows()  Maps a vsock port to a Named Pipe.
 *  - krun_get_max_vcpus()    NOT available on Windows (not exported from DLL).
 *  - uid / gid types         Are plain uint32_t (no POSIX uid_t / gid_t).
 *
 * Compatibility:
 *  Compiled with MSVC (x86_64-pc-windows-msvc).  All integer types use
 *  <stdint.h> aliases; no POSIX headers are required.
 */

#ifndef _LIBKRUN_WINDOWS_H
#define _LIBKRUN_WINDOWS_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

/* =========================================================================
 * Export / import macro
 * ========================================================================= */

#ifdef KRUN_BUILDING_DLL
#  define KRUN_API __declspec(dllexport)
#else
#  define KRUN_API __declspec(dllimport)
#endif

/* =========================================================================
 * Logging constants
 * ========================================================================= */

#define KRUN_LOG_TARGET_DEFAULT (-1)

#define KRUN_LOG_LEVEL_OFF   0
#define KRUN_LOG_LEVEL_ERROR 1
#define KRUN_LOG_LEVEL_WARN  2
#define KRUN_LOG_LEVEL_INFO  3
#define KRUN_LOG_LEVEL_DEBUG 4
#define KRUN_LOG_LEVEL_TRACE 5

#define KRUN_LOG_STYLE_AUTO   0
#define KRUN_LOG_STYLE_ALWAYS 1
#define KRUN_LOG_STYLE_NEVER  2

#define KRUN_LOG_OPTION_NO_ENV 1

/* =========================================================================
 * Disk format constants  (used by krun_add_disk on other platforms;
 * the Windows krun_add_disk only supports raw, listed here for completeness)
 * ========================================================================= */

#define KRUN_DISK_FORMAT_RAW   0
#define KRUN_DISK_FORMAT_QCOW2 1
#define KRUN_DISK_FORMAT_VMDK  2

/* =========================================================================
 * Kernel format constants  (passed to krun_set_kernel)
 * ========================================================================= */

#define KRUN_KERNEL_FORMAT_RAW       0
#define KRUN_KERNEL_FORMAT_ELF       1
#define KRUN_KERNEL_FORMAT_PE_GZ     2
#define KRUN_KERNEL_FORMAT_IMAGE_BZ2 3
#define KRUN_KERNEL_FORMAT_IMAGE_GZ  4
#define KRUN_KERNEL_FORMAT_IMAGE_ZSTD 5

/* =========================================================================
 * TSI (Transparent Socket Impersonation) feature flags
 * ========================================================================= */

#define KRUN_TSI_HIJACK_INET (1 << 0)
#define KRUN_TSI_HIJACK_UNIX (1 << 1)

/* =========================================================================
 * virtio-net feature flags  (used for future krun_add_net_* extensions)
 * ========================================================================= */

#define NET_FEATURE_CSUM        (1 << 0)
#define NET_FEATURE_GUEST_CSUM  (1 << 1)
#define NET_FEATURE_GUEST_TSO4  (1 << 7)
#define NET_FEATURE_GUEST_TSO6  (1 << 8)
#define NET_FEATURE_GUEST_UFO   (1 << 10)
#define NET_FEATURE_HOST_TSO4   (1 << 11)
#define NET_FEATURE_HOST_TSO6   (1 << 12)
#define NET_FEATURE_HOST_UFO    (1 << 14)

/* =========================================================================
 * Logging
 * ========================================================================= */

/**
 * Sets the log level for the library.
 *
 * Arguments:
 *  "level" - one of KRUN_LOG_LEVEL_{OFF,ERROR,WARN,INFO,DEBUG,TRACE}.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_log_level(uint32_t level);

/**
 * Initializes logging for the library.
 *
 * Arguments:
 *  "target_fd" - CRT file descriptor to write log to, or KRUN_LOG_TARGET_DEFAULT
 *                for stderr.  On Windows only -1 (default/stderr), 1 (stdout),
 *                and 2 (stderr) are accepted; other values return -EINVAL.
 *  "level"     - verbosity level (KRUN_LOG_LEVEL_* constants).
 *  "style"     - KRUN_LOG_STYLE_{AUTO,ALWAYS,NEVER}.
 *  "options"   - bitmask; use 0 for defaults, KRUN_LOG_OPTION_NO_ENV to
 *                disallow environment-variable overrides.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_init_log(int target_fd, uint32_t level, uint32_t style, uint32_t options);

/* =========================================================================
 * Context lifecycle
 * ========================================================================= */

/**
 * Creates a configuration context.
 *
 * Returns:
 *  The context ID (>= 0) on success or a negative error number on failure.
 */
KRUN_API int32_t krun_create_ctx(void);

/**
 * Frees an existing configuration context.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_free_ctx(uint32_t ctx_id);

/* =========================================================================
 * VM configuration
 * ========================================================================= */

/**
 * Sets the basic parameters for the microVM.
 *
 * Arguments:
 *  "ctx_id"    - the configuration context ID.
 *  "num_vcpus" - the number of vCPUs.
 *  "ram_mib"   - the amount of RAM in MiB.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_vm_config(uint32_t ctx_id, uint8_t num_vcpus, uint32_t ram_mib);

/**
 * Sets the path to the kernel to be loaded in the microVM.
 *
 * NOTE: On Windows this function must be called before krun_start_enter()
 * unless libkrunfw.dll is available next to krun.dll to provide the bundled
 * companion kernel. Use KRUN_KERNEL_FORMAT_ELF for a raw ELF vmlinux image
 * (e.g. from libkrunfw-windows or a custom build).
 *
 * Arguments:
 *  "ctx_id"        - the configuration context ID.
 *  "kernel_path"   - path to the kernel image on the host filesystem.
 *  "kernel_format" - kernel format (KRUN_KERNEL_FORMAT_* constants).
 *  "initramfs"     - path to the initramfs, or NULL if not needed.
 *  "cmdline"       - kernel command line, or NULL to use the default.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_kernel(uint32_t ctx_id,
                                  const char *kernel_path,
                                  uint32_t kernel_format,
                                  const char *initramfs,
                                  const char *cmdline);

/**
 * Sets the path to the firmware to be loaded in the microVM.
 *
 * Arguments:
 *  "ctx_id"        - the configuration context ID.
 *  "firmware_path" - path to the firmware on the host filesystem.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_firmware(uint32_t ctx_id, const char *firmware_path);

/**
 * Specify whether to split IRQCHIP responsibilities between host and guest.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *  "enable" - whether to enable the split IRQCHIP.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_split_irqchip(uint32_t ctx_id, bool enable);

/* =========================================================================
 * Root filesystem / virtiofs
 * ========================================================================= */

/**
 * Sets the path to be used as root for the microVM.
 *
 * Exposes the host directory at "root_path" as the guest root via virtio-fs.
 *
 * Arguments:
 *  "ctx_id"    - the configuration context ID.
 *  "root_path" - null-terminated path to the host directory.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_root(uint32_t ctx_id, const char *root_path);

/**
 * Adds an independent virtio-fs device pointing to a host directory.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *  "c_tag"  - tag to identify the filesystem in the guest.
 *  "c_path" - full path to the host directory.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_virtiofs(uint32_t ctx_id,
                                    const char *c_tag,
                                    const char *c_path);

/**
 * Adds an independent virtio-fs device with an explicit DAX window size.
 *
 * Arguments:
 *  "ctx_id"   - the configuration context ID.
 *  "c_tag"    - tag to identify the filesystem in the guest.
 *  "c_path"   - full path to the host directory.
 *  "shm_size" - size of the DAX SHM window in bytes.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_virtiofs2(uint32_t ctx_id,
                                     const char *c_tag,
                                     const char *c_path,
                                     uint64_t shm_size);

/**
 * NO LONGER SUPPORTED.  Always returns -EINVAL.
 */
KRUN_API int32_t krun_set_mapped_volumes(uint32_t ctx_id,
                                          const char *const mapped_volumes[]);

/* =========================================================================
 * Block devices  (Windows-specific)
 * ========================================================================= */

/**
 * Adds a virtio-blk disk device to the microVM.
 *
 * Windows implementation: uses the host filesystem (std::fs::File + Seek).
 * Only raw disk images are supported.
 *
 * NOTE: This signature is the Windows variant.  It differs from the Linux/macOS
 * variant in that it does not support non-Raw formats or sync-mode tuning.
 *
 * Arguments:
 *  "ctx_id"    - the configuration context ID.
 *  "block_id"  - null-terminated device identifier (e.g. "root", "data").
 *  "disk_path" - null-terminated path to the raw disk image on the host.
 *  "read_only" - if true the device is presented read-only to the guest.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_disk(uint32_t ctx_id,
                                const char *block_id,
                                const char *disk_path,
                                bool read_only);

/* =========================================================================
 * Networking
 * ========================================================================= */

/**
 * Adds a virtio-net device backed by an optional TCP socket.  (Windows only)
 *
 * The "krun_add_net_*" functions can be called multiple times.  In the guest
 * the interfaces appear in order: first added → eth0, second → eth1, etc.
 *
 * If no network interface is added, libkrun enables the TSI backend
 * automatically (unless krun_disable_implicit_vsock() is called).
 *
 * Arguments:
 *  "ctx_id"      - the configuration context ID.
 *  "c_iface_id"  - null-terminated ASCII identifier for MMIO registration.
 *  "c_mac"       - pointer to a 6-byte MAC address array.
 *  "c_tcp_addr"  - null-terminated TCP address string ("host:port", e.g.
 *                  "127.0.0.1:9000"), or NULL for a disconnected (drop-all)
 *                  device.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_net_tcp(uint32_t ctx_id,
                                   const char *c_iface_id,
                                   const uint8_t *c_mac,
                                   const char *c_tcp_addr);

/**
 * Configures a map of host→guest TCP port forwards for the TSI backend.
 *
 * Arguments:
 *  "ctx_id"   - the configuration context ID.
 *  "port_map" - NULL-terminated array of "host_port:guest_port" strings.
 *               Passing NULL exposes all listening guest ports to the host.
 *               Passing a pointer to a NULL entry means no port is exposed.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_port_map(uint32_t ctx_id,
                                    const char *const port_map[]);

/* =========================================================================
 * VSock / TSI
 * ========================================================================= */

/**
 * Adds a vsock device with the specified TSI feature flags.
 *
 * Must call krun_disable_implicit_vsock() first if you want explicit control.
 *
 * Arguments:
 *  "ctx_id"       - the configuration context ID.
 *  "tsi_features" - bitmask of TSI features (KRUN_TSI_HIJACK_INET,
 *                   KRUN_TSI_HIJACK_UNIX).  Use 0 for plain vsock.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_vsock(uint32_t ctx_id, uint32_t tsi_features);

/**
 * Maps a guest vsock port to a Windows Named Pipe on the host.  (Windows only)
 *
 * When the guest connects to CID 2 (host) on "port", the vsock device
 * connects to \\.\pipe\<pipe_name> on the host.
 *
 * Arguments:
 *  "ctx_id"     - the configuration context ID.
 *  "port"       - the guest vsock port number.
 *  "c_pipe_name"- null-terminated pipe name (without the \\.\pipe\ prefix),
 *                 e.g. "myservice".
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_vsock_port_windows(uint32_t ctx_id,
                                              uint32_t port,
                                              const char *c_pipe_name);

/**
 * Disables the implicit vsock device.
 *
 * By default libkrun creates a vsock device automatically.  Call this to
 * prevent that and add vsock manually via krun_add_vsock().
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_disable_implicit_vsock(uint32_t ctx_id);

/* =========================================================================
 * Console devices
 * ========================================================================= */

/**
 * Redirects the implicit console output to a file.
 *
 * Only applies to the implicitly created console.  No-op if the implicit
 * console was disabled via krun_disable_implicit_console().
 *
 * Arguments:
 *  "ctx_id"    - the configuration context ID.
 *  "c_filepath"- null-terminated path to the output file.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_console_output(uint32_t ctx_id, const char *c_filepath);

/**
 * Disables the implicit console device.
 *
 * After this call libkrun will not create any console device automatically.
 * Add consoles manually via krun_add_*_console_default() or the multiport API.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_disable_implicit_console(uint32_t ctx_id);

/**
 * Specifies the value of "console=" in the kernel command line.
 *
 * Arguments:
 *  "ctx_id"     - the configuration context ID.
 *  "console_id" - null-terminated console identifier (e.g. "ttyS0").
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_kernel_console(uint32_t ctx_id, const char *console_id);

/**
 * Adds a virtio-console device (single-port, auto-configured).
 *
 * In the guest this appears as hvcN in the order added.  If the implicit
 * console is not disabled it occupies hvc0 and the first explicit one is hvc1.
 *
 * Arguments:
 *  "ctx_id"    - the configuration context ID.
 *  "input_fd"  - CRT file descriptor for console input.
 *  "output_fd" - CRT file descriptor for console output.
 *  "err_fd"    - CRT file descriptor for error output.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_virtio_console_default(uint32_t ctx_id,
                                                  int input_fd,
                                                  int output_fd,
                                                  int err_fd);

/**
 * Adds a legacy serial device (ttyS0, ttyS1, ...) to the guest.
 *
 * Arguments:
 *  "ctx_id"    - the configuration context ID.
 *  "input_fd"  - CRT file descriptor for serial input.
 *  "output_fd" - CRT file descriptor for serial output.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_serial_console_default(uint32_t ctx_id,
                                                   int input_fd,
                                                   int output_fd);

/**
 * Adds a multi-port virtio-console device (no ports attached yet).
 *
 * Use krun_add_console_port_tty() or krun_add_console_port_inout() to
 * attach ports to the returned console_id.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *
 * Returns:
 *  The console_id (>= 0) on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_virtio_console_multiport(uint32_t ctx_id);

/**
 * Adds a TTY port to a multi-port virtio-console device.
 *
 * The port is marked VIRTIO_CONSOLE_CONSOLE_PORT (supports window resize).
 *
 * Arguments:
 *  "ctx_id"     - the configuration context ID.
 *  "console_id" - ID returned by krun_add_virtio_console_multiport().
 *  "name"       - null-terminated port name visible in the guest (may be "").
 *  "tty_fd"     - CRT file descriptor for the TTY (input + output).
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_console_port_tty(uint32_t ctx_id,
                                            uint32_t console_id,
                                            const char *name,
                                            int tty_fd);

/**
 * Adds a generic bidirectional I/O port to a multi-port virtio-console device.
 *
 * NOT marked as VIRTIO_CONSOLE_CONSOLE_PORT (no window-resize support).
 *
 * Arguments:
 *  "ctx_id"     - the configuration context ID.
 *  "console_id" - ID returned by krun_add_virtio_console_multiport().
 *  "name"       - null-terminated port name visible in the guest (may be "").
 *  "input_fd"   - CRT fd for input (host writes, guest reads).
 *  "output_fd"  - CRT fd for output (guest writes, host reads).
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_add_console_port_inout(uint32_t ctx_id,
                                              uint32_t console_id,
                                              const char *name,
                                              int input_fd,
                                              int output_fd);

/* =========================================================================
 * Workload execution
 * ========================================================================= */

/**
 * Sets the working directory for the executable inside the microVM.
 *
 * Arguments:
 *  "ctx_id"       - the configuration context ID.
 *  "workdir_path" - null-terminated path inside the guest root.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_workdir(uint32_t ctx_id, const char *workdir_path);

/**
 * Sets the executable, arguments, and environment variables for the microVM.
 *
 * Arguments:
 *  "ctx_id"    - the configuration context ID.
 *  "exec_path" - path to the executable, relative to the guest root.
 *  "argv"      - NULL-terminated array of argument strings, or NULL.
 *  "envp"      - NULL-terminated array of "KEY=VALUE" strings, or NULL to
 *                auto-inherit from the host environment.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_exec(uint32_t ctx_id,
                                const char *exec_path,
                                const char *const argv[],
                                const char *const envp[]);

/**
 * Sets environment variables for the executable inside the microVM.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *  "envp"   - NULL-terminated array of "KEY=VALUE" strings, or NULL to
 *             auto-inherit from the host environment.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_env(uint32_t ctx_id, const char *const envp[]);

/**
 * Sets resource limits to be applied inside the microVM.
 *
 * The limits are passed to the guest init process via the kernel command line.
 *
 * Arguments:
 *  "ctx_id"   - the configuration context ID.
 *  "c_rlimits"- NULL-terminated array of "RLIMIT_<TYPE>=<value>" strings.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_rlimits(uint32_t ctx_id, const char *const c_rlimits[]);

/**
 * Sets the UID to drop privileges to before starting the microVM.
 *
 * NOTE: On Windows the uid is stored but setuid() is a no-op.
 *       This function is provided for API compatibility.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *  "uid"    - the user ID (uint32_t on Windows).
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_setuid(uint32_t ctx_id, uint32_t uid);

/**
 * Sets the GID to drop privileges to before starting the microVM.
 *
 * NOTE: On Windows the gid is stored but setgid() is a no-op.
 *       This function is provided for API compatibility.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *  "gid"    - the group ID (uint32_t on Windows).
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_setgid(uint32_t ctx_id, uint32_t gid);

/* =========================================================================
 * SMBIOS / firmware metadata
 * ========================================================================= */

/**
 * Sets SMBIOS OEM strings exposed to the guest.
 *
 * Arguments:
 *  "ctx_id"     - the configuration context ID.
 *  "oem_strings"- NULL-terminated array of null-terminated OEM strings.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_smbios_oem_strings(uint32_t ctx_id,
                                              const char *const oem_strings[]);

/* =========================================================================
 * Sound
 * ========================================================================= */

/**
 * Enables or disables the virtio-snd device.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *  "enable" - true to add a sound device to the guest.
 *
 * Returns:
 *  Zero on success or a negative error number on failure.
 */
KRUN_API int32_t krun_set_snd_device(uint32_t ctx_id, bool enable);

/* =========================================================================
 * Shutdown event
 * ========================================================================= */

/**
 * Returns a Windows HANDLE (as an int32_t) to signal an orderly guest shutdown.
 *
 * Must be called before krun_start_enter().
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *
 * Returns:
 *  An event handle (>= 0) on success, or a negative error number on failure.
 */
KRUN_API int32_t krun_get_shutdown_eventfd(uint32_t ctx_id);

/* =========================================================================
 * Nested virtualization  (stubs — not supported on Windows)
 * ========================================================================= */

/**
 * Attempts to configure nested virtualization.
 *
 * NOTE: On Windows this always returns -EINVAL.
 *       The function is exported for API compatibility.
 *
 * Arguments:
 *  "ctx_id"  - the configuration context ID.
 *  "enabled" - requested state.
 *
 * Returns:
 *  -EINVAL on Windows.
 */
KRUN_API int32_t krun_set_nested_virt(uint32_t ctx_id, bool enabled);

/**
 * Checks whether nested virtualization is supported.
 *
 * NOTE: On Windows this always returns -EOPNOTSUPP.
 *       The function is exported for API compatibility.
 *
 * Returns:
 *  -EOPNOTSUPP on Windows.
 */
KRUN_API int32_t krun_check_nested_virt(void);

/*
 * NOTE: krun_get_max_vcpus() is NOT available on Windows.
 *       It is only compiled for Linux and macOS and is not exported
 *       from krun.dll.  Do not call it.
 */

/* =========================================================================
 * GPU / Display  (stubs — return -ENOTSUP without the gpu feature)
 * ========================================================================= */

/**
 * Sets virgl rendering options.  Returns -ENOTSUP when krun.dll was built
 * without the "gpu" feature (the standard Windows build).
 */
KRUN_API int32_t krun_set_gpu_options(uint32_t ctx_id, uint32_t virgl_flags);

/** Variant of krun_set_gpu_options with an explicit SHM window size. */
KRUN_API int32_t krun_set_gpu_options2(uint32_t ctx_id,
                                        uint32_t virgl_flags,
                                        uint64_t shm_size);

/** Sets a custom display backend vtable.  Returns -ENOTSUP without gpu. */
KRUN_API int32_t krun_set_display_backend(uint32_t ctx_id,
                                           const void *vtable,
                                           size_t vtable_size);

/** Adds a virtual display.  Returns -ENOTSUP without gpu. */
KRUN_API int32_t krun_add_display(uint32_t ctx_id, uint32_t width, uint32_t height);

/** Sets the refresh rate for a virtual display.  Returns -ENOTSUP without gpu. */
KRUN_API int32_t krun_display_set_refresh_rate(uint32_t ctx_id,
                                                uint32_t display_id,
                                                uint32_t refresh_rate);

/** Provides a raw EDID blob for a virtual display.  Returns -ENOTSUP without gpu. */
KRUN_API int32_t krun_display_set_edid(uint32_t ctx_id,
                                        uint32_t display_id,
                                        const uint8_t *edid_blob,
                                        size_t blob_size);

/** Sets the physical dimensions (mm) of a virtual display.  Returns -ENOTSUP without gpu. */
KRUN_API int32_t krun_display_set_physical_size(uint32_t ctx_id,
                                                 uint32_t display_id,
                                                 uint16_t width_mm,
                                                 uint16_t height_mm);

/** Sets the DPI for a virtual display.  Returns -ENOTSUP without gpu. */
KRUN_API int32_t krun_display_set_dpi(uint32_t ctx_id,
                                       uint32_t display_id,
                                       uint32_t dpi);

/* =========================================================================
 * Input devices  (stubs — return -ENOTSUP without the input feature)
 * ========================================================================= */

/** Adds an input device via vtables.  Returns -ENOTSUP without input. */
KRUN_API int32_t krun_add_input_device(uint32_t ctx_id,
                                        const void *config_backend,
                                        size_t config_backend_size,
                                        const void *event_provider_backend,
                                        size_t event_provider_backend_size);

/** Adds an input device via a file descriptor.  Returns -ENOTSUP without input. */
KRUN_API int32_t krun_add_input_device_fd(uint32_t ctx_id, int input_fd);

/* =========================================================================
 * Start
 * ========================================================================= */

/**
 * Starts and enters the microVM with the configured parameters.
 *
 * This function does not return on success; the VMM calls exit() with the
 * workload's exit code once the microVM shuts down.
 *
 * PREREQUISITE: on Windows, either krun_set_kernel() must have been called
 * first or libkrunfw.dll must be available to provide the bundled kernel.
 *
 * Arguments:
 *  "ctx_id" - the configuration context ID.
 *
 * Returns:
 *  -EINVAL if the VMM detects a configuration error before starting.
 *
 * Exit codes (from the guest init process):
 *  125 - init cannot set up the environment inside the microVM.
 *  126 - init found the executable but cannot execute it.
 *  127 - init cannot find the executable.
 */
KRUN_API int32_t krun_start_enter(uint32_t ctx_id);

#ifdef __cplusplus
}
#endif

#endif /* _LIBKRUN_WINDOWS_H */
