use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const RANDOM_BITS: u32 = 80;
const RANDOM_MASK: u128 = (1 << RANDOM_BITS) - 1;
const TIMESTAMP_MASK: u128 = (1 << 48) - 1;

/// A message identifier: a [ULID](https://github.com/ulid/spec).
///
/// The first 48 bits hold the creation time in Unix milliseconds and the
/// remaining 80 bits are random, so identifiers sort by creation time. The
/// text form is 26 Crockford base32 characters.
///
/// This type never reads the clock or a random source; the engine supplies
/// both, usually through [`MessageIdGenerator`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u128);

impl MessageId {
    /// Builds an identifier from a Unix millisecond timestamp (the lower 48
    /// bits are used) and random bits (the lower 80 bits are used).
    pub const fn from_parts(timestamp_ms: u64, random: u128) -> Self {
        Self(((timestamp_ms as u128 & TIMESTAMP_MASK) << RANDOM_BITS) | (random & RANDOM_MASK))
    }

    /// The identifier as a 128-bit integer.
    pub const fn to_u128(self) -> u128 {
        self.0
    }

    /// An identifier from a 128-bit integer.
    pub const fn from_u128(value: u128) -> Self {
        Self(value)
    }

    /// The creation time in Unix milliseconds.
    pub const fn timestamp_ms(self) -> u64 {
        (self.0 >> RANDOM_BITS) as u64
    }

    /// The random part.
    pub const fn random(self) -> u128 {
        self.0 & RANDOM_MASK
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = [0u8; 26];
        for (i, slot) in text.iter_mut().enumerate() {
            let shift = 125 - 5 * i as u32;
            *slot = CROCKFORD[((self.0 >> shift) & 31) as usize];
        }
        // All bytes come from the ASCII alphabet above.
        f.write_str(std::str::from_utf8(&text).map_err(|_| fmt::Error)?)
    }
}

impl fmt::Debug for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageId({self})")
    }
}

/// Returned when text is not a valid ULID.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid message identifier {0:?}")]
pub struct InvalidMessageId(pub String);

impl FromStr for MessageId {
    type Err = InvalidMessageId;

    fn from_str(s: &str) -> Result<Self, InvalidMessageId> {
        let invalid = || InvalidMessageId(s.to_owned());
        let bytes = s.as_bytes();
        if bytes.len() != 26 {
            return Err(invalid());
        }
        let mut value: u128 = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let digit = CROCKFORD
                .iter()
                .position(|&c| c == b.to_ascii_uppercase())
                .ok_or_else(invalid)? as u128;
            // The first character carries only the top three bits.
            if i == 0 && digit > 7 {
                return Err(invalid());
            }
            value = (value << 5) | digit;
        }
        Ok(Self(value))
    }
}

impl Serialize for MessageId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MessageId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Generates strictly increasing [`MessageId`]s.
///
/// When several identifiers are created within the same millisecond, or the
/// clock moves backwards, the previous identifier is incremented instead of
/// using the new random bits, so ordering is preserved.
#[derive(Debug, Clone, Default)]
pub struct MessageIdGenerator {
    last: Option<MessageId>,
}

impl MessageIdGenerator {
    /// Creates a generator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the next identifier for the current time `timestamp_ms` and
    /// fresh `random` bits.
    pub fn next(&mut self, timestamp_ms: u64, random: u128) -> MessageId {
        let candidate = MessageId::from_parts(timestamp_ms, random);
        let id = match self.last {
            Some(last) if candidate <= last => MessageId(last.0.wrapping_add(1)),
            _ => candidate,
        };
        self.last = Some(id);
        id
    }
}

/// Returned when a name is not a valid identifier.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "invalid {kind} {value:?}: use 1 to 64 lowercase ASCII letters, digits, '-' or '_', starting with a letter or digit"
)]
pub struct InvalidName {
    /// What kind of name was rejected, for example `channel identifier`.
    pub kind: &'static str,
    /// The rejected text.
    pub value: String,
}

fn check_name(kind: &'static str, value: String) -> Result<String, InvalidName> {
    let valid = (1..=64).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if valid {
        Ok(value)
    } else {
        Err(InvalidName { kind, value })
    }
}

macro_rules! name_type {
    ($(#[$doc:meta])* $name:ident, $kind:literal) => {
        $(#[$doc])*
        ///
        /// Names are 1 to 64 lowercase ASCII letters, digits, `-` or `_`,
        /// starting with a letter or digit, so they are safe in file names,
        /// URLs and metric labels.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validates and wraps a name.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidName> {
                check_name($kind, value.into()).map(Self)
            }

            /// The name as text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidName;

            fn try_from(value: String) -> Result<Self, InvalidName> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidName;

            fn from_str(s: &str) -> Result<Self, InvalidName> {
                Self::new(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// Identifier of a channel.
    ChannelId,
    "channel identifier"
);
name_type!(
    /// Identifier of a source or destination connector within a channel.
    ConnectorId,
    "connector identifier"
);
name_type!(
    /// Identifier of a registered device.
    DeviceId,
    "device identifier"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_ulids() {
        let id = MessageId::from_parts(1_469_918_176_385, 0x3DD8_A0E0_FB3C_1FB1_E7E4);
        let text = id.to_string();
        assert_eq!(text.len(), 26);
        assert_eq!(text.parse::<MessageId>().unwrap(), id);
        assert_eq!(text.to_lowercase().parse::<MessageId>().unwrap(), id);
        assert_eq!(id.timestamp_ms(), 1_469_918_176_385);
        assert_eq!(
            MessageId::from_u128(0).to_string(),
            "00000000000000000000000000"
        );
        assert_eq!(
            MessageId::from_u128(u128::MAX).to_string(),
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZ"
        );
    }

    #[test]
    fn rejects_invalid_ulids() {
        for text in [
            "",
            "0123",
            "80000000000000000000000000",
            "0000000000000000000000000U",
            "0000000000000000000000000!",
        ] {
            assert!(text.parse::<MessageId>().is_err(), "{text}");
        }
    }

    #[test]
    fn generator_is_monotonic() {
        let mut generator = MessageIdGenerator::new();
        let a = generator.next(1000, 50);
        let b = generator.next(1000, 10);
        let c = generator.next(999, 99);
        let d = generator.next(1001, 0);
        assert!(a < b && b < c && c < d);
        assert_eq!(b.to_u128(), a.to_u128() + 1);
        assert_eq!(d.timestamp_ms(), 1001);
    }

    #[test]
    fn validates_names() {
        assert!(ChannelId::new("lab-chemistry_1").is_ok());
        for bad in ["", "Lab", "-lab", "lab chem", "lab/chem", &"a".repeat(65)] {
            assert!(ChannelId::new(bad).is_err(), "{bad}");
        }
        let json = serde_json::to_string(&ChannelId::new("lab").unwrap()).unwrap();
        assert_eq!(json, "\"lab\"");
        assert!(serde_json::from_str::<ChannelId>("\"Bad Name\"").is_err());
    }

    #[test]
    fn serializes_ids_as_text() {
        let id = MessageId::from_parts(1, 2);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<MessageId>(&json).unwrap(), id);
    }
}
