use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build/windows_init_wrapper.rs");
    println!("cargo:rerun-if-changed=../../init/init");
    println!("cargo:rerun-if-changed=../../init/init.codex-backup");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("missing OUT_DIR"));
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let wrapper_src = manifest_dir.join("build").join("windows_init_wrapper.rs");
    let init_dir = manifest_dir.join("..").join("..").join("init");
    let wrapped_init_backup = init_dir.join("init.codex-backup");
    let wrapped_init_default = init_dir.join("init");
    let wrapped_init = if wrapped_init_backup.is_file() {
        wrapped_init_backup
    } else {
        wrapped_init_default
    };
    let wrapper_bin = out_dir.join("init.krun");

    let status = Command::new(&rustc)
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
