//! Deterministic identifiers, so retried deliveries produce the same
//! bundle and conditional creates do not duplicate resources.

use oxim_model::MessageId;

const FNV_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013B;

/// A UUID (version 8, name-based) derived from the message id and a key
/// naming the entry, for example `observation-0-2`. The same inputs always
/// give the same UUID.
pub fn entry_uuid(message: MessageId, key: &str) -> String {
    let mut hash = FNV_OFFSET;
    for byte in message.to_u128().to_be_bytes().iter().chain(key.as_bytes()) {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let mut bytes = hash.to_be_bytes();
    // RFC 9562: version 8 (custom), variant 10.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The `urn:uuid:` full URL of an entry.
pub fn entry_url(message: MessageId, key: &str) -> String {
    format!("urn:uuid:{}", entry_uuid(message, key))
}

/// Percent-encodes a search parameter value, keeping characters that are
/// unambiguous in a query string.
pub(crate) fn query_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A token search value: `system|value`, or `value` without a system.
pub(crate) fn token(system: Option<&str>, value: &str) -> String {
    match system {
        Some(system) => format!("{}|{}", query_escape(system), query_escape(value)),
        None => query_escape(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_stable_and_distinct() {
        let message = MessageId::from_parts(1_790_000_000_000, 42);
        let a = entry_uuid(message, "observation-0-0");
        assert_eq!(a, entry_uuid(message, "observation-0-0"));
        assert_ne!(a, entry_uuid(message, "observation-0-1"));
        assert_ne!(
            a,
            entry_uuid(MessageId::from_parts(1, 1), "observation-0-0")
        );
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "8");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn escapes_search_values() {
        assert_eq!(
            token(Some("urn:oxim:obs"), "A 1|x&y"),
            "urn:oxim:obs|A%201%7Cx%26y"
        );
        assert_eq!(token(None, "S123"), "S123");
    }
}
