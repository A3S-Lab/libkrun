// a3s-box/build.rs
//
// Copies krun.dll next to the final binary so the exe can find it at runtime.
// krun-sys-windows exposes the DLL directory via the DEP_KRUN_DLL_DIR
// metadata key (set in its own build.rs via `cargo:dll_dir=...`).

fn main() {
    #[cfg(windows)]
    copy_krun_dll();
}

#[cfg(windows)]
fn copy_krun_dll() {
    use std::path::PathBuf;

    let dll_dir = match std::env::var("DEP_KRUN_DLL_DIR") {
        Ok(d) => PathBuf::from(d),
        // DEP_KRUN_DLL_DIR is only set when a3s-libkrun-sys is in the
        // dependency tree.  If it is absent the crate is being built for a
        // non-Windows target or without the Windows feature — do nothing.
        Err(_) => return,
    };

    let src = dll_dir.join("krun.dll");
    if !src.exists() {
        println!(
            "cargo:warning=krun.dll not found at {}; \
             copy it manually next to the built binary",
            src.display()
        );
        return;
    }

    // OUT_DIR is  .../target/<profile>/build/<crate>-<hash>/out
    // The binary lives three levels up from OUT_DIR.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bin_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("unexpected OUT_DIR depth")
        .to_path_buf();

    let dst = bin_dir.join("krun.dll");
    std::fs::copy(&src, &dst).unwrap_or_else(|e| panic!("failed to copy krun.dll: {e}"));
    println!(
        "cargo:warning=copied {} -> {}",
        src.display(),
        dst.display()
    );

    println!("cargo:rerun-if-changed={}", src.display());
}
