use std::borrow::Cow;

use encoding_rs::{Encoding, UTF_8};
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
    /// One subcomponent of a component.
    Subcomponent,
    /// A value that is never split, such as MSH-1 and MSH-2.
    Atomic,
}

/// A borrowed, still-escaped value inside a message.
///
/// Navigation follows HL7 conventions: asking a field for its components
/// uses its first repetition, and asking a field or repetition for its
/// subcomponents uses its first component.
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

    /// Whether the value is empty (not valued).
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Whether the value is the explicit HL7 null `""`, which instructs the
    /// receiver to delete the stored value.
    pub fn is_null(&self) -> bool {
        self.raw == b"\"\""
    }

    /// The value with escape sequences resolved, still in the message's
    /// character set.
    pub fn unescaped(&self) -> Cow<'a, [u8]> {
        unescape(self.raw, self.delimiters)
    }

    /// Resolves escape sequences and decodes the bytes with `encoding`,
    /// replacing malformed sequences with U+FFFD.
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
        self.to_text(UTF_8)
    }

    /// The repetitions of a field. Other levels yield themselves once.
    pub fn repetitions(&self) -> Parts<'a> {
        match self.level {
            Level::Field => self.split(Some(self.delimiters.repetition), Level::Repetition),
            _ => self.split(None, self.level),
        }
    }

    /// The components of a repetition. A field uses its first repetition;
    /// components and subcomponents yield themselves once.
    pub fn components(&self) -> Parts<'a> {
        match self.level {
            Level::Field => self.first(Self::repetitions).components(),
            Level::Repetition => self.split(Some(self.delimiters.component), Level::Component),
            _ => self.split(None, self.level),
        }
    }

    /// The subcomponents of a component. Fields and repetitions use their
    /// first component; subcomponents yield themselves once.
    pub fn subcomponents(&self) -> Parts<'a> {
        match self.level {
            Level::Field | Level::Repetition => self.first(Self::components).subcomponents(),
            Level::Component => self.split(self.delimiters.subcomponent, Level::Subcomponent),
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

    /// The 1-based subcomponent `index`, if present.
    pub fn subcomponent(&self, index: usize) -> Option<Value<'a>> {
        index
            .checked_sub(1)
            .and_then(|i| self.subcomponents().nth(i))
    }

    fn split(&self, delimiter: Option<u8>, child: Level) -> Parts<'a> {
        Parts {
            remaining: Some(self.raw),
            delimiter,
            child,
            delimiters: self.delimiters,
        }
    }

    fn first(&self, parts: fn(&Self) -> Parts<'a>) -> Value<'a> {
        parts(self).next().unwrap_or(*self)
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
        component: b'^',
        repetition: b'~',
        escape: Some(b'\\'),
        subcomponent: Some(b'&'),
        truncation: None,
    };

    fn field(raw: &[u8]) -> Value<'_> {
        Value::new(raw, &D, Level::Field)
    }

    #[test]
    fn navigates_levels() {
        let v = field(b"a1&a2^b~c^d1&d2");
        let reps: Vec<_> = v.repetitions().map(|r| r.raw()).collect();
        assert_eq!(reps, [&b"a1&a2^b"[..], b"c^d1&d2"]);
        assert_eq!(v.component(2).unwrap(), "b");
        assert_eq!(
            v.repetition(2)
                .unwrap()
                .component(2)
                .unwrap()
                .subcomponent(2)
                .unwrap(),
            "d2"
        );
        assert_eq!(v.subcomponent(2).unwrap(), "a2");
        assert!(v.repetition(3).is_none());
        assert!(v.component(0).is_none());
    }

    #[test]
    fn empty_value_has_one_empty_part() {
        let v = field(b"");
        assert!(v.is_empty());
        assert_eq!(v.components().count(), 1);
        assert!(v.component(1).unwrap().is_empty());
        assert!(v.component(2).is_none());
    }

    #[test]
    fn keeps_trailing_empty_parts() {
        let v = field(b"a^^");
        assert_eq!(v.components().count(), 3);
    }

    #[test]
    fn atomic_values_are_never_split() {
        let v = Value::new(b"^~\\&", &D, Level::Atomic);
        assert_eq!(v.component(1).unwrap(), "^~\\&");
        assert!(v.component(2).is_none());
        assert_eq!(v.repetitions().count(), 1);
    }

    #[test]
    fn decodes_text() {
        assert_eq!(field(b"Doe\\S\\Jr").to_string_lossy(), "Doe^Jr");
        let latin5 = field(&[0xDE, 0x65, 0x6E, 0x6C, 0x69, 0x6B]);
        assert_eq!(latin5.to_text(WINDOWS_1254), "Şenlik");
        assert!(field(b"\"\"").is_null());
    }
}
