//! Fixed-width records: one record per line, each field at a declared byte
//! position.
//!
//! Lines are kept exactly as written, so unmodified records serialize byte
//! for byte. Positions and lengths are counted in bytes of the encoded text.
//!
//! Reading a field removes its padding: trailing pad characters for
//! left-aligned fields, leading ones for right-aligned fields. A line that
//! ends before a field yields an empty value. Writing pads the value to the
//! field length; a value longer than the field is rejected unless the layout
//! allows truncation, because silently shortening clinical data is unsafe.

use encoding_rs::{Encoding, UTF_8};
use thiserror::Error;

use crate::text::{
    LineEnding, Lines, decode, encode, is_byte_safe, preferred_ending, split_table_path,
};

/// The largest accepted number of fields in a layout.
const MAX_FIELDS: usize = 4096;
/// The largest accepted record length.
const MAX_RECORD_LEN: usize = 1024 * 1024;

/// How a value is placed inside its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Alignment {
    /// Value first, padding after (typical for text).
    Left,
    /// Padding first, value after (typical for numbers).
    Right,
}

/// What happens when a written value is longer than its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Overflow {
    /// Reject the value with [`FixedWidthError::Overflow`].
    Reject,
    /// Keep as many whole characters as fit.
    Truncate,
}

/// One field of a [`FixedWidthLayout`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FixedField {
    name: String,
    start: usize,
    length: usize,
    alignment: Alignment,
    pad: u8,
}

impl FixedField {
    /// A left-aligned, space-padded field of `length` bytes starting at the
    /// 0-based byte offset `start`.
    pub fn new(name: impl Into<String>, start: usize, length: usize) -> Self {
        Self {
            name: name.into(),
            start,
            length,
            alignment: Alignment::Left,
            pad: b' ',
        }
    }

    /// Sets the alignment.
    pub fn aligned(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Sets the pad character, for example `b'0'` for zero-filled numbers.
    pub fn padded_with(mut self, pad: u8) -> Self {
        self.pad = pad;
        self
    }

    /// The field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The 0-based byte offset.
    pub fn start(&self) -> usize {
        self.start
    }

    /// The length in bytes.
    pub fn length(&self) -> usize {
        self.length
    }
}

/// The field layout shared by all records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedWidthLayout {
    fields: Vec<FixedField>,
    overflow: Overflow,
    encoding: &'static Encoding,
    line_ending: LineEnding,
    max_records: usize,
}

impl FixedWidthLayout {
    /// Creates a layout. Field names must be unique and non-empty, lengths
    /// non-zero, fields must not overlap, and pad characters must not be
    /// line breaks.
    pub fn new(fields: Vec<FixedField>) -> Result<Self, FixedWidthError> {
        let invalid = |reason: &str| FixedWidthError::InvalidLayout(reason.to_owned());
        if fields.is_empty() || fields.len() > MAX_FIELDS {
            return Err(invalid("a layout needs between 1 and 4096 fields"));
        }
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(fields.len());
        for (i, field) in fields.iter().enumerate() {
            let end = field
                .start
                .checked_add(field.length)
                .filter(|&end| end <= MAX_RECORD_LEN)
                .ok_or_else(|| invalid("field extends beyond the maximum record length"))?;
            if field.name.is_empty() || field.name.contains('/') {
                return Err(invalid(
                    "field names must be non-empty and must not contain '/'",
                ));
            }
            if field.length == 0 {
                return Err(invalid("field lengths must be greater than zero"));
            }
            if field.pad == b'\r' || field.pad == b'\n' {
                return Err(invalid("pad characters cannot be line breaks"));
            }
            if fields[..i].iter().any(|other| other.name == field.name) {
                return Err(FixedWidthError::InvalidLayout(format!(
                    "duplicate field {:?}",
                    field.name
                )));
            }
            if spans
                .iter()
                .any(|&(start, other_end)| field.start < other_end && start < end)
            {
                return Err(FixedWidthError::InvalidLayout(format!(
                    "field {:?} overlaps another field",
                    field.name
                )));
            }
            spans.push((field.start, end));
        }
        Ok(Self {
            fields,
            overflow: Overflow::Reject,
            encoding: UTF_8,
            line_ending: LineEnding::CrLf,
            max_records: 1_000_000,
        })
    }

    /// Sets the overflow policy (default [`Overflow::Reject`]).
    pub fn with_overflow(mut self, overflow: Overflow) -> Self {
        self.overflow = overflow;
        self
    }

    /// Sets the text encoding (default UTF-8). Encodings whose multi-byte
    /// characters may contain line-break bytes are rejected.
    pub fn with_encoding(mut self, encoding: &'static Encoding) -> Result<Self, FixedWidthError> {
        if !is_byte_safe(encoding) {
            return Err(FixedWidthError::InvalidLayout(format!(
                "unsupported encoding {}",
                encoding.name()
            )));
        }
        self.encoding = encoding;
        Ok(self)
    }

    /// Sets the line ending for records added to a document whose records
    /// have none (default CRLF).
    pub fn with_line_ending(mut self, line_ending: LineEnding) -> Self {
        self.line_ending = line_ending;
        self
    }

    /// Sets the largest accepted number of records (default 1 000 000).
    pub fn with_max_records(mut self, max_records: usize) -> Self {
        self.max_records = max_records;
        self
    }

    /// The fields in declaration order.
    pub fn fields(&self) -> &[FixedField] {
        &self.fields
    }

    fn field(&self, name: &str) -> Result<&FixedField, FixedWidthError> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .ok_or_else(|| FixedWidthError::UnknownField(name.to_owned()))
    }
}

/// Returned when a layout is invalid or a record cannot be read or written.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FixedWidthError {
    /// The layout is inconsistent.
    #[error("invalid fixed-width layout: {0}")]
    InvalidLayout(String),
    /// The input has more records than the layout allows.
    #[error("fixed-width input exceeds the configured record limit")]
    TooManyRecords,
    /// The field name is not in the layout.
    #[error("unknown field {0:?}")]
    UnknownField(String),
    /// The path text is not `record/field`.
    #[error("invalid fixed-width path {0:?}")]
    Path(String),
    /// A record index is more than one past the last record.
    #[error("record {record} does not exist and is not the next record ({records} records)")]
    RecordOutOfRange {
        /// The requested record.
        record: usize,
        /// The number of records.
        records: usize,
    },
    /// The value is longer than the field and the layout rejects overflow.
    #[error("value of {len} bytes does not fit field {field:?} of {max} bytes")]
    Overflow {
        /// The field name.
        field: String,
        /// The encoded value length.
        len: usize,
        /// The field length.
        max: usize,
    },
    /// The value contains a line break or a character the encoding cannot
    /// represent.
    #[error("value cannot be stored in a fixed-width record")]
    InvalidValue,
}

/// Parsed fixed-width records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedWidthDocument {
    layout: FixedWidthLayout,
    records: Vec<(Vec<u8>, LineEnding)>,
}

impl FixedWidthDocument {
    /// Splits `input` into records (CRLF, LF or CR).
    pub fn parse(input: &[u8], layout: &FixedWidthLayout) -> Result<Self, FixedWidthError> {
        let mut records = Vec::new();
        for (line, ending) in Lines::new(input) {
            if records.len() == layout.max_records {
                return Err(FixedWidthError::TooManyRecords);
            }
            records.push((line.to_vec(), ending));
        }
        Ok(Self {
            layout: layout.clone(),
            records,
        })
    }

    /// The layout.
    pub fn layout(&self) -> &FixedWidthLayout {
        &self.layout
    }

    /// The number of records.
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// The value of field `name` in `record` with its padding removed, or
    /// `None` when the record does not exist.
    pub fn get(&self, record: usize, name: &str) -> Result<Option<String>, FixedWidthError> {
        let field = self.layout.field(name)?;
        let Some((line, _)) = self.records.get(record) else {
            return Ok(None);
        };
        let start = field.start.min(line.len());
        let end = (field.start + field.length).min(line.len());
        let raw = &line[start..end];
        let trimmed = match field.alignment {
            Alignment::Left => {
                let len = raw
                    .iter()
                    .rposition(|&b| b != field.pad)
                    .map_or(0, |i| i + 1);
                &raw[..len]
            }
            Alignment::Right => {
                let start = raw
                    .iter()
                    .position(|&b| b != field.pad)
                    .unwrap_or(raw.len());
                &raw[start..]
            }
        };
        Ok(Some(decode(trimmed, self.layout.encoding)))
    }

    /// Stores `value` in field `name` of `record`, padding it to the field
    /// length. `record` may be one past the last record to append a record.
    /// Bytes between the end of a short line and the field are filled with
    /// spaces.
    pub fn set(&mut self, record: usize, name: &str, value: &str) -> Result<(), FixedWidthError> {
        let field = self.layout.field(name)?.clone();
        if value.contains(['\r', '\n']) {
            return Err(FixedWidthError::InvalidValue);
        }
        let mut bytes = encode(value, self.layout.encoding)
            .ok_or(FixedWidthError::InvalidValue)?
            .into_owned();
        if bytes.len() > field.length {
            match self.layout.overflow {
                Overflow::Reject => {
                    return Err(FixedWidthError::Overflow {
                        field: field.name,
                        len: bytes.len(),
                        max: field.length,
                    });
                }
                Overflow::Truncate => bytes = self.truncate(value, field.length),
            }
        }
        let records = self.records.len();
        if record > records {
            return Err(FixedWidthError::RecordOutOfRange { record, records });
        }
        if record == records {
            self.append_record();
        }
        let Some((line, _)) = self.records.get_mut(record) else {
            return Err(FixedWidthError::RecordOutOfRange { record, records });
        };
        let padding = vec![field.pad; field.length - bytes.len()];
        let content = match field.alignment {
            Alignment::Left => [bytes, padding].concat(),
            Alignment::Right => [padding, bytes].concat(),
        };
        let end = field.start + field.length;
        if line.len() < end {
            line.resize(end, b' ');
        }
        line[field.start..end].copy_from_slice(&content);
        Ok(())
    }

    /// The value at a `record/field` path, where `record` is 0-based.
    pub fn get_path(&self, path: &str) -> Result<Option<String>, FixedWidthError> {
        let (record, name) =
            split_table_path(path).ok_or_else(|| FixedWidthError::Path(path.to_owned()))?;
        self.get(record, name)
    }

    /// Stores `value` at a `record/field` path.
    pub fn set_path(&mut self, path: &str, value: &str) -> Result<(), FixedWidthError> {
        let (record, name) =
            split_table_path(path).ok_or_else(|| FixedWidthError::Path(path.to_owned()))?;
        self.set(record, name, value)
    }

    /// Serializes the records. Unmodified records are reproduced byte for
    /// byte.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (line, ending) in &self.records {
            out.extend_from_slice(line);
            out.extend_from_slice(ending.as_bytes());
        }
        out
    }

    /// Keeps as many whole characters of `value` as fit in `max` bytes.
    fn truncate(&self, value: &str, max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buffer = [0; 4];
        for c in value.chars() {
            let (bytes, _, _) = self.layout.encoding.encode(c.encode_utf8(&mut buffer));
            if out.len() + bytes.len() > max {
                break;
            }
            out.extend_from_slice(&bytes);
        }
        out
    }

    fn append_record(&mut self) {
        let ending = preferred_ending(self.records.first().map(|r| r.1), self.layout.line_ending);
        match self.records.last_mut() {
            Some(last) if last.1 == LineEnding::None => {
                last.1 = ending;
                self.records.push((Vec::new(), LineEnding::None));
            }
            _ => self.records.push((Vec::new(), ending)),
        }
    }
}

#[cfg(test)]
mod tests {
    use encoding_rs::WINDOWS_1254;

    use super::*;

    fn layout() -> FixedWidthLayout {
        FixedWidthLayout::new(vec![
            FixedField::new("sample", 0, 6),
            FixedField::new("test", 6, 4),
            FixedField::new("value", 10, 7)
                .aligned(Alignment::Right)
                .padded_with(b'0'),
            FixedField::new("flag", 18, 1),
        ])
        .unwrap()
    }

    const INPUT: &[u8] = b"S1    GLU 0005.40 N\r\nS2    HGB 0013.20\r\n";

    #[test]
    fn reads_and_writes_fields() {
        let mut doc = FixedWidthDocument::parse(INPUT, &layout()).unwrap();
        assert_eq!(doc.to_bytes(), INPUT);
        assert_eq!(doc.get(0, "test").unwrap().as_deref(), Some("GLU"));
        assert_eq!(doc.get_path("0/value").unwrap().as_deref(), Some("5.40"));
        assert_eq!(doc.get(1, "flag").unwrap().as_deref(), Some(""));
        assert_eq!(doc.get(2, "flag").unwrap(), None);
        doc.set_path("1/flag", "H").unwrap();
        doc.set(0, "value", "6.1").unwrap();
        doc.set(2, "sample", "S3").unwrap();
        assert_eq!(
            doc.to_bytes(),
            b"S1    GLU 00006.1 N\r\nS2    HGB 0013.20 H\r\nS3    \r\n"
        );
    }

    #[test]
    fn rejects_or_truncates_long_values() {
        let mut doc = FixedWidthDocument::parse(INPUT, &layout()).unwrap();
        assert!(matches!(
            doc.set(0, "test", "GLUCOSE"),
            Err(FixedWidthError::Overflow { .. })
        ));
        let mut doc =
            FixedWidthDocument::parse(INPUT, &layout().with_overflow(Overflow::Truncate)).unwrap();
        doc.set(0, "test", "GLUCOSE").unwrap();
        assert_eq!(doc.get(0, "test").unwrap().as_deref(), Some("GLUC"));
        doc.set(0, "test", "ÇÇÇ").unwrap();
        assert_eq!(doc.get(0, "test").unwrap().as_deref(), Some("ÇÇ"));
        assert_eq!(
            doc.set(0, "test", "a\nb"),
            Err(FixedWidthError::InvalidValue)
        );
    }

    #[test]
    fn uses_the_layout_encoding() {
        let layout = layout().with_encoding(WINDOWS_1254).unwrap();
        let mut doc = FixedWidthDocument::parse(b"", &layout).unwrap();
        doc.set(0, "sample", "ŞÜ").unwrap();
        assert_eq!(doc.to_bytes(), b"\xDE\xDC    \r\n");
        assert_eq!(doc.get(0, "sample").unwrap().as_deref(), Some("ŞÜ"));
    }

    #[test]
    fn validates_layouts() {
        for fields in [
            vec![],
            vec![FixedField::new("a", 0, 0)],
            vec![FixedField::new("", 0, 1)],
            vec![FixedField::new("a", 0, 2), FixedField::new("b", 1, 2)],
            vec![FixedField::new("a", 0, 1), FixedField::new("a", 1, 1)],
            vec![FixedField::new("a", usize::MAX, 2)],
        ] {
            assert!(FixedWidthLayout::new(fields).is_err());
        }
        let doc = FixedWidthDocument::parse(b"x", &layout()).unwrap();
        assert!(matches!(
            doc.get(0, "nope"),
            Err(FixedWidthError::UnknownField(_))
        ));
        assert!(matches!(
            doc.get_path("nope"),
            Err(FixedWidthError::Path(_))
        ));
    }
}
