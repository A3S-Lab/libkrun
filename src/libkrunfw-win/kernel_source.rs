// Shared build-time validation for the Windows libkrunfw kernel input.
//
// This module intentionally uses only the Rust standard library so its unit
// tests can be compiled without a kernel image or the rest of the workspace.

#[path = "elf.rs"]
mod elf;

pub use elf::ValidatedElf;

pub const RAW_BUNDLE_FORMAT: &str = "libkrunfw-raw-bundle-v1";
pub const RAW_BUNDLE_GENERATOR: &str = "scripts/extract_kernel.c";
pub const PINNED_SOURCE_URL: &str =
    "https://github.com/libkrun/libkrunfw/releases/download/v5.5.0/libkrunfw-x86_64.tgz";
pub const PINNED_SOURCE_ARCHIVE_SHA256: &str =
    "c169206b01c89fbe134f1728bf4f988702bc7f73b4cf73e6fdece447d6fceca1";
pub const PINNED_SOURCE_LIBRARY_MEMBER: &str = "lib64/libkrunfw.so.5.5.0";
pub const PINNED_SOURCE_LIBRARY_SHA256: &str =
    "6df51f65d7f99fc22215e69a4236c770b1588ceb6777eca014f92b366517d237";
pub const PINNED_RAW_BUNDLE_GUEST_LOAD_ADDR: u64 = 0x0100_0000;
pub const PINNED_RAW_BUNDLE_ENTRY_ADDR: u64 = 0x0100_0123;
pub const PINNED_RAW_BUNDLE_SIZE: u64 = 21_364_736;
pub const PINNED_RAW_BUNDLE_SHA256: &str =
    "781375ea09f4279ec5bfeab26ecc7067358a3fc98190467e2ab01cc6e98936dd";

const PAGE_SIZE: u64 = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawBundleMetadata {
    pub guest_load_addr: u64,
    pub entry_addr: u64,
    pub bundle_size: u64,
    pub bundle_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KernelSource {
    Elf(ValidatedElf),
    RawBundle(RawBundleMetadata),
}

#[derive(Default)]
struct ParsedFields {
    format: Option<String>,
    generator: Option<String>,
    guest_load_addr: Option<String>,
    entry_addr: Option<String>,
    bundle_size: Option<String>,
    bundle_sha256: Option<String>,
    source_url: Option<String>,
    source_archive_sha256: Option<String>,
    source_library_member: Option<String>,
    source_library_sha256: Option<String>,
}

fn set_once(slot: &mut Option<String>, key: &str, value: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate raw bundle metadata key: {key}"));
    }
    *slot = Some(value.to_owned());
    Ok(())
}

fn required<'a>(value: &'a Option<String>, key: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .ok_or_else(|| format!("missing raw bundle metadata key: {key}"))
}

fn parse_hex_u64(value: &str, key: &str) -> Result<u64, String> {
    let digits = value
        .strip_prefix("0x")
        .ok_or_else(|| format!("{key} must use an explicit 0x-prefixed hexadecimal value"))?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{key} is not a valid hexadecimal value"));
    }
    u64::from_str_radix(digits, 16).map_err(|_| format!("{key} does not fit in u64"))
}

fn parse_decimal_u64(value: &str, key: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{key} must be an unsigned decimal integer"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("{key} does not fit in u64"))
}

fn validate_sha256(value: &str, key: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{key} must be a canonical lowercase 64-character SHA-256 digest"
        ));
    }
    Ok(())
}

pub fn parse_raw_bundle_metadata(contents: &str) -> Result<RawBundleMetadata, String> {
    let mut fields = ParsedFields::default();

    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, value) = line.split_once('=').ok_or_else(|| {
            format!(
                "invalid raw bundle metadata line {}: expected key=value",
                index + 1
            )
        })?;
        if key.trim() != key || value.trim() != value || key.is_empty() || value.is_empty() {
            return Err(format!(
                "invalid whitespace or empty value in raw bundle metadata line {}",
                index + 1
            ));
        }

        match key {
            "format" => set_once(&mut fields.format, key, value)?,
            "generator" => set_once(&mut fields.generator, key, value)?,
            "guest_load_addr" => set_once(&mut fields.guest_load_addr, key, value)?,
            "entry_addr" => set_once(&mut fields.entry_addr, key, value)?,
            "bundle_size" => set_once(&mut fields.bundle_size, key, value)?,
            "bundle_sha256" => set_once(&mut fields.bundle_sha256, key, value)?,
            "source_url" => set_once(&mut fields.source_url, key, value)?,
            "source_archive_sha256" => set_once(&mut fields.source_archive_sha256, key, value)?,
            "source_library_member" => set_once(&mut fields.source_library_member, key, value)?,
            "source_library_sha256" => set_once(&mut fields.source_library_sha256, key, value)?,
            _ => return Err(format!("unknown raw bundle metadata key: {key}")),
        }
    }

    if required(&fields.format, "format")? != RAW_BUNDLE_FORMAT {
        return Err(format!(
            "unsupported raw bundle metadata format (expected {RAW_BUNDLE_FORMAT})"
        ));
    }
    if required(&fields.generator, "generator")? != RAW_BUNDLE_GENERATOR {
        return Err(format!(
            "raw bundle metadata was not produced by {RAW_BUNDLE_GENERATOR}"
        ));
    }
    if required(&fields.source_url, "source_url")? != PINNED_SOURCE_URL {
        return Err("raw bundle metadata does not name the pinned source URL".to_owned());
    }
    if required(&fields.source_archive_sha256, "source_archive_sha256")?
        != PINNED_SOURCE_ARCHIVE_SHA256
    {
        return Err(
            "raw bundle metadata does not name the pinned source archive digest".to_owned(),
        );
    }
    if required(&fields.source_library_member, "source_library_member")?
        != PINNED_SOURCE_LIBRARY_MEMBER
    {
        return Err("raw bundle metadata does not name the pinned library member".to_owned());
    }
    if required(&fields.source_library_sha256, "source_library_sha256")?
        != PINNED_SOURCE_LIBRARY_SHA256
    {
        return Err("raw bundle metadata does not name the pinned library digest".to_owned());
    }

    let guest_load_addr = parse_hex_u64(
        required(&fields.guest_load_addr, "guest_load_addr")?,
        "guest_load_addr",
    )?;
    let entry_addr = parse_hex_u64(required(&fields.entry_addr, "entry_addr")?, "entry_addr")?;
    let bundle_size =
        parse_decimal_u64(required(&fields.bundle_size, "bundle_size")?, "bundle_size")?;
    let bundle_sha256 = required(&fields.bundle_sha256, "bundle_sha256")?.to_owned();
    validate_sha256(&bundle_sha256, "bundle_sha256")?;
    validate_sha256(
        required(&fields.source_archive_sha256, "source_archive_sha256")?,
        "source_archive_sha256",
    )?;
    validate_sha256(
        required(&fields.source_library_sha256, "source_library_sha256")?,
        "source_library_sha256",
    )?;

    if guest_load_addr == 0 || guest_load_addr % PAGE_SIZE != 0 {
        return Err("guest_load_addr must be non-zero and 4096-byte aligned".to_owned());
    }
    if entry_addr == 0 {
        return Err("entry_addr must be non-zero".to_owned());
    }
    if bundle_size == 0 || bundle_size % PAGE_SIZE != 0 {
        return Err("bundle_size must be non-zero and 4096-byte aligned".to_owned());
    }
    guest_load_addr
        .checked_add(bundle_size)
        .ok_or_else(|| "raw bundle guest address range overflows u64".to_owned())?;

    Ok(RawBundleMetadata {
        guest_load_addr,
        entry_addr,
        bundle_size,
        bundle_sha256,
    })
}

pub fn detect_kernel_source(
    image: &[u8],
    raw_metadata: Option<&str>,
    actual_sha256: &str,
) -> Result<KernelSource, String> {
    if image.is_empty() {
        return Err("kernel image is empty".to_owned());
    }

    if image.starts_with(b"\x7fELF") {
        if raw_metadata.is_some() {
            return Err("raw bundle metadata must not accompany an ELF kernel".to_owned());
        }
        return elf::validate_elf(image)
            .map(KernelSource::Elf)
            .map_err(|error| format!("invalid ELF kernel: {error}"));
    }

    let metadata_contents = raw_metadata.ok_or_else(|| {
        "non-ELF kernel input is rejected without extractor-generated raw bundle metadata"
            .to_owned()
    })?;
    validate_sha256(actual_sha256, "computed bundle SHA-256")?;
    let metadata = parse_raw_bundle_metadata(metadata_contents)?;
    let actual_size = u64::try_from(image.len())
        .map_err(|_| "raw bundle length does not fit in u64".to_owned())?;
    if metadata.bundle_size != actual_size {
        return Err(format!(
            "raw bundle size mismatch: metadata={}, actual={actual_size}",
            metadata.bundle_size
        ));
    }
    if metadata.bundle_sha256 != actual_sha256 {
        return Err(format!(
            "raw bundle SHA-256 mismatch: metadata={}, actual={actual_sha256}",
            metadata.bundle_sha256
        ));
    }

    Ok(KernelSource::RawBundle(metadata))
}

pub fn validate_pinned_raw_bundle(metadata: &RawBundleMetadata) -> Result<(), String> {
    if metadata.guest_load_addr != PINNED_RAW_BUNDLE_GUEST_LOAD_ADDR {
        return Err(format!(
            "raw bundle guest load address does not match pinned libkrunfw v5.5.0: \
             expected=0x{PINNED_RAW_BUNDLE_GUEST_LOAD_ADDR:x}, actual=0x{:x}",
            metadata.guest_load_addr
        ));
    }
    if metadata.entry_addr != PINNED_RAW_BUNDLE_ENTRY_ADDR {
        return Err(format!(
            "raw bundle entry address does not match pinned libkrunfw v5.5.0: \
             expected=0x{PINNED_RAW_BUNDLE_ENTRY_ADDR:x}, actual=0x{:x}",
            metadata.entry_addr
        ));
    }
    if metadata.bundle_size != PINNED_RAW_BUNDLE_SIZE {
        return Err(format!(
            "raw bundle size does not match pinned libkrunfw v5.5.0: \
             expected={PINNED_RAW_BUNDLE_SIZE}, actual={}",
            metadata.bundle_size
        ));
    }
    if metadata.bundle_sha256 != PINNED_RAW_BUNDLE_SHA256 {
        return Err(format!(
            "raw bundle SHA-256 does not match pinned libkrunfw v5.5.0: \
             expected={PINNED_RAW_BUNDLE_SHA256}, actual={}",
            metadata.bundle_sha256
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn raw_metadata(overrides: &[(&str, &str)]) -> String {
        let mut fields = vec![
            ("format", RAW_BUNDLE_FORMAT),
            ("generator", RAW_BUNDLE_GENERATOR),
            ("guest_load_addr", "0x0000000001000000"),
            ("entry_addr", "0x0000000001000123"),
            ("bundle_size", "4096"),
            ("bundle_sha256", ZERO_SHA256),
            ("source_url", PINNED_SOURCE_URL),
            ("source_archive_sha256", PINNED_SOURCE_ARCHIVE_SHA256),
            ("source_library_member", PINNED_SOURCE_LIBRARY_MEMBER),
            ("source_library_sha256", PINNED_SOURCE_LIBRARY_SHA256),
        ];
        for (override_key, override_value) in overrides {
            let field = fields
                .iter_mut()
                .find(|(key, _)| key == override_key)
                .expect("test override key must exist");
            field.1 = override_value;
        }
        fields
            .into_iter()
            .map(|(key, value)| format!("{key}={value}\n"))
            .collect()
    }

    #[test]
    fn detects_a_small_x86_64_elf_without_metadata() {
        let detected = detect_kernel_source(&elf::valid_test_elf(), None, ZERO_SHA256).unwrap();
        assert!(matches!(detected, KernelSource::Elf(_)));
    }

    #[test]
    fn rejects_metadata_attached_to_an_elf() {
        let error = detect_kernel_source(
            &elf::valid_test_elf(),
            Some(&raw_metadata(&[])),
            ZERO_SHA256,
        )
        .unwrap_err();
        assert!(error.contains("must not accompany an ELF"));
    }

    #[test]
    fn accepts_raw_bundle_only_when_metadata_matches() {
        let raw = vec![0_u8; 4096];
        let source = detect_kernel_source(&raw, Some(&raw_metadata(&[])), ZERO_SHA256).unwrap();
        assert_eq!(
            source,
            KernelSource::RawBundle(RawBundleMetadata {
                guest_load_addr: 0x0100_0000,
                entry_addr: 0x0100_0123,
                bundle_size: 4096,
                bundle_sha256: ZERO_SHA256.to_owned(),
            })
        );
    }

    #[test]
    fn rejects_raw_bytes_without_metadata() {
        let error = detect_kernel_source(&[1_u8; 64], None, ZERO_SHA256).unwrap_err();
        assert!(error.contains("extractor-generated"));
    }

    #[test]
    fn rejects_stale_raw_bundle_size_and_digest() {
        let raw = vec![0_u8; 4096];
        let size_error = detect_kernel_source(
            &raw,
            Some(&raw_metadata(&[("bundle_size", "8192")])),
            ZERO_SHA256,
        )
        .unwrap_err();
        assert!(size_error.contains("size mismatch"));

        let digest_error = detect_kernel_source(
            &raw,
            Some(&raw_metadata(&[(
                "bundle_sha256",
                "1111111111111111111111111111111111111111111111111111111111111111",
            )])),
            ZERO_SHA256,
        )
        .unwrap_err();
        assert!(digest_error.contains("SHA-256 mismatch"));
    }

    #[test]
    fn rejects_truncated_raw_metadata() {
        let complete = raw_metadata(&[]);
        let truncated: String = complete
            .lines()
            .filter(|line| !line.starts_with("entry_addr="))
            .map(|line| format!("{line}\n"))
            .collect();
        let error = parse_raw_bundle_metadata(&truncated).unwrap_err();
        assert!(error.contains("missing raw bundle metadata key: entry_addr"));
    }

    #[test]
    fn rejects_wrong_source_provenance() {
        let error = parse_raw_bundle_metadata(&raw_metadata(&[(
            "source_archive_sha256",
            "1111111111111111111111111111111111111111111111111111111111111111",
        )]))
        .unwrap_err();
        assert!(error.contains("pinned source archive digest"));
    }

    #[test]
    fn validates_the_pinned_raw_bundle_identity_without_loading_kernel_bytes() {
        let pinned = RawBundleMetadata {
            guest_load_addr: PINNED_RAW_BUNDLE_GUEST_LOAD_ADDR,
            entry_addr: PINNED_RAW_BUNDLE_ENTRY_ADDR,
            bundle_size: PINNED_RAW_BUNDLE_SIZE,
            bundle_sha256: PINNED_RAW_BUNDLE_SHA256.to_owned(),
        };
        validate_pinned_raw_bundle(&pinned).unwrap();

        let mut stale = pinned.clone();
        stale.bundle_size -= 4096;
        assert!(validate_pinned_raw_bundle(&stale)
            .unwrap_err()
            .contains("size does not match"));

        stale = pinned;
        stale.entry_addr += 1;
        assert!(validate_pinned_raw_bundle(&stale)
            .unwrap_err()
            .contains("entry address does not match"));
    }

    #[test]
    fn rejects_duplicate_unknown_and_unaligned_metadata() {
        let duplicate = format!("{}bundle_size=4096\n", raw_metadata(&[]));
        assert!(parse_raw_bundle_metadata(&duplicate)
            .unwrap_err()
            .contains("duplicate"));

        let unknown = format!("{}surprise=true\n", raw_metadata(&[]));
        assert!(parse_raw_bundle_metadata(&unknown)
            .unwrap_err()
            .contains("unknown"));

        assert!(
            parse_raw_bundle_metadata(&raw_metadata(&[("guest_load_addr", "0x1000001")]))
                .unwrap_err()
                .contains("aligned")
        );
    }
}
