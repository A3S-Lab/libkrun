// Test kernel boot with higher-half mapping fix
//
// Usage:
//   1. With libkrunfw.dll (if available):
//      RUST_LOG=info cargo run --release --example test_kernel_boot
//
//   2. With external kernel (recommended for testing):
//      RUST_LOG=info cargo run --release --example test_kernel_boot -- <kernel_path>
//      Example: cargo run --release --example test_kernel_boot -- C:\vms\vmlinux
//
// To get a kernel:
//   Download from: https://github.com/containers/libkrunfw/releases
//   Extract vmlinux from the archive

fn main() {
    #[cfg(windows)]
    unsafe {
        let args: Vec<String> = std::env::args().collect();
        let kernel_path = if args.len() > 1 {
            Some(args[1].as_str())
        } else {
            None
        };
        run(kernel_path);
    }
    #[cfg(not(windows))]
    {
        eprintln!("This example only runs on Windows");
        std::process::exit(1);
    }
}

#[cfg(windows)]
use std::path::Path;

#[cfg(windows)]
unsafe fn run(kernel_path: Option<&str>) {
    use a3s_libkrun_sys::*;
    use std::ffi::CString;
    use std::path::PathBuf;

    macro_rules! check {
        ($call:expr) => {{
            let ret = $call;
            if ret < 0 {
                eprintln!("[FAIL] {} returned {}", stringify!($call), ret);
                std::process::exit(1);
            }
            ret
        }};
    }

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("krun-sys-windows must live under the repo root")
        .to_path_buf();

    // Create minimal rootfs
    let temp_dir = std::env::temp_dir().join("libkrun-test-rootfs");
    std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

    let exec_guest_path = if let Ok(payload_path) = std::env::var("LIBKRUN_TEST_GUEST_PAYLOAD") {
        let payload_path = PathBuf::from(payload_path);
        let init_binary = repo_root.join("init").join("init");

        if !init_binary.exists() {
            eprintln!("[FAIL] init binary not found: {}", init_binary.display());
            std::process::exit(1);
        }
        if !payload_path.exists() {
            eprintln!("[FAIL] guest payload not found: {}", payload_path.display());
            std::process::exit(1);
        }

        std::fs::copy(&init_binary, temp_dir.join("init"))
            .expect("Failed to copy init binary into test rootfs");
        copy_guest_payload(&payload_path, &temp_dir.join("payload"));
        println!(
            "[INFO] Prepared rootfs with init={} payload={}",
            init_binary.display(),
            payload_path.display()
        );
        "/payload"
    } else {
        let init_path = temp_dir.join("init");
        std::fs::write(
            &init_path,
            "#!/bin/sh\necho 'Kernel booted successfully!'\nls -la /\n",
        )
        .expect("Failed to write init script");
        "/init"
    };

    println!("[INFO] Created test rootfs at {:?}", temp_dir);

    // Initialize libkrun logging (3 = info level, 4 = debug level)
    krun_set_log_level(3);
    println!("[OK] krun_set_log_level");

    // 1. Create context
    let ctx_id = check!(krun_create_ctx()) as u32;
    println!("[OK] krun_create_ctx → ctx_id={}", ctx_id);

    // 2. VM parameters
    check!(krun_set_vm_config(ctx_id, 1, 256));
    println!("[OK] krun_set_vm_config (1 vCPU, 256 MiB)");

    // 3. Set kernel (if external kernel provided)
    if let Some(kernel) = kernel_path {
        println!("[INFO] Using external kernel: {}", kernel);
        let kernel_cstr = CString::new(kernel).unwrap();
        let mut cmdline = String::from("console=ttyS0 panic=1");
        if let Ok(extra) = std::env::var("LIBKRUN_TEST_KERNEL_CMDLINE_APPEND") {
            let extra = extra.trim();
            if !extra.is_empty() {
                cmdline.push(' ');
                cmdline.push_str(extra);
            }
        }
        let cmdline = CString::new(cmdline).unwrap();
        check!(krun_set_kernel(
            ctx_id,
            kernel_cstr.as_ptr(),
            KRUN_KERNEL_FORMAT_ELF,
            std::ptr::null(),
            cmdline.as_ptr(),
        ));
        println!("[OK] krun_set_kernel");
    } else {
        println!("[INFO] No external kernel specified, will try to use libkrunfw.dll");
    }

    // 4. Set root directory
    let root_path = CString::new(temp_dir.to_str().unwrap()).unwrap();
    check!(krun_set_root(ctx_id, root_path.as_ptr()));
    println!("[OK] krun_set_root");

    // 5. Set working directory
    let workdir = CString::new("/").unwrap();
    check!(krun_set_workdir(ctx_id, workdir.as_ptr()));
    println!("[OK] krun_set_workdir");

    // 6. Set exec
    let exec_path = CString::new(exec_guest_path).unwrap();
    check!(krun_set_exec(
        ctx_id,
        exec_path.as_ptr(),
        std::ptr::null_mut(),
        std::ptr::null_mut()
    ));
    println!("[OK] krun_set_exec");

    // 7. Console - serial output to stdout
    check!(krun_add_serial_console_default(ctx_id, 0, 1));
    println!("[OK] krun_add_serial_console_default");

    // 8. Start (does not return on success)
    println!("[INFO] Starting VM with higher-half kernel mapping...");
    println!("[INFO] Watch for page table configuration logs...");
    let ret = krun_start_enter(ctx_id);

    // Only reached on error
    eprintln!("[FAIL] krun_start_enter returned {}", ret);
    std::process::exit(1);
}

#[cfg(windows)]
fn copy_guest_payload(src: &Path, dst: &Path) {
    std::fs::copy(src, dst).unwrap_or_else(|e| {
        panic!(
            "Failed to copy guest payload from {} to {}: {e}",
            src.display(),
            dst.display()
        )
    });
}
