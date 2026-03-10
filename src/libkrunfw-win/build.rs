use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let vmlinux = manifest.join("kernel/vmlinux");
    let addrs = manifest.join("kernel/addrs.rs");

    // Rebuild the DLL whenever these files change
    println!("cargo:rerun-if-changed=kernel/vmlinux");
    println!("cargo:rerun-if-changed=kernel/addrs.rs");

    if !vmlinux.exists() {
        panic!(
            "\n\
            ╔══════════════════════════════════════════════════════════════╗\n\
            ║  kernel/vmlinux not found.                                   ║\n\
            ║                                                              ║\n\
            ║  Run the extraction script first (requires Linux/macOS):     ║\n\
            ║    bash scripts/extract_kernel.sh                            ║\n\
            ║                                                              ║\n\
            ║  This downloads libkrunfw and extracts the embedded kernel.  ║\n\
            ╚══════════════════════════════════════════════════════════════╝"
        );
    }

    if !addrs.exists() {
        panic!(
            "\n\
            ╔══════════════════════════════════════════════════════════════╗\n\
            ║  kernel/addrs.rs not found.                                  ║\n\
            ║                                                              ║\n\
            ║  Run the extraction script to generate it:                   ║\n\
            ║    bash scripts/extract_kernel.sh                            ║\n\
            ╚══════════════════════════════════════════════════════════════╝"
        );
    }

    let size = std::fs::metadata(&vmlinux)
        .expect("cannot stat kernel/vmlinux")
        .len();
    if size == 0 {
        panic!("kernel/vmlinux is empty — re-run scripts/extract_kernel.sh");
    }

    println!("cargo:warning=Embedding kernel: {} bytes ({:.1} MB)", size, size as f64 / 1_048_576.0);
}
