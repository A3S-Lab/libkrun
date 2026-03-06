// examples/start_vm.rs — minimal end-to-end smoke test for a3s-libkrun-sys.
//
// Usage (PowerShell):
//   $env:LIBKRUN_DIR = "..\..\target\x86_64-pc-windows-msvc\release"
//   cargo run --example start_vm --target x86_64-pc-windows-msvc -- \
//       C:\vms\vmlinux C:\vms\root.img

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: start_vm <kernel_path> <disk_image_path>");
        std::process::exit(1);
    }

    #[cfg(windows)]
    unsafe {
        run(&args[1], &args[2]);
    }
    #[cfg(not(windows))]
    {
        eprintln!("this example only runs on Windows");
        std::process::exit(1);
    }
}

#[cfg(windows)]
unsafe fn run(kernel_path: &str, disk_path: &str) {
    use a3s_libkrun_sys::*;
    use std::ffi::CString;

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

    // 1. Create context
    let ctx_id = check!(krun_create_ctx()) as u32;
    println!("[OK] krun_create_ctx → ctx_id={ctx_id}");

    // 2. VM parameters
    check!(krun_set_vm_config(ctx_id, 2, 512));
    println!("[OK] krun_set_vm_config (2 vCPUs, 512 MiB)");

    // 3. Kernel  — REQUIRED on Windows
    let kernel = CString::new(kernel_path).unwrap();
    let cmdline = CString::new(
        "console=ttyS0 root=/dev/vda rw panic=1 init=/bin/sh",
    )
    .unwrap();
    check!(krun_set_kernel(
        ctx_id,
        kernel.as_ptr(),
        KRUN_KERNEL_FORMAT_ELF,
        std::ptr::null(),
        cmdline.as_ptr(),
    ));
    println!("[OK] krun_set_kernel");

    // 4. Disk image
    let disk  = CString::new(disk_path).unwrap();
    let blkid = CString::new("root").unwrap();
    check!(krun_add_disk(ctx_id, blkid.as_ptr(), disk.as_ptr(), false));
    println!("[OK] krun_add_disk");

    // 5. Console  — serial output to stdout
    check!(krun_add_serial_console_default(ctx_id, 0, 1));
    println!("[OK] krun_add_serial_console_default");

    // 6. Start (does not return on success)
    println!("[  ] krun_start_enter …");
    let ret = krun_start_enter(ctx_id);
    // Only reached on pre-start error
    eprintln!("[FAIL] krun_start_enter returned {ret}");
    std::process::exit(1);
}
