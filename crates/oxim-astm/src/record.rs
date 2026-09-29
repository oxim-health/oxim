use crate::delimiters::Delimiters;
use crate::error::PathError;
use crate::escape::escape;
use crate::path::FieldPath;
use crate::value::{Level, Value};

/// How a record was terminated in the original bytes.
///
/// ASTM E1394 terminates records with a carriage return. Files and some
/// senders use CRLF or LF; the original ending is kept so serialization is
/// byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineEnding {
    /// Carriage return (`\r`), the standard terminator.
    #[default]
    Cr,
    /// Line feed (`\n`).
    Lf,
    /// Carriage return followed by line feed.
    CrLf,
    /// No terminator (only possible for the last record).
    None,
}

impl LineEnding {
    /// The bytes written after the record.
    pub fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
            Self::None => b"",
        }
    }
}

/// Storage for one record line. Fields are raw, escaped bytes.
///
/// `fields[0]` is field 1 (the record type). In the header, `fields[1]` is
/// field 2 without its leading field delimiter: the delimiter definition
/// `\^&`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Record {
    pub(crate) fields: Vec<Vec<u8>>,
    pub(crate) line_ending: LineEnding,
}

impl Record {
    pub(crate) fn new(record_type: &[u8], line_ending: LineEnding) -> Self {
        Self {
            fields: vec![record_type.to_vec()],
            line_ending,
        }
    }

    pub(crate) fn record_type(&self) -> &[u8] {
        self.fields.first().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.fields.len() == 1 && self.record_type().is_empty()
    }

    pub(crate) fn is_header(&self) -> bool {
        self.record_type() == b"H"
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

/// A read-only view of one record.
#[derive(Debug, Clone, Copy)]
pub struct RecordRef<'a> {
    record: &'a Record,
    delimiters: &'a Delimiters,
    index: usize,
}

impl<'a> RecordRef<'a> {
    pub(crate) fn new(record: &'a Record, delimiters: &'a Delimiters, index: usize) -> Self {
        Self {
            record,
            delimiters,
            index,
        }
    }

    /// The record type, for example `b"R"`.
    pub fn record_type(&self) -> &'a [u8] {
        self.record.record_type()
    }

    /// Whether the record type equals `record_type`.
    pub fn is(&self, record_type: &str) -> bool {
        self.record.record_type() == record_type.as_bytes()
    }

    /// The position of this record in [`Message::records`](crate::Message::records).
    pub fn index(&self) -> usize {
        self.index
    }

    /// The number of the last field present in the record.
    pub fn field_count(&self) -> usize {
        self.record.fields.len()
    }

    /// The line ending that terminated this record.
    pub fn line_ending(&self) -> LineEnding {
        self.record.line_ending
    }

    /// Field `n` (1-based, record type = field 1). The header's field 2, the
    /// delimiter definition, is atomic.
    pub fn field(&self, n: usize) -> Option<Value<'a>> {
        let raw = self.record.fields.get(n.checked_sub(1)?)?;
        let level = if self.record.is_header() && n == 2 {
            Level::Atomic
        } else {
            Level::Field
        };
        Some(Value::new(raw, self.delimiters, level))
    }

    /// The value at a field path such as `4`, `3.4` or `6[2].1`. Returns
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
        Some(value)
    }

    /// The record serialized exactly as it appears in the message, including
    /// its line ending.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.record.write_to(self.delimiters.field, &mut out);
        out
    }
}

/// A mutable view of one record.
#[derive(Debug)]
pub struct RecordMut<'a> {
    record: &'a mut Record,
    delimiters: &'a Delimiters,
    index: usize,
}

impl<'a> RecordMut<'a> {
    pub(crate) fn new(record: &'a mut Record, delimiters: &'a Delimiters, index: usize) -> Self {
        Self {
            record,
            delimiters,
            index,
        }
    }

    /// A read-only view of the record.
    pub fn view(&self) -> RecordRef<'_> {
        RecordRef::new(self.record, self.delimiters, self.index)
    }

    /// Escapes `bytes` (already in the target character encoding) and stores
    /// them at a field path such as `3.4`.
    pub fn set_bytes(&mut self, path: &str, bytes: &[u8]) -> Result<(), PathError> {
        let raw = escape(bytes, self.delimiters);
        set_raw(self.record, self.delimiters, &path.parse()?, &raw)
    }

    /// Stores already-escaped `raw` bytes at a field path. Delimiters inside
    /// `raw` keep their structural meaning.
    pub fn set_raw(&mut self, path: &str, raw: &[u8]) -> Result<(), PathError> {
        set_raw(self.record, self.delimiters, &path.parse()?, raw)
    }
}

/// Writes `raw` at `path`, creating missing fields, repetitions and
/// components as empty values.
pub(crate) fn set_raw(
    record: &mut Record,
    delimiters: &Delimiters,
    path: &FieldPath,
    raw: &[u8],
) -> Result<(), PathError> {
    let field = path.field_number();
    if field == 1 || (record.is_header() && field == 2) {
        return Err(PathError::ReadOnly);
    }
    let index = field - 1;
    if record.fields.len() <= index {
        record.fields.resize(index + 1, Vec::new());
    }
    let current = std::mem::take(&mut record.fields[index]);
    let nested = path.repetition_index().is_some() || path.component_index().is_some();
    record.fields[index] = if nested {
        replace_part(
            &current,
            delimiters.repeat,
            path.repetition_index().unwrap_or(1),
            |repetition| match path.component_index() {
                None => raw.to_vec(),
                Some(component) => {
                    replace_part(repetition, delimiters.component, component, |_| {
                        raw.to_vec()
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

    fn record(line: &[u8]) -> Record {
        Record {
            fields: line.split(|&b| b == b'|').map(<[u8]>::to_vec).collect(),
            line_ending: LineEnding::Cr,
        }
    }

    fn write(record: &Record) -> Vec<u8> {
        let mut out = Vec::new();
        record.write_to(b'|', &mut out);
        out
    }

    fn set(record: &mut Record, path: &str, raw: &[u8]) -> Result<(), PathError> {
        set_raw(record, &Delimiters::default(), &path.parse().unwrap(), raw)
    }

    #[test]
    fn uses_astm_field_numbering() {
        let r = record(b"R|1|^^^GLU|5.4|mmol/L");
        let d = Delimiters::default();
        let view = RecordRef::new(&r, &d, 0);
        assert_eq!(view.field(1).unwrap(), "R");
        assert_eq!(view.field(2).unwrap(), "1");
        assert_eq!(view.get("3.4").unwrap(), "GLU");
        assert_eq!(view.field(4).unwrap(), "5.4");
        assert_eq!(view.field_count(), 5);
        assert!(view.field(0).is_none());
    }

    #[test]
    fn sets_values_with_padding() {
        let mut r = record(b"R|1");
        set(&mut r, "3.4", b"GLU").unwrap();
        assert_eq!(write(&r), b"R|1|^^^GLU\r");
        set(&mut r, "3[2].4", b"HGB").unwrap();
        assert_eq!(write(&r), b"R|1|^^^GLU\\^^^HGB\r");
        set(&mut r, "7", b"N").unwrap();
        assert_eq!(write(&r), b"R|1|^^^GLU\\^^^HGB||||N\r");
    }

    #[test]
    fn protects_record_type_and_delimiter_definition() {
        let mut header = record(b"H|\\^&|||LIS");
        assert_eq!(set(&mut header, "1", b"X"), Err(PathError::ReadOnly));
        assert_eq!(set(&mut header, "2", b"X"), Err(PathError::ReadOnly));
        set(&mut header, "5", b"OXIM").unwrap();
        assert_eq!(write(&header), b"H|\\^&|||OXIM\r");
        let d = Delimiters::default();
        let view = RecordRef::new(&header, &d, 0);
        assert_eq!(view.get("2.1").unwrap(), "\\^&");
        let mut result = record(b"R|1");
        assert_eq!(set(&mut result, "1", b"X"), Err(PathError::ReadOnly));
    }
}
