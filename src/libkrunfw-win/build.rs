use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::env;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[path = "ikconfig.rs"]
mod ikconfig;
#[path = "kernel_source.rs"]
mod kernel_source;

use ikconfig::{compressed_payload, read_bounded_utf8, MAX_IKCONFIG_BYTES};
use kernel_source::{detect_kernel_source, validate_pinned_raw_bundle, KernelSource};

const REQUIRED_CONFIGS: &[&str] = &[
    "CONFIG_VIRTIO_MMIO=y",
    "CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y",
    "CONFIG_X86_MPPARSE=y",
];
const MAX_KERNEL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const IMAGE_OVERRIDE_ENV: &str = "LIBKRUNFW_KERNEL_IMAGE";
const METADATA_OVERRIDE_ENV: &str = "LIBKRUNFW_KERNEL_METADATA";

struct KernelInput {
    image: PathBuf,
    metadata: Option<PathBuf>,
}

fn extract_embedded_config(kernel: &[u8]) -> Result<Option<String>, String> {
    let Some(payload) = compressed_payload(kernel)? else {
        return Ok(None);
    };
    read_bounded_utf8(GzDecoder::new(payload), MAX_IKCONFIG_BYTES).map(Some)
}

fn validate_kernel_config(kernel: &[u8], display_name: &str) {
    let Some(config) = extract_embedded_config(kernel)
        .unwrap_or_else(|error| panic!("{display_name} has invalid embedded IKCONFIG: {error}"))
    else {
        println!(
            "cargo:warning={display_name} does not embed IKCONFIG; cannot verify Windows guest compatibility automatically"
        );
        return;
    };

    let missing: Vec<_> = REQUIRED_CONFIGS
        .iter()
        .copied()
        .filter(|required| !config.lines().any(|line| line.trim() == *required))
        .collect();

    if !missing.is_empty() {
        panic!(
            "{display_name} is not compatible with libkrun WHPX x86_64 MMIO discovery. Missing: {}. \
             Rebuild or replace the bundled kernel with one that enables these options. \
             See src/libkrunfw-win/VMLINUX_SETUP.md.",
            missing.join(", ")
        );
    }
}

fn path_is_present(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("cannot inspect {}: {error}", path.display()),
    }
}

fn open_regular_file_no_follow(path: &Path, label: &str) -> File {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT

    let file = options
        .open(path)
        .unwrap_or_else(|error| panic!("cannot open {label} {}: {error}", path.display()));
    let metadata = file.metadata().unwrap_or_else(|error| {
        panic!("cannot inspect opened {label} {}: {error}", path.display())
    });
    #[cfg(windows)]
    if metadata.file_attributes() & 0x0000_0400 != 0 {
        panic!(
            "{label} must not be a Windows reparse point: {}",
            path.display()
        );
    }
    if !metadata.file_type().is_file() {
        panic!(
            "{label} must be a regular non-symlink file: {}",
            path.display()
        );
    }
    file
}

fn read_bounded_regular_file(path: &Path, maximum: u64, label: &str) -> Vec<u8> {
    let file = open_regular_file_no_follow(path, label);
    let metadata = file.metadata().unwrap_or_else(|error| {
        panic!("cannot inspect opened {label} {}: {error}", path.display())
    });
    if metadata.len() == 0 {
        panic!("{label} is empty: {}", path.display());
    }
    if metadata.len() > maximum {
        panic!(
            "{label} is too large: {} bytes exceeds the {} byte limit ({})",
            metadata.len(),
            maximum,
            path.display()
        );
    }

    let read_limit = maximum
        .checked_add(1)
        .expect("bounded file read limit must fit in u64");
    let mut contents = Vec::with_capacity(
        usize::try_from(metadata.len()).expect("bounded file length must fit in usize"),
    );
    file.take(read_limit)
        .read_to_end(&mut contents)
        .unwrap_or_else(|error| panic!("cannot read {label} {}: {error}", path.display()));
    if contents.len() as u64 > maximum {
        panic!(
            "{label} grew beyond the {maximum} byte limit while being read ({})",
            path.display()
        );
    }
    if contents.len() as u64 != metadata.len() {
        panic!(
            "{label} changed while it was being read: {}",
            path.display()
        );
    }
    contents
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(|value| {
        if value.is_empty() {
            panic!("{name} must not be empty when it is set");
        }
        PathBuf::from(value)
    })
}

fn select_kernel_input(manifest: &Path) -> KernelInput {
    let image_override = env_path(IMAGE_OVERRIDE_ENV);
    let metadata_override = env_path(METADATA_OVERRIDE_ENV);
    if let Some(image) = image_override {
        return KernelInput {
            image,
            metadata: metadata_override,
        };
    }
    if metadata_override.is_some() {
        panic!("{METADATA_OVERRIDE_ENV} requires {IMAGE_OVERRIDE_ENV}");
    }

    let elf = manifest.join("kernel/vmlinux");
    let raw = manifest.join("kernel/kernel.bundle");
    let raw_metadata = manifest.join("kernel/kernel.bundle.metadata");
    let elf_present = path_is_present(&elf);
    let raw_present = path_is_present(&raw);
    let metadata_present = path_is_present(&raw_metadata);

    if elf_present && (raw_present || metadata_present) {
        panic!(
            "ambiguous kernel inputs: keep either kernel/vmlinux or the \
             kernel/kernel.bundle + kernel/kernel.bundle.metadata pair, not both"
        );
    }
    if elf_present {
        return KernelInput {
            image: elf,
            metadata: None,
        };
    }
    if raw_present || metadata_present {
        if !raw_present || !metadata_present {
            panic!(
                "raw kernel input is incomplete: kernel/kernel.bundle and \
                 kernel/kernel.bundle.metadata must both exist"
            );
        }
        return KernelInput {
            image: raw,
            metadata: Some(raw_metadata),
        };
    }

    panic!(
        "no kernel input found. Provide an x86_64 ELF at kernel/vmlinux, or run \
         scripts/extract_kernel.sh on Linux to generate kernel/kernel.bundle and \
         kernel/kernel.bundle.metadata. See src/libkrunfw-win/VMLINUX_SETUP.md."
    );
}

fn sha256_hex(contents: &[u8]) -> String {
    let digest = Sha256::digest(contents);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn write_generated_source(out_dir: &Path, source: &KernelSource) {
    let declaration = match source {
        KernelSource::Elf(validated) => {
            let mut declaration = String::new();
            writeln!(
                &mut declaration,
                "const ELF_LOAD_SEGMENTS: &[EmbeddedLoadSegment] = &["
            )
            .expect("writing generated Rust to String cannot fail");
            for segment in &validated.segments {
                writeln!(
                    &mut declaration,
                    "    EmbeddedLoadSegment {{ file_offset: {}, file_size: {}, destination_offset: {} }},",
                    segment.file_offset, segment.file_size, segment.destination_offset
                )
                .expect("writing generated Rust to String cannot fail");
            }
            writeln!(&mut declaration, "];\n")
                .expect("writing generated Rust to String cannot fail");
            writeln!(
                &mut declaration,
                "const KERNEL_SOURCE: EmbeddedKernelSource = EmbeddedKernelSource::Elf {{ \
                 guest_load_addr: 0x{:016x}, entry_addr: 0x{:016x}, image_size: {}, \
                 segments: ELF_LOAD_SEGMENTS }};",
                validated.guest_load_addr, validated.entry_addr, validated.image_size
            )
            .expect("writing generated Rust to String cannot fail");
            declaration
        }
        KernelSource::RawBundle(metadata) => format!(
            "const KERNEL_SOURCE: EmbeddedKernelSource = EmbeddedKernelSource::RawBundle {{ \
             guest_load_addr: 0x{:016x}, entry_addr: 0x{:016x} }};\n",
            metadata.guest_load_addr, metadata.entry_addr
        ),
    };
    fs::write(out_dir.join("kernel_source_generated.rs"), declaration)
        .expect("cannot write generated kernel source declaration");
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));

    println!("cargo:rerun-if-env-changed={IMAGE_OVERRIDE_ENV}");
    println!("cargo:rerun-if-env-changed={METADATA_OVERRIDE_ENV}");
    println!("cargo:rerun-if-changed=ikconfig.rs");
    println!("cargo:rerun-if-changed=kernel_source.rs");
    println!("cargo:rerun-if-changed=elf.rs");
    println!("cargo:rerun-if-changed=kernel/vmlinux");
    println!("cargo:rerun-if-changed=kernel/kernel.bundle");
    println!("cargo:rerun-if-changed=kernel/kernel.bundle.metadata");

    let input = select_kernel_input(&manifest);
    println!("cargo:rerun-if-changed={}", input.image.display());
    if let Some(metadata_path) = &input.metadata {
        println!("cargo:rerun-if-changed={}", metadata_path.display());
    }

    let image = read_bounded_regular_file(&input.image, MAX_KERNEL_BYTES, "libkrunfw kernel image");
    let metadata_bytes = input
        .metadata
        .as_deref()
        .map(|path| read_bounded_regular_file(path, MAX_METADATA_BYTES, "raw bundle metadata"));
    let metadata_text = metadata_bytes.as_deref().map(|contents| {
        std::str::from_utf8(contents)
            .unwrap_or_else(|_| panic!("raw bundle metadata must be UTF-8"))
    });
    let actual_sha256 = sha256_hex(&image);
    let source = detect_kernel_source(&image, metadata_text, &actual_sha256)
        .unwrap_or_else(|error| panic!("invalid libkrunfw kernel input: {error}"));
    if let KernelSource::RawBundle(metadata) = &source {
        validate_pinned_raw_bundle(metadata)
            .unwrap_or_else(|error| panic!("invalid pinned libkrunfw raw bundle: {error}"));
    }

    validate_kernel_config(&image, &input.image.display().to_string());
    write_generated_source(&out_dir, &source);

    // rustc must embed the exact bytes validated above, even if the source file
    // is replaced concurrently after this build script returns.
    let validated_image = out_dir.join("validated-kernel-image.bin");
    fs::write(&validated_image, &image).expect("cannot stage the validated kernel image");
    let validated_image = validated_image
        .to_str()
        .expect("kernel image path must be valid UTF-8 for rustc");
    println!("cargo:rustc-env=LIBKRUNFW_EMBEDDED_KERNEL_PATH={validated_image}");

    let format = match source {
        KernelSource::Elf(_) => "validated ET_EXEC ELF PT_LOAD image",
        KernelSource::RawBundle(_) => "validated libkrunfw raw bundle",
    };
    println!(
        "cargo:warning=Embedding {format}: {} bytes ({:.1} MB), sha256={actual_sha256}",
        image.len(),
        image.len() as f64 / 1_048_576.0
    );
}
