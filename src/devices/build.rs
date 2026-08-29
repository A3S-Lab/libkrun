use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build/windows_init_wrapper.rs");
    println!("cargo:rerun-if-changed=../../init/init");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("missing OUT_DIR"));
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let wrapper_src = PathBuf::from("build/windows_init_wrapper.rs");
    let wrapped_init = PathBuf::from("../../../init/init");
    let wrapped_init_on_host = manifest_dir.join("..").join("..").join("init").join("init");
    if !wrapped_init_on_host.is_file() {
        panic!(
            "missing Linux guest init {}; run scripts/build-windows-init.ps1 first",
            wrapped_init_on_host.display()
        );
    }
    let wrapper_bin = out_dir.join("init.krun");

    let mut command = Command::new(&rustc);
    command
        .arg("--edition=2021")
        .arg("--crate-name")
        .arg("windows_init_wrapper")
        .arg("--target")
        .arg("x86_64-unknown-linux-musl")
        .arg("-C")
        .arg("linker=rust-lld")
        .arg("-C")
        .arg("link-self-contained=yes")
        .arg("-C")
        .arg("panic=abort")
        .arg("-O")
        .arg(&wrapper_src)
        .arg("-o")
        .arg(&wrapper_bin)
        .env("LIBKRUN_WRAPPED_INIT_PATH", &wrapped_init)
        .current_dir(&manifest_dir);

    if let Some(user_profile) = env::var_os("USERPROFILE") {
        command
            .arg("--remap-path-prefix")
            .arg(format!("{}=.", PathBuf::from(user_profile).display()));
    }
    command
        .arg("--remap-path-prefix")
        .arg(format!("{}=libkrun/src/devices", manifest_dir.display()));

    let status = command
        .status()
        .expect("failed to invoke rustc for Windows init wrapper");

    if !status.success() {
        panic!(
            "failed to build Windows init wrapper {} from {}",
            wrapper_bin.display(),
            wrapper_src.display()
        );
    }

    println!(
        "cargo:rustc-env=LIBKRUN_WINDOWS_INIT_BINARY={}",
        wrapper_bin.display()
    );
}
