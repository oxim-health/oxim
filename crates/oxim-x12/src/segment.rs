//! Segments and their views.

use std::borrow::Cow;

use crate::delimiters::Delimiters;
use crate::error::PathError;
use crate::path::MAX_INDEX;
use crate::value::{Level, Value};

/// Storage for one segment: raw elements (element 0 is the identifier), the
/// terminator flag and the bytes after the terminator (line breaks).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Segment {
    pub(crate) elements: Vec<Vec<u8>>,
    pub(crate) terminated: bool,
    pub(crate) suffix: Vec<u8>,
}

impl Segment {
    pub(crate) fn id(&self) -> &[u8] {
        self.elements.first().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn write_to(&self, delimiters: &Delimiters, out: &mut Vec<u8>) {
        for (i, element) in self.elements.iter().enumerate() {
            if i > 0 {
                out.push(delimiters.element);
            }
            out.extend_from_slice(element);
        }
        if self.terminated {
            out.push(delimiters.segment);
        }
        out.extend_from_slice(&self.suffix);
    }
}

/// A read-only view of one segment.
#[derive(Debug, Clone, Copy)]
pub struct SegmentRef<'a> {
    segment: &'a Segment,
    delimiters: &'a Delimiters,
    index: usize,
}

impl<'a> SegmentRef<'a> {
    pub(crate) fn new(segment: &'a Segment, delimiters: &'a Delimiters, index: usize) -> Self {
        Self {
            segment,
            delimiters,
            index,
        }
    }

    /// The segment identifier, for example `b"NM1"`.
    pub fn id(&self) -> &'a [u8] {
        self.segment.id()
    }

    /// Whether the segment identifier equals `id`.
    pub fn is(&self, id: &str) -> bool {
        self.segment.id() == id.as_bytes()
    }

    /// The position of the segment in the interchange (0-based).
    pub fn index(&self) -> usize {
        self.index
    }

    /// The position of the last element present (the identifier is 0).
    pub fn element_count(&self) -> usize {
        self.segment.elements.len().saturating_sub(1)
    }

    /// Whether the segment ended with the segment terminator.
    pub fn is_terminated(&self) -> bool {
        self.segment.terminated
    }

    /// Element `n`; element 0 is the identifier.
    pub fn element(&self, n: usize) -> Option<Value<'a>> {
        let raw = self.segment.elements.get(n)?;
        let level = if n == 0 || self.is_delimiter_element(n) {
            Level::Atomic
        } else {
            Level::Element
        };
        Some(Value::new(raw, self.delimiters, level))
    }

    /// The text of element `n`, or `None` when it is absent.
    pub fn text(&self, n: usize) -> Option<Cow<'a, str>> {
        self.element(n).map(|value| value.to_string_lossy())
    }

    /// The text of element `n` without surrounding spaces; empty elements
    /// are `None`. Convenient for the space-padded `ISA` elements.
    pub fn trimmed(&self, n: usize) -> Option<String> {
        self.text(n)
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    fn is_delimiter_element(&self, n: usize) -> bool {
        self.segment.id() == b"ISA"
            && (n == 16 || (n == 11 && self.delimiters.repetition.is_some()))
    }

    /// The segment serialized as it appears in the interchange, including
    /// its terminator and line break.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.segment.write_to(self.delimiters, &mut out);
        out
    }
}

/// A mutable view of one segment.
#[derive(Debug)]
pub struct SegmentMut<'a> {
    segment: &'a mut Segment,
    delimiters: &'a Delimiters,
    index: usize,
}

impl<'a> SegmentMut<'a> {
    pub(crate) fn new(segment: &'a mut Segment, delimiters: &'a Delimiters, index: usize) -> Self {
        Self {
            segment,
            delimiters,
            index,
        }
    }

    /// A read-only view of the segment.
    pub fn view(&self) -> SegmentRef<'_> {
        SegmentRef::new(self.segment, self.delimiters, self.index)
    }

    /// Stores `text` as element `n`. The text must not contain a delimiter.
    pub fn set(&mut self, n: usize, text: &str) -> Result<(), PathError> {
        self.delimiters.check_text(text.as_bytes())?;
        set_raw(
            self.segment,
            self.delimiters,
            n,
            None,
            None,
            text.as_bytes(),
        )
    }

    /// Stores `raw` as element `n`; component and repetition separators
    /// inside `raw` keep their meaning.
    pub fn set_raw(&mut self, n: usize, raw: &[u8]) -> Result<(), PathError> {
        set_raw(self.segment, self.delimiters, n, None, None, raw)
    }
}

/// Writes `raw` at an element, repetition and component, creating missing
/// elements, repetitions and components as empty values.
pub(crate) fn set_raw(
    segment: &mut Segment,
    delimiters: &Delimiters,
    element: usize,
    repetition: Option<usize>,
    component: Option<usize>,
    raw: &[u8],
) -> Result<(), PathError> {
    let is_isa = segment.id() == b"ISA";
    if element == 0
        || (is_isa && (element == 16 || (element == 11 && delimiters.repetition.is_some())))
    {
        return Err(PathError::ReadOnly);
    }
    if element > MAX_INDEX {
        return Err(PathError::Invalid(element.to_string()));
    }
    if let Some(&b) = raw
        .iter()
        .find(|&&b| b == delimiters.element || b == delimiters.segment)
    {
        return Err(PathError::Delimiter(char::from(b)));
    }
    if segment.elements.len() <= element {
        segment.elements.resize(element + 1, Vec::new());
    }
    let current = std::mem::take(&mut segment.elements[element]);
    let replace_component = |repetition: &[u8]| match component {
        None => raw.to_vec(),
        Some(c) => replace_part(repetition, delimiters.component, c, |_| raw.to_vec()),
    };
    segment.elements[element] = match (repetition, delimiters.repetition) {
        (None, _) if component.is_none() => raw.to_vec(),
        (None | Some(1), None) => replace_component(&current),
        (Some(_), None) => {
            segment.elements[element] = current;
            return Err(PathError::Unsupported(
                "the interchange declares no repetition separator",
            ));
        }
        (r, Some(separator)) => {
            replace_part(&current, separator, r.unwrap_or(1), replace_component)
        }
    };
    Ok(())
}

/// Replaces the 1-based part `index` of `raw` split by `separator`, padding
/// with empty parts when `raw` has fewer.
fn replace_part(
    raw: &[u8],
    separator: u8,
    index: usize,
    replacement: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = raw.split(|&b| b == separator).collect();
    if parts.len() < index {
        parts.resize(index, &[]);
    }
    let position = index.saturating_sub(1);
    let new_part = replacement(parts.get(position).copied().unwrap_or_default());
    let mut out = Vec::with_capacity(raw.len() + new_part.len() + index);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(separator);
        }
        if i == position {
            out.extend_from_slice(&new_part);
        } else {
            out.extend_from_slice(part);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &[u8]) -> Segment {
        Segment {
            elements: text.split(|&b| b == b'*').map(<[u8]>::to_vec).collect(),
            terminated: true,
            suffix: Vec::new(),
        }
    }

    fn write(segment: &Segment) -> Vec<u8> {
        let mut out = Vec::new();
        segment.write_to(&Delimiters::default(), &mut out);
        out
    }

    #[test]
    fn sets_elements_repetitions_and_components() {
        let d = Delimiters::default();
        let mut s = segment(b"CLM*A37YH556*21");
        set_raw(&mut s, &d, 5, None, Some(1), b"11").unwrap();
        set_raw(&mut s, &d, 5, None, Some(3), b"1").unwrap();
        assert_eq!(write(&s), b"CLM*A37YH556*21***11::1~");
        set_raw(&mut s, &d, 2, Some(2), Some(2), b"X").unwrap();
        assert_eq!(write(&s), b"CLM*A37YH556*21^:X***11::1~");
        assert_eq!(
            set_raw(&mut s, &d, 0, None, None, b"XYZ"),
            Err(PathError::ReadOnly)
        );
        assert_eq!(
            set_raw(&mut s, &d, 3, None, None, b"A*B"),
            Err(PathError::Delimiter('*'))
        );
    }

    #[test]
    fn protects_isa_delimiter_elements() {
        let d = Delimiters::default();
        let mut isa = segment(
            b"ISA*00*          *00*          *ZZ*A*ZZ*B*260929*1200*^*00501*000000001*0*T*:",
        );
        assert_eq!(
            set_raw(&mut isa, &d, 16, None, None, b">"),
            Err(PathError::ReadOnly)
        );
        assert_eq!(
            set_raw(&mut isa, &d, 11, None, None, b"!"),
            Err(PathError::ReadOnly)
        );
        set_raw(&mut isa, &d, 15, None, None, b"P").unwrap();
        let view = SegmentRef::new(&isa, &d, 0);
        assert_eq!(view.element(15).unwrap(), "P");
        assert_eq!(view.element(16).unwrap().level(), Level::Atomic);
        assert_eq!(view.trimmed(2), None);
        assert_eq!(view.element_count(), 16);
    }

    #[test]
    fn rejects_repetitions_without_a_separator() {
        let d = Delimiters {
            repetition: None,
            ..Delimiters::default()
        };
        let mut s = segment(b"HI*A:B");
        assert!(matches!(
            set_raw(&mut s, &d, 1, Some(2), None, b"C"),
            Err(PathError::Unsupported(_))
        ));
        assert_eq!(write(&s), b"HI*A:B~");
        set_raw(&mut s, &d, 1, Some(1), Some(2), b"C").unwrap();
        assert_eq!(write(&s), b"HI*A:C~");
    }
}
