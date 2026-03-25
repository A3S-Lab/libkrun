// build.rs for a3s-libkrun-sys
//
// Locates krun.lib (MSVC import library) and emits the rustc-link directives.
//
// Search order:
//   1. LIBKRUN_DIR environment variable (CI override or local build directory).
//   2. prebuilt/<triple>/ directory adjacent to this Cargo.toml.
//
// The DLL path is emitted via cargo:dll_dir=<path> so that downstream
// crates (e.g. a3s-box) can copy krun.dll next to the final binary.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        // Nothing to do on non-Windows; the crate's lib.rs is also gated.
        return;
    }

    let target_arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_string());
    // MSVC triple used for the prebuilt subdirectory name.
    let triple = format!("{}-pc-windows-msvc", target_arch);

    // --- Locate krun.lib ---
    let lib_dir = if let Ok(dir) = std::env::var("LIBKRUN_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("prebuilt")
            .join(&triple)
    };

    let krun_lib = {
        let canonical = lib_dir.join("krun.lib");
        if canonical.exists() {
            canonical
        } else {
            lib_dir.join("krun.dll.lib")
        }
    };
    if !krun_lib.exists() {
        // Emit a warning instead of panicking so that `cargo check` and IDE
        // tools work without a pre-built library.  The linker will produce a
        // clear error if krun.lib is genuinely absent during `cargo build`.
        println!(
            "cargo:warning=krun import library not found at {}. \
             Build libkrun first (`cargo build --release -p libkrun \
             --target {triple}`) and either set LIBKRUN_DIR to the output \
             directory or copy krun.lib/krun.dll.lib into krun-sys-windows/prebuilt/{triple}/",
            krun_lib.display(),
            triple = triple,
        );
    }

    // --- Link directives ---
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=krun");
    // krun.dll itself links WinHvPlatform; re-declare it so linker finds it
    // even when a3s-libkrun-sys is used as the sole Windows entry point.
    println!("cargo:rustc-link-lib=WinHvPlatform");

    // --- Expose DLL location to downstream build scripts ---
    // Accessible as DEP_KRUN_DLL_DIR in build scripts of crates that
    // depend on a3s-libkrun-sys.
    println!("cargo:dll_dir={}", lib_dir.display());

    // --- Rerun triggers ---
    println!("cargo:rerun-if-env-changed=LIBKRUN_DIR");
    println!("cargo:rerun-if-changed=prebuilt/{}/krun.lib", triple);
}
