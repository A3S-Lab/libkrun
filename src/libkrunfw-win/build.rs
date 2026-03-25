use flate2::read::GzDecoder;
use std::io::Read;
use std::path::PathBuf;

const IKCFG_START: &[u8] = b"IKCFG_ST";
const IKCFG_END: &[u8] = b"IKCFG_ED";
const REQUIRED_CONFIGS: &[&str] = &[
    "CONFIG_VIRTIO_MMIO=y",
    "CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y",
    "CONFIG_X86_MPPARSE=y",
];

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn extract_embedded_config(vmlinux: &[u8]) -> Option<String> {
    let start = find_subslice(vmlinux, IKCFG_START)?;
    let config_start = start + IKCFG_START.len();
    let config_end = find_subslice(&vmlinux[config_start..], IKCFG_END)? + config_start;

    let mut decoder = GzDecoder::new(&vmlinux[config_start..config_end]);
    let mut config = String::new();
    decoder.read_to_string(&mut config).ok()?;
    Some(config)
}

fn validate_kernel_config(vmlinux_path: &PathBuf) {
    let vmlinux = std::fs::read(vmlinux_path).expect("cannot read kernel/vmlinux");
    let Some(config) = extract_embedded_config(&vmlinux) else {
        println!(
            "cargo:warning=kernel/vmlinux does not embed IKCONFIG; cannot verify Windows guest compatibility automatically"
        );
        return;
    };

    let mut missing = Vec::new();
    for required in REQUIRED_CONFIGS {
        if !config.lines().any(|line| line.trim() == *required) {
            missing.push(*required);
        }
    }

    if !missing.is_empty() {
        panic!(
            "kernel/vmlinux is not compatible with libkrun WHPX x86_64 MMIO discovery. Missing: {}. \
             Rebuild or replace the bundled kernel with one that enables these options.",
            missing.join(", ")
        );
    }
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let vmlinux = manifest.join("kernel/vmlinux");

    println!("cargo:rerun-if-changed=kernel/vmlinux");

    if !vmlinux.exists() {
        panic!(
            "kernel/vmlinux not found. Place a Windows-capable x86_64 ELF vmlinux in \
             src/libkrunfw-win/kernel/ before building libkrunfw-windows."
        );
    }

    let size = std::fs::metadata(&vmlinux)
        .expect("cannot stat kernel/vmlinux")
        .len();
    if size == 0 {
        panic!("kernel/vmlinux is empty");
    }

    validate_kernel_config(&vmlinux);

    println!(
        "cargo:warning=Embedding kernel ELF: {} bytes ({:.1} MB)",
        size,
        size as f64 / 1_048_576.0
    );
}
