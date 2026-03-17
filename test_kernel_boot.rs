// Simple test to verify kernel boot with higher-half mapping
use std::env;
use std::path::PathBuf;

fn main() {
    env_logger::init();

    // Check if libkrunfw.dll exists
    let libkrunfw_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("libkrunfw.dll");

    if !libkrunfw_path.exists() {
        eprintln!("Error: libkrunfw.dll not found at {:?}", libkrunfw_path);
        eprintln!("Please build libkrunfw first");
        std::process::exit(1);
    }

    println!("Found libkrunfw.dll at {:?}", libkrunfw_path);

    // Create a minimal rootfs
    let temp_dir = std::env::temp_dir().join("libkrun-test-rootfs");
    std::fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

    // Create init script
    let init_path = temp_dir.join("init");
    std::fs::write(&init_path, "#!/bin/sh\necho 'Kernel booted successfully!'\n")
        .expect("Failed to write init script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&init_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&init_path, perms).unwrap();
    }

    println!("Created test rootfs at {:?}", temp_dir);
    println!("Attempting to boot kernel...");

    // Try to create a libkrun context
    unsafe {
        let ctx_id = krun::krun_create_ctx();
        if ctx_id < 0 {
            eprintln!("Failed to create krun context: {}", ctx_id);
            std::process::exit(1);
        }

        println!("Created krun context: {}", ctx_id);

        // Set root path
        let root_cstr = std::ffi::CString::new(temp_dir.to_str().unwrap()).unwrap();
        let ret = krun::krun_set_root(ctx_id, root_cstr.as_ptr());
        if ret < 0 {
            eprintln!("Failed to set root: {}", ret);
            krun::krun_free_ctx(ctx_id);
            std::process::exit(1);
        }

        println!("Set root path");

        // Set working directory
        let workdir_cstr = std::ffi::CString::new("/").unwrap();
        let ret = krun::krun_set_workdir(ctx_id, workdir_cstr.as_ptr());
        if ret < 0 {
            eprintln!("Failed to set workdir: {}", ret);
            krun::krun_free_ctx(ctx_id);
            std::process::exit(1);
        }

        println!("Set working directory");

        // Set exec path
        let exec_cstr = std::ffi::CString::new("/init").unwrap();
        let ret = krun::krun_set_exec(ctx_id, exec_cstr.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut());
        if ret < 0 {
            eprintln!("Failed to set exec: {}", ret);
            krun::krun_free_ctx(ctx_id);
            std::process::exit(1);
        }

        println!("Set exec path");
        println!("Starting VM...");

        // Start the VM
        let ret = krun::krun_start_enter(ctx_id);
        println!("VM exited with code: {}", ret);

        krun::krun_free_ctx(ctx_id);
    }
}
