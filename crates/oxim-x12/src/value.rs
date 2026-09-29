//! Borrowed views of element values.

use std::borrow::Cow;
use std::fmt;

use crate::delimiters::Delimiters;

/// How far a value has been split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    /// A whole element, possibly with repetitions and components.
    Element,
    /// One repetition of an element.
    Repetition,
    /// One component; it has no further structure.
    Component,
    /// An atomic value such as a segment identifier.
    Atomic,
}

/// A borrowed element, repetition or component value.
///
/// X12 has no escape mechanism, so the raw bytes are the text.
#[derive(Clone, Copy)]
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

    /// The bytes of the value, including nested delimiters.
    pub fn raw(&self) -> &'a [u8] {
        self.raw
    }

    /// How far the value has been split.
    pub fn level(&self) -> Level {
        self.level
    }

    /// Whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// The value as text; bytes that are not UTF-8 are replaced.
    pub fn to_string_lossy(&self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.raw)
    }

    /// The repetitions of an element. Other values, and elements of
    /// interchanges without a repetition separator, have one repetition.
    pub fn repetitions(&self) -> impl Iterator<Item = Value<'a>> + 'a {
        let delimiters = self.delimiters;
        let split: Box<dyn Iterator<Item = &'a [u8]>> = match (self.level, delimiters.repetition) {
            (Level::Element, Some(separator)) => Box::new(self.raw.split(move |&b| b == separator)),
            _ => Box::new(std::iter::once(self.raw)),
        };
        split.map(move |raw| Value::new(raw, delimiters, Level::Repetition))
    }

    /// The components of an element (of its first repetition) or of a
    /// repetition. A component has itself as its only component.
    pub fn components(&self) -> impl Iterator<Item = Value<'a>> + 'a {
        let delimiters = self.delimiters;
        let base = match self.level {
            Level::Element => self.repetition(1).map_or(self.raw, |r| r.raw),
            _ => self.raw,
        };
        let split: Box<dyn Iterator<Item = &'a [u8]>> = match self.level {
            Level::Element | Level::Repetition => {
                Box::new(base.split(move |&b| b == delimiters.component))
            }
            Level::Component | Level::Atomic => Box::new(std::iter::once(base)),
        };
        split.map(move |raw| Value::new(raw, delimiters, Level::Component))
    }

    /// Repetition `n` (1-based).
    pub fn repetition(&self, n: usize) -> Option<Value<'a>> {
        self.repetitions().nth(n.checked_sub(1)?)
    }

    /// Component `n` (1-based).
    pub fn component(&self, n: usize) -> Option<Value<'a>> {
        self.components().nth(n.checked_sub(1)?)
    }
}

impl fmt::Debug for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Value")
            .field("raw", &self.to_string_lossy())
            .field("level", &self.level)
            .finish()
    }
}

impl fmt::Display for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

impl PartialEq<&str> for Value<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.raw == other.as_bytes()
    }
}

impl PartialEq<str> for Value<'_> {
    fn eq(&self, other: &str) -> bool {
        self.raw == other.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_repetitions_and_components() {
        let d = Delimiters::default();
        let v = Value::new(b"ABK:I10^ABF:E119", &d, Level::Element);
        assert_eq!(v.repetitions().count(), 2);
        assert_eq!(v.component(2).unwrap(), "I10");
        assert_eq!(v.repetition(2).unwrap().component(2).unwrap(), "E119");
        assert!(v.repetition(3).is_none());
        assert!(v.component(0).is_none());
        let old = Delimiters {
            repetition: None,
            ..d
        };
        let v = Value::new(b"A^B:C", &old, Level::Element);
        assert_eq!(v.repetitions().count(), 1);
        assert_eq!(v.component(2).unwrap(), "C");
        let atomic = Value::new(b"A:B", &d, Level::Atomic);
        assert_eq!(atomic.components().count(), 1);
    }
}
