use std::borrow::Cow;

use encoding_rs::Encoding;
use memchr::memchr;

use crate::delimiters::Delimiters;
use crate::escape::unescape;

/// The structural level a [`Value`] was taken from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    /// A whole field, possibly containing repetitions.
    Field,
    /// One repetition of a field.
    Repetition,
    /// One component of a repetition.
    Component,
    /// A value that is never split, such as the header's delimiter
    /// definition.
    Atomic,
}

/// A borrowed, still-escaped value inside a message.
///
/// Asking a field for its components uses its first repetition.
#[derive(Debug, Clone, Copy)]
pub struct Value<'a> {
    raw: &'a [u8],
    delimiters: &'a Delimiters,
    level: Level,
}

impl<'a> Value<'a> {
    pub(crate) fn new(raw: &'a [u8], delimiters: &'a Delimiters, level: Level) -> Self {
        Self {
            raw,
            delimiters,
            level,
        }
    }

    /// The raw bytes, exactly as they appear in the message (still escaped).
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }

    /// The structural level of this value.
    pub fn level(&self) -> Level {
        self.level
    }

    /// Whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// The value with escape sequences resolved, still in the sender's
    /// character encoding.
    pub fn unescaped(&self) -> Cow<'a, [u8]> {
        unescape(self.raw, self.delimiters)
    }

    /// Resolves escape sequences and decodes the bytes with `encoding`,
    /// replacing malformed sequences with U+FFFD.
    ///
    /// ASTM E1394 has no standard character set declaration, so the encoding
    /// must come from the device profile or channel configuration.
    pub fn to_text(&self, encoding: &'static Encoding) -> Cow<'a, str> {
        match self.unescaped() {
            Cow::Borrowed(bytes) => encoding.decode_without_bom_handling(bytes).0,
            Cow::Owned(bytes) => {
                Cow::Owned(encoding.decode_without_bom_handling(&bytes).0.into_owned())
            }
        }
    }

    /// Resolves escape sequences and decodes the bytes as UTF-8, replacing
    /// malformed sequences with U+FFFD.
    pub fn to_string_lossy(&self) -> Cow<'a, str> {
        self.to_text(encoding_rs::UTF_8)
    }

    /// The repetitions of a field. Other levels yield themselves once.
    pub fn repetitions(&self) -> Parts<'a> {
        match self.level {
            Level::Field => self.split(Some(self.delimiters.repeat), Level::Repetition),
            _ => self.split(None, self.level),
        }
    }

    /// The components of a repetition. A field uses its first repetition;
    /// components yield themselves once.
    pub fn components(&self) -> Parts<'a> {
        match self.level {
            Level::Field => self.repetitions().next().unwrap_or(*self).components(),
            Level::Repetition => self.split(Some(self.delimiters.component), Level::Component),
            _ => self.split(None, self.level),
        }
    }

    /// The 1-based repetition `index`, if present.
    pub fn repetition(&self, index: usize) -> Option<Value<'a>> {
        index.checked_sub(1).and_then(|i| self.repetitions().nth(i))
    }

    /// The 1-based component `index`, if present.
    pub fn component(&self, index: usize) -> Option<Value<'a>> {
        index.checked_sub(1).and_then(|i| self.components().nth(i))
    }

    fn split(&self, delimiter: Option<u8>, child: Level) -> Parts<'a> {
        Parts {
            remaining: Some(self.raw),
            delimiter,
            child,
            delimiters: self.delimiters,
        }
    }
}

impl PartialEq<[u8]> for Value<'_> {
    fn eq(&self, other: &[u8]) -> bool {
        self.raw == other
    }
}

impl PartialEq<&[u8]> for Value<'_> {
    fn eq(&self, other: &&[u8]) -> bool {
        self.raw == *other
    }
}

impl PartialEq<str> for Value<'_> {
    fn eq(&self, other: &str) -> bool {
        self.raw == other.as_bytes()
    }
}

impl PartialEq<&str> for Value<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.raw == other.as_bytes()
    }
}

/// Iterator over the parts of a [`Value`] at the next structural level.
#[derive(Debug, Clone)]
pub struct Parts<'a> {
    remaining: Option<&'a [u8]>,
    delimiter: Option<u8>,
    child: Level,
    delimiters: &'a Delimiters,
}

impl<'a> Iterator for Parts<'a> {
    type Item = Value<'a>;

    fn next(&mut self) -> Option<Value<'a>> {
        let rest = self.remaining?;
        let (part, remaining) = match self.delimiter.and_then(|d| memchr(d, rest)) {
            Some(at) => (&rest[..at], Some(&rest[at + 1..])),
            None => (rest, None),
        };
        self.remaining = remaining;
        Some(Value::new(part, self.delimiters, self.child))
    }
}

#[cfg(test)]
mod tests {
    use encoding_rs::WINDOWS_1254;

    use super::*;

    const D: Delimiters = Delimiters {
        field: b'|',
        repeat: b'\\',
        component: b'^',
        escape: b'&',
    };

    fn field(raw: &[u8]) -> Value<'_> {
        Value::new(raw, &D, Level::Field)
    }

    #[test]
    fn navigates_levels() {
        let v = field(b"^^^GLU^1\\^^^HGB^2");
        assert_eq!(v.repetitions().count(), 2);
        assert_eq!(v.component(4).unwrap(), "GLU");
        assert_eq!(v.repetition(2).unwrap().component(4).unwrap(), "HGB");
        assert!(v.repetition(3).is_none());
        assert!(v.component(0).is_none());
    }

    #[test]
    fn atomic_values_are_never_split() {
        let v = Value::new(b"\\^&", &D, Level::Atomic);
        assert_eq!(v.component(1).unwrap(), "\\^&");
        assert!(v.component(2).is_none());
    }

    #[test]
    fn decodes_text() {
        assert_eq!(field(b"a&S&b").to_string_lossy(), "a^b");
        assert_eq!(field(b"\xDEeker").to_text(WINDOWS_1254), "Şeker");
    }
}
