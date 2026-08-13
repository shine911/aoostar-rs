// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! Payload encoding/decoding: the `label: value` sensor format.
//!
//! The payload inside each shared-memory slot uses exactly the same format as
//! the legacy `cfg/sensors/*.txt` files, so panel configs, sensor mappings
//! and the renderer keep working without changes:
//!
//! - one `key: value` pair per line
//! - empty lines and lines starting with `#` are skipped
//! - keys and values are trimmed

use std::collections::HashMap;

/// Parses a `label: value` payload into a map of sensor values.
///
/// Invalid lines (no `:` separator) are skipped. The payload is read as
/// UTF-8 lossily; producers should write UTF-8 (values are ASCII in
/// practice).
pub fn parse_key_value(payload: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(payload);
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_value_payload() {
        let map = parse_key_value(b"cpu_temperature: 45.5\n gpu_core: 12 \n");
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("cpu_temperature").unwrap(), "45.5");
        assert_eq!(map.get("gpu_core").unwrap(), "12");
    }

    #[test]
    fn skips_empty_comments_and_invalid_lines() {
        let map = parse_key_value(b"\n# comment\nvalid: 1\nno separator here\n  \nvalid2:2\n");
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("valid").unwrap(), "1");
        assert_eq!(map.get("valid2").unwrap(), "2");
    }

    #[test]
    fn handles_truncated_utf8_lossily() {
        let map = parse_key_value(b"key: \xFF\xFE\n");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn empty_payload_gives_empty_map() {
        assert!(parse_key_value(b"").is_empty());
    }
}
