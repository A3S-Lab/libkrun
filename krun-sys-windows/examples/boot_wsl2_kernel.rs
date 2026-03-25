fn main() {
    #[cfg(windows)]
    unsafe {
        run();
    }

    #[cfg(not(windows))]
    {
        eprintln!("this example only runs on Windows");
        std::process::exit(1);
    }
}

#[cfg(windows)]
unsafe fn run() {
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

    let kernel_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            repo_root
                .join("src")
                .join("libkrunfw-win")
                .join("kernel")
                .join("vmlinux")
        });
    let root_path = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("init"));

    if !kernel_path.exists() {
        eprintln!("[FAIL] kernel not found: {}", kernel_path.display());
        std::process::exit(2);
    }
    if !root_path.exists() {
        eprintln!("[FAIL] root path not found: {}", root_path.display());
        std::process::exit(2);
    }

    let kernel = CString::new(kernel_path.to_string_lossy().as_bytes()).unwrap();
    let root = CString::new(root_path.to_string_lossy().as_bytes()).unwrap();
    let exec = CString::new("/init").unwrap();
    let workdir = CString::new("/").unwrap();
    let argv = [exec.as_ptr(), std::ptr::null()];
    let envp = [std::ptr::null()];

    println!("[START] Booting Windows WHPX VM with external WSL2 kernel");
    println!("[INFO] kernel: {}", kernel_path.display());
    println!("[INFO] root:   {}", root_path.display());

    check!(krun_set_log_level(KRUN_LOG_LEVEL_INFO));
    let ctx_id = check!(krun_create_ctx()) as u32;
    println!("[OK] krun_create_ctx -> {}", ctx_id);

    check!(krun_set_vm_config(ctx_id, 1, 256));
    println!("[OK] krun_set_vm_config (1 vCPU, 256 MiB)");

    check!(krun_set_kernel(
        ctx_id,
        kernel.as_ptr(),
        KRUN_KERNEL_FORMAT_ELF,
        std::ptr::null(),
        std::ptr::null(),
    ));
    println!("[OK] krun_set_kernel (ELF)");

    check!(krun_set_root(ctx_id, root.as_ptr()));
    println!("[OK] krun_set_root");

    check!(krun_set_workdir(ctx_id, workdir.as_ptr()));
    check!(krun_set_exec(
        ctx_id,
        exec.as_ptr(),
        argv.as_ptr(),
        envp.as_ptr()
    ));
    println!("[OK] krun_set_exec (/init)");

    check!(krun_add_serial_console_default(ctx_id, 0, 1));
    println!("[OK] krun_add_serial_console_default");

    println!("[RUN] krun_start_enter");
    let ret = krun_start_enter(ctx_id);
    eprintln!("[FAIL] krun_start_enter returned {}", ret);
    std::process::exit(1);
}
