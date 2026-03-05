fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,-soname,libkrun.so.{}",
            std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap()
        );
    }
    if target_os == "macos" {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,-install_name,libkrun.{}.dylib,-compatibility_version,{}.0.0,-current_version,{}.{}.0",
            std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap(), std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
            std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap(), std::env::var("CARGO_PKG_VERSION_MINOR").unwrap()
        );
        println!("cargo:rustc-link-lib=framework=Hypervisor");
    }
    if target_os == "windows" {
        println!("cargo:rustc-link-lib=WinHvPlatform");
    }
}
