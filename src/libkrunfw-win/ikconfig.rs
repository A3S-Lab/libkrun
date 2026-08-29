use std::io::Read;

const IKCFG_START: &[u8] = b"IKCFG_ST";
const IKCFG_END: &[u8] = b"IKCFG_ED";

pub const MAX_IKCONFIG_BYTES: u64 = 4 * 1024 * 1024;
pub const REQUIRED_WINDOWS_CONFIGS: &[&str] = &[
    "CONFIG_NUMA=y",
    "CONFIG_VIRTIO_MMIO=y",
    "CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y",
    "CONFIG_X86_MPPARSE=y",
];

pub fn missing_required_configs(config: &str) -> Vec<&'static str> {
    REQUIRED_WINDOWS_CONFIGS
        .iter()
        .copied()
        .filter(|required| !config.lines().any(|line| line.trim() == *required))
        .collect()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub fn compressed_payload(kernel: &[u8]) -> Result<Option<&[u8]>, String> {
    let Some(start) = find_subslice(kernel, IKCFG_START) else {
        if find_subslice(kernel, IKCFG_END).is_some() {
            return Err("embedded IKCONFIG has an end marker without a start marker".to_owned());
        }
        return Ok(None);
    };
    let payload_start = start
        .checked_add(IKCFG_START.len())
        .ok_or_else(|| "embedded IKCONFIG start offset overflows usize".to_owned())?;
    let payload_end = find_subslice(&kernel[payload_start..], IKCFG_END)
        .and_then(|relative| payload_start.checked_add(relative))
        .ok_or_else(|| "embedded IKCONFIG start marker has no end marker".to_owned())?;
    Ok(Some(&kernel[payload_start..payload_end]))
}

pub fn read_bounded_utf8<R: Read>(reader: R, maximum: u64) -> Result<String, String> {
    let read_limit = maximum
        .checked_add(1)
        .ok_or_else(|| "embedded IKCONFIG read limit overflows u64".to_owned())?;
    let initial_capacity = usize::try_from(maximum.min(64 * 1024))
        .map_err(|_| "embedded IKCONFIG size limit does not fit in usize".to_owned())?;
    let mut decoded = Vec::with_capacity(initial_capacity);
    reader
        .take(read_limit)
        .read_to_end(&mut decoded)
        .map_err(|error| format!("cannot decompress embedded IKCONFIG: {error}"))?;
    if decoded.len() as u64 > maximum {
        return Err(format!(
            "decompressed embedded IKCONFIG exceeds the {maximum} byte limit"
        ));
    }
    String::from_utf8(decoded)
        .map_err(|_| "decompressed embedded IKCONFIG is not valid UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::InvalidData, "broken gzip"))
        }
    }

    #[test]
    fn distinguishes_absent_and_malformed_markers() {
        assert_eq!(compressed_payload(b"ordinary kernel bytes").unwrap(), None);
        assert!(compressed_payload(b"prefix IKCFG_ST payload")
            .unwrap_err()
            .contains("no end marker"));
        assert!(compressed_payload(b"prefix IKCFG_ED suffix")
            .unwrap_err()
            .contains("without a start marker"));
    }

    #[test]
    fn extracts_only_the_delimited_compressed_payload() {
        assert_eq!(
            compressed_payload(b"before IKCFG_STgzip bytesIKCFG_ED after").unwrap(),
            Some(&b"gzip bytes"[..])
        );
    }

    #[test]
    fn rejects_decompressed_content_over_the_limit() {
        let error = read_bounded_utf8(Cursor::new(vec![b'x'; 17]), 16).unwrap_err();
        assert!(error.contains("exceeds the 16 byte limit"));
    }

    #[test]
    fn rejects_decompression_and_utf8_errors() {
        assert!(read_bounded_utf8(FailingReader, 16)
            .unwrap_err()
            .contains("cannot decompress"));
        assert!(read_bounded_utf8(Cursor::new([0xff]), 16)
            .unwrap_err()
            .contains("not valid UTF-8"));
    }

    #[test]
    fn requires_numa_and_whpx_boot_settings() {
        let config = "\
CONFIG_VIRTIO_MMIO=y\n\
CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y\n\
CONFIG_X86_MPPARSE=y\n";

        assert_eq!(missing_required_configs(config), ["CONFIG_NUMA=y"]);
        assert!(missing_required_configs(&format!("{config}CONFIG_NUMA=y\n")).is_empty());
    }
}
