use crate::delimiters::Delimiters;
use crate::error::PathError;
use crate::escape::escape;
use crate::path::FieldPath;
use crate::value::{Level, Value};

/// How a segment was terminated in the original bytes.
///
/// HL7 v2 terminates segments with a carriage return. Files and some senders
/// use CRLF or LF instead; the original ending is kept so serialization is
/// byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineEnding {
    /// Carriage return (`\r`), the HL7 standard.
    #[default]
    Cr,
    /// Line feed (`\n`).
    Lf,
    /// Carriage return followed by line feed.
    CrLf,
    /// No terminator (only possible for the last segment).
    None,
}

impl LineEnding {
    /// The bytes written after the segment.
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
            Self::None => b"",
        }
    }
}

/// Storage for one segment line. Fields are kept as raw, escaped bytes so
/// that unmodified parts serialize exactly as received.
///
/// `fields[0]` is the segment identifier. For header segments (`MSH`, `FHS`,
/// `BHS`), `fields[1]` holds the encoding characters (field 2) because the
/// field separator itself is field 1.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Segment {
    pub(crate) fields: Vec<Vec<u8>>,
    pub(crate) line_ending: LineEnding,
}

impl Segment {
    pub(crate) fn new(id: &[u8], line_ending: LineEnding) -> Self {
        Self {
            fields: vec![id.to_vec()],
            line_ending,
        }
    }

    pub(crate) fn id(&self) -> &[u8] {
        self.fields.first().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.fields.len() == 1 && self.id().is_empty()
    }

    pub(crate) fn is_header(&self) -> bool {
        is_header_id(self.id())
    }

    /// Index into `fields` holding HL7 field `n`, or `None` for the virtual
    /// field 1 of a header segment.
    fn storage_index(&self, n: usize) -> Option<usize> {
        match (self.is_header(), n) {
            (_, 0) | (true, 1) => None,
            (true, n) => Some(n - 1),
            (false, n) => Some(n),
        }
    }

    fn field_count(&self) -> usize {
        if self.is_header() {
            self.fields.len()
        } else {
            self.fields.len().saturating_sub(1)
        }
    }

    pub(crate) fn write_to(&self, separator: u8, out: &mut Vec<u8>) {
        for (i, field) in self.fields.iter().enumerate() {
            if i > 0 {
                out.push(separator);
            }
            out.extend_from_slice(field);
        }
        out.extend_from_slice(self.line_ending.as_bytes());
    }
}

pub(crate) fn is_header_id(id: &[u8]) -> bool {
    matches!(id, b"MSH" | b"FHS" | b"BHS")
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

    /// The segment identifier, for example `b"PID"`.
    pub fn id(&self) -> &'a [u8] {
        self.segment.id()
    }

    /// Whether the segment identifier equals `id`.
    pub fn is(&self, id: &str) -> bool {
        self.segment.id() == id.as_bytes()
    }

    /// The position of this segment in [`Message::segments`](crate::Message::segments).
    pub fn index(&self) -> usize {
        self.index
    }

    /// The number of the last field present in the segment.
    pub fn field_count(&self) -> usize {
        self.segment.field_count()
    }

    /// The line ending that terminated this segment.
    pub fn line_ending(&self) -> LineEnding {
        self.segment.line_ending
    }

    /// Field `n` (1-based). For header segments, field 1 is the field
    /// separator and field 2 the encoding characters; both are atomic.
    pub fn field(&self, n: usize) -> Option<Value<'a>> {
        if self.segment.is_header() && n <= 2 {
            return match n {
                1 => Some(Value::new(
                    std::slice::from_ref(&self.delimiters.field),
                    self.delimiters,
                    Level::Atomic,
                )),
                2 => self
                    .segment
                    .fields
                    .get(1)
                    .map(|raw| Value::new(raw, self.delimiters, Level::Atomic)),
                _ => None,
            };
        }
        let index = self.segment.storage_index(n)?;
        self.segment
            .fields
            .get(index)
            .map(|raw| Value::new(raw, self.delimiters, Level::Field))
    }

    /// The value at a field path such as `5`, `5.1` or `3[2].1`. Returns
    /// `None` when the path is invalid or the value is absent.
    pub fn get(&self, path: &str) -> Option<Value<'a>> {
        self.get_path(&path.parse().ok()?)
    }

    /// The value at `path`, or `None` when it is absent.
    pub fn get_path(&self, path: &FieldPath) -> Option<Value<'a>> {
        let mut value = self.field(path.field_number())?;
        if let Some(repetition) = path.repetition_index() {
            value = value.repetition(repetition)?;
        }
        if let Some(component) = path.component_index() {
            value = value.component(component)?;
        }
        if let Some(subcomponent) = path.subcomponent_index() {
            value = value.subcomponent(subcomponent)?;
        }
        Some(value)
    }

    /// The segment serialized exactly as it would appear in the message,
    /// including its line ending.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.segment.write_to(self.delimiters.field, &mut out);
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

    /// Escapes `bytes` (already in the message character set) and stores
    /// them at a field path such as `5.1`.
    pub fn set_bytes(&mut self, path: &str, bytes: &[u8]) -> Result<(), PathError> {
        let path: FieldPath = path.parse()?;
        let raw = escape(bytes, self.delimiters)?;
        set_raw(self.segment, self.delimiters, &path, &raw)
    }

    /// Stores already-escaped `raw` bytes at a field path. Delimiters inside
    /// `raw` keep their structural meaning.
    pub fn set_raw(&mut self, path: &str, raw: &[u8]) -> Result<(), PathError> {
        set_raw(self.segment, self.delimiters, &path.parse()?, raw)
    }

    /// Stores already-escaped `raw` bytes at `path`.
    pub fn set_raw_path(&mut self, path: &FieldPath, raw: &[u8]) -> Result<(), PathError> {
        set_raw(self.segment, self.delimiters, path, raw)
    }
}

/// Writes `raw` at `path`, creating missing fields, repetitions, components
/// and subcomponents as empty values.
pub(crate) fn set_raw(
    segment: &mut Segment,
    delimiters: &Delimiters,
    path: &FieldPath,
    raw: &[u8],
) -> Result<(), PathError> {
    if segment.is_header() && path.field_number() <= 2 {
        return Err(PathError::ReadOnly);
    }
    let subcomponent = match path.subcomponent_index() {
        Some(index) => Some((
            delimiters
                .subcomponent
                .ok_or(PathError::MissingDelimiter("subcomponent"))?,
            index,
        )),
        None => None,
    };
    let index = segment
        .storage_index(path.field_number())
        .ok_or(PathError::ReadOnly)?;
    if segment.fields.len() <= index {
        segment.fields.resize(index + 1, Vec::new());
    }
    let current = std::mem::take(&mut segment.fields[index]);
    let nested = path.repetition_index().is_some() || path.component_index().is_some();
    segment.fields[index] = if nested {
        replace_part(
            &current,
            delimiters.repetition,
            path.repetition_index().unwrap_or(1),
            |repetition| match path.component_index() {
                None => raw.to_vec(),
                Some(component) => {
                    replace_part(repetition, delimiters.component, component, |value| {
                        match subcomponent {
                            None => raw.to_vec(),
                            Some((separator, index)) => {
                                replace_part(value, separator, index, |_| raw.to_vec())
                            }
                        }
                    })
                }
            },
        )
    } else {
        raw.to_vec()
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

    fn segment(line: &[u8]) -> Segment {
        Segment {
            fields: line.split(|&b| b == b'|').map(<[u8]>::to_vec).collect(),
            line_ending: LineEnding::Cr,
        }
    }

    fn write(segment: &Segment) -> Vec<u8> {
        let mut out = Vec::new();
        segment.write_to(b'|', &mut out);
        out
    }

    fn set(segment: &mut Segment, path: &str, raw: &[u8]) {
        set_raw(segment, &Delimiters::default(), &path.parse().unwrap(), raw).unwrap();
    }

    #[test]
    fn replace_part_pads_and_preserves() {
        assert_eq!(replace_part(b"a^b^c", b'^', 2, |_| b"X".to_vec()), b"a^X^c");
        assert_eq!(replace_part(b"a", b'^', 3, |_| b"X".to_vec()), b"a^^X");
        assert_eq!(replace_part(b"", b'^', 1, |_| b"X".to_vec()), b"X");
    }

    #[test]
    fn sets_nested_values_with_padding() {
        let mut s = segment(b"PID|1");
        set(&mut s, "5.2", b"Jane");
        assert_eq!(write(&s), b"PID|1||||^Jane\r");
        set(&mut s, "5.1", b"Doe");
        assert_eq!(write(&s), b"PID|1||||Doe^Jane\r");
        set(&mut s, "3[2].1", b"X");
        assert_eq!(write(&s), b"PID|1||~X||Doe^Jane\r");
        set(&mut s, "5.1.2", b"van");
        assert_eq!(write(&s), b"PID|1||~X||Doe&van^Jane\r");
        set(&mut s, "5", b"Whole");
        assert_eq!(write(&s), b"PID|1||~X||Whole\r");
    }

    #[test]
    fn header_fields_are_numbered_from_the_separator() {
        let mut s = segment(b"MSH|^~\\&|APP");
        let d = Delimiters::default();
        let view = SegmentRef::new(&s, &d, 0);
        assert_eq!(view.field(1).unwrap(), "|");
        assert_eq!(view.field(2).unwrap(), "^~\\&");
        assert_eq!(view.field(3).unwrap(), "APP");
        assert_eq!(view.field_count(), 3);
        set(&mut s, "5", b"LIS");
        assert_eq!(write(&s), b"MSH|^~\\&|APP||LIS\r");
        assert_eq!(
            set_raw(&mut s, &d, &"2".parse().unwrap(), b"x"),
            Err(PathError::ReadOnly)
        );
        assert_eq!(
            set_raw(&mut s, &d, &"1".parse().unwrap(), b"x"),
            Err(PathError::ReadOnly)
        );
    }

    #[test]
    fn requires_subcomponent_delimiter() {
        let mut s = segment(b"PID|1");
        let d = Delimiters {
            subcomponent: None,
            ..Delimiters::default()
        };
        assert_eq!(
            set_raw(&mut s, &d, &"5.1.2".parse().unwrap(), b"x"),
            Err(PathError::MissingDelimiter("subcomponent"))
        );
    }
}
