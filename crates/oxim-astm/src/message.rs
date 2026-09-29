use std::fmt;

use encoding_rs::Encoding;
use memchr::{memchr, memchr2};

use crate::delimiters::Delimiters;
use crate::error::{ParseError, PathError};
use crate::escape::escape;
use crate::path::{Path, is_record_type};
use crate::record::{LineEnding, Record, RecordMut, RecordRef, set_raw};
use crate::value::Value;

/// Which byte sequences end a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RecordTerminators {
    /// CR and CRLF. A lone LF is treated as data.
    Standard,
    /// CR, CRLF and a lone LF. Suits messages read from files.
    #[default]
    Lenient,
}

/// Options for [`Message::parse_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseOptions {
    /// Which byte sequences end a record.
    pub record_terminators: RecordTerminators,
    /// The largest number of record lines accepted.
    pub max_records: usize,
    /// The largest number of fields accepted in one record.
    pub max_fields_per_record: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            record_terminators: RecordTerminators::default(),
            max_records: 100_000,
            max_fields_per_record: 10_000,
        }
    }
}

/// An ASTM E1394 (CLSI LIS02-A2) message: a header (`H`) record followed by
/// patient (`P`), order (`O`), result (`R`), comment (`C`), request (`Q`),
/// manufacturer (`M`), scientific (`S`) and terminator (`L`) records.
///
/// The message keeps every record as raw, escaped bytes together with its
/// original line ending, so [`Message::to_bytes`] reproduces the parsed input
/// byte for byte until the message is edited, and edits only rewrite the
/// values they touch.
///
/// # Field numbering
///
/// ASTM numbers fields differently from HL7: **the record type is field 1**
/// in every record, and in the header field 2 is the delimiter definition
/// (`\^&`). So `R-3` is the universal test ID and `R-4` the measurement
/// value, matching the numbering of the standard (`R.3`, `R.4`), and `H-5`
/// is the sender name. The dotted form `R.4` is accepted as a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message {
    delimiters: Delimiters,
    lines: Vec<Record>,
}

impl Message {
    /// Parses a message with the default [`ParseOptions`].
    pub fn parse(input: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with(input, &ParseOptions::default())
    }

    /// Parses a message. The input must start with the header record;
    /// record types and order are not validated.
    pub fn parse_with(input: &[u8], options: &ParseOptions) -> Result<Self, ParseError> {
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        if input.first() != Some(&b'H') {
            return Err(ParseError::MissingHeader);
        }
        let delimiters = Delimiters::from_header(input)?;
        let mut lines = Vec::new();
        for (line, line_ending) in Lines::new(input, options.record_terminators) {
            if lines.len() == options.max_records {
                return Err(ParseError::LimitExceeded("records"));
            }
            let mut fields = Vec::new();
            for field in line.split(|&b| b == delimiters.field) {
                if fields.len() == options.max_fields_per_record {
                    return Err(ParseError::LimitExceeded("fields per record"));
                }
                fields.push(field.to_vec());
            }
            lines.push(Record {
                fields,
                line_ending,
            });
        }
        Ok(Self { delimiters, lines })
    }

    /// Creates a message containing only a header record with the given
    /// delimiters.
    pub fn new(delimiters: Delimiters) -> Result<Self, ParseError> {
        delimiters.validate()?;
        Ok(Self {
            delimiters,
            lines: vec![Record {
                fields: vec![b"H".to_vec(), delimiters.definition().to_vec()],
                line_ending: LineEnding::Cr,
            }],
        })
    }

    /// Serializes the message.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out);
        out
    }

    /// Appends the serialized message to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        for line in &self.lines {
            line.write_to(self.delimiters.field, out);
        }
    }

    /// The delimiters declared by the header.
    pub fn delimiters(&self) -> &Delimiters {
        &self.delimiters
    }

    /// The header record.
    pub fn header(&self) -> RecordRef<'_> {
        RecordRef::new(&self.lines[0], &self.delimiters, 0)
    }

    /// The records in order, skipping blank lines.
    pub fn records(&self) -> impl Iterator<Item = RecordRef<'_>> + '_ {
        self.lines
            .iter()
            .filter(|line| !line.is_blank())
            .enumerate()
            .map(|(index, line)| RecordRef::new(line, &self.delimiters, index))
    }

    /// The number of records, not counting blank lines.
    pub fn record_count(&self) -> usize {
        self.lines.iter().filter(|line| !line.is_blank()).count()
    }

    /// The records of type `record_type`, in order.
    pub fn records_of_type<'a>(
        &'a self,
        record_type: &'a str,
    ) -> impl Iterator<Item = RecordRef<'a>> + 'a {
        self.records().filter(move |record| record.is(record_type))
    }

    /// The 1-based `occurrence` of a record type.
    pub fn record(&self, record_type: &str, occurrence: usize) -> Option<RecordRef<'_>> {
        self.records()
            .filter(|record| record.is(record_type))
            .nth(occurrence.checked_sub(1)?)
    }

    /// A mutable view of the 1-based `occurrence` of a record type.
    pub fn record_mut(&mut self, record_type: &str, occurrence: usize) -> Option<RecordMut<'_>> {
        let index = self.record(record_type, occurrence)?.index();
        self.record_at_mut(index)
    }

    /// A mutable view of the record at position `index` of
    /// [`Message::records`].
    pub fn record_at_mut(&mut self, index: usize) -> Option<RecordMut<'_>> {
        let line = self.line_index(index)?;
        Some(RecordMut::new(
            &mut self.lines[line],
            &self.delimiters,
            index,
        ))
    }

    /// The value at a path such as `R-4`, `R[2]-3.4` or `P-6[2].1`. Returns
    /// `None` when the path is invalid or the value is absent.
    pub fn get(&self, path: &str) -> Option<Value<'_>> {
        self.get_path(&path.parse().ok()?)
    }

    /// The value at `path`, or `None` when it is absent.
    pub fn get_path(&self, path: &Path) -> Option<Value<'_>> {
        self.record(path.record_type(), path.occurrence())?
            .get_path(path.field_path())
    }

    /// Encodes `text` with `encoding`, escapes delimiters and stores the
    /// result at `path`.
    pub fn set_text(
        &mut self,
        path: &str,
        text: &str,
        encoding: &'static Encoding,
    ) -> Result<(), PathError> {
        let (bytes, _, unmappable) = encoding.encode(text);
        if unmappable {
            return Err(PathError::Unencodable(encoding.name()));
        }
        self.set_bytes(path, &bytes)
    }

    /// Escapes `bytes`, which must already be in the target encoding, and
    /// stores them at `path`.
    pub fn set_bytes(&mut self, path: &str, bytes: &[u8]) -> Result<(), PathError> {
        let raw = escape(bytes, &self.delimiters).into_owned();
        self.set_raw(path, &raw)
    }

    /// Stores already-escaped `raw` bytes at `path`. Delimiters inside `raw`
    /// keep their structural meaning.
    pub fn set_raw(&mut self, path: &str, raw: &[u8]) -> Result<(), PathError> {
        self.set_raw_path(&path.parse()?, raw)
    }

    /// Stores already-escaped `raw` bytes at `path`.
    pub fn set_raw_path(&mut self, path: &Path, raw: &[u8]) -> Result<(), PathError> {
        let index = self
            .record(path.record_type(), path.occurrence())
            .ok_or_else(|| PathError::RecordNotFound(path.to_string()))?
            .index();
        let line = self
            .line_index(index)
            .ok_or_else(|| PathError::RecordNotFound(path.to_string()))?;
        set_raw(
            &mut self.lines[line],
            &self.delimiters,
            path.field_path(),
            raw,
        )
    }

    /// Appends an empty record and returns a mutable view of it.
    pub fn push_record(&mut self, record_type: &str) -> Result<RecordMut<'_>, PathError> {
        let count = self.record_count();
        self.insert_record(count, record_type)
    }

    /// Inserts an empty record so it becomes position `index` of
    /// [`Message::records`]. Position 0 is reserved for the header.
    pub fn insert_record(
        &mut self,
        index: usize,
        record_type: &str,
    ) -> Result<RecordMut<'_>, PathError> {
        let type_bytes = record_type.as_bytes();
        if !is_record_type(type_bytes) || type_bytes == b"H" {
            return Err(PathError::InvalidRecordType(record_type.to_owned()));
        }
        let count = self.record_count();
        if index == 0 || index > count {
            return Err(PathError::IndexOutOfRange { max: count });
        }
        let ending = self.preferred_line_ending();
        let line = match self.line_index(index) {
            Some(line) => line,
            None => {
                if let Some(last) = self.lines.last_mut()
                    && last.line_ending == LineEnding::None
                {
                    last.line_ending = ending;
                    self.lines.push(Record::new(type_bytes, LineEnding::None));
                    return self
                        .record_at_mut(index)
                        .ok_or(PathError::IndexOutOfRange { max: count });
                }
                self.lines.len()
            }
        };
        self.lines.insert(line, Record::new(type_bytes, ending));
        self.record_at_mut(index)
            .ok_or(PathError::IndexOutOfRange { max: count })
    }

    /// Removes the record at position `index` of [`Message::records`]. The
    /// header cannot be removed.
    pub fn remove_record(&mut self, index: usize) -> Result<(), PathError> {
        let count = self.record_count();
        if index == 0 || index >= count {
            return Err(PathError::IndexOutOfRange {
                max: count.saturating_sub(1),
            });
        }
        let line = self
            .line_index(index)
            .ok_or(PathError::IndexOutOfRange { max: count })?;
        let removed = self.lines.remove(line);
        if removed.line_ending == LineEnding::None
            && line == self.lines.len()
            && let Some(last) = self.lines.last_mut()
        {
            last.line_ending = LineEnding::None;
        }
        Ok(())
    }

    /// Whether the message ends with a terminator (`L`) record.
    pub fn is_terminated(&self) -> bool {
        self.records().last().is_some_and(|record| record.is("L"))
    }

    fn line_index(&self, index: usize) -> Option<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !line.is_blank())
            .nth(index)
            .map(|(line, _)| line)
    }

    fn preferred_line_ending(&self) -> LineEnding {
        match self.lines.first().map(|line| line.line_ending) {
            Some(LineEnding::None) | None => LineEnding::Cr,
            Some(ending) => ending,
        }
    }
}

impl fmt::Display for Message {
    /// Writes the message as UTF-8 (lossy), one record per line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, record) in self.records().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            let mut bytes = record.to_bytes();
            bytes.truncate(bytes.len() - record.line_ending().as_bytes().len());
            f.write_str(&String::from_utf8_lossy(&bytes))?;
        }
        Ok(())
    }
}

/// Splits input into lines and their endings.
pub(crate) struct Lines<'a> {
    rest: &'a [u8],
    mode: RecordTerminators,
}

impl<'a> Lines<'a> {
    pub(crate) fn new(input: &'a [u8], mode: RecordTerminators) -> Self {
        Self { rest: input, mode }
    }
}

impl<'a> Iterator for Lines<'a> {
    type Item = (&'a [u8], LineEnding);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let found = match self.mode {
            RecordTerminators::Standard => memchr(b'\r', self.rest),
            RecordTerminators::Lenient => memchr2(b'\r', b'\n', self.rest),
        };
        let Some(at) = found else {
            let line = self.rest;
            self.rest = &[];
            return Some((line, LineEnding::None));
        };
        let line = &self.rest[..at];
        let (ending, len) = match (self.rest[at], self.rest.get(at + 1)) {
            (b'\r', Some(b'\n')) => (LineEnding::CrLf, 2),
            (b'\r', _) => (LineEnding::Cr, 1),
            _ => (LineEnding::Lf, 1),
        };
        self.rest = &self.rest[at + len..];
        Some((line, ending))
    }
}

#[cfg(test)]
mod tests {
    use encoding_rs::{UTF_8, WINDOWS_1254};

    use super::*;

    const RESULTS: &[u8] = b"H|\\^&|||ANALYZER^1.0|||||LIS||P|1|20260929120000\r\
P|1||PID001||Doe^Jane||19800101|F\r\
O|1|SMP001||^^^GLU\\^^^HGB|R||||||N\r\
R|1|^^^GLU|5.4|mmol/L|3.9^6.1|N||F||||20260929115900\r\
C|1|I|Fasting sample|G\r\
R|2|^^^HGB|13.2|g/dL|12^16|N||F\r\
L|1|N\r";

    #[test]
    fn round_trips_byte_for_byte() {
        for input in [
            RESULTS,
            b"H|\\^&\nP|1\nL|1\n",
            b"H|\\^&\r\nP|1\r\nL|1",
            b"H|\\^&\r\rR|1|||\r\r",
            b"H!~*%!!!LIS\rR!1!\xff!x\r",
        ] {
            let message = Message::parse(input).unwrap();
            assert_eq!(
                message.to_bytes(),
                input,
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn reads_values_with_astm_numbering() {
        let message = Message::parse(RESULTS).unwrap();
        assert_eq!(message.get("H-5.1").unwrap(), "ANALYZER");
        assert!(message.get("P-3").unwrap().is_empty());
        assert_eq!(message.get("P-4").unwrap(), "PID001");
        assert_eq!(message.get("P-6.2").unwrap(), "Jane");
        assert_eq!(message.get("O-5[2].4").unwrap(), "HGB");
        assert_eq!(message.get("R-3.4").unwrap(), "GLU");
        assert_eq!(message.get("R.4").unwrap(), "5.4");
        assert_eq!(message.get("R[2]-4").unwrap(), "13.2");
        assert_eq!(message.get("R[2]-6.1").unwrap(), "12");
        assert!(message.get("R[3]-4").is_none());
        assert_eq!(message.record_count(), 7);
        assert_eq!(message.records_of_type("R").count(), 2);
        assert!(message.is_terminated());
    }

    #[test]
    fn edits_only_what_it_touches() {
        let mut message = Message::parse(RESULTS).unwrap();
        message.set_text("R-4", "5.5", UTF_8).unwrap();
        message
            .set_text("C-4", "Hemolysed^slightly", UTF_8)
            .unwrap();
        let text = String::from_utf8(message.to_bytes()).unwrap();
        let expected = String::from_utf8(RESULTS.to_vec())
            .unwrap()
            .replace("|^^^GLU|5.4|", "|^^^GLU|5.5|")
            .replace("|Fasting sample|", "|Hemolysed&S&slightly|");
        assert_eq!(text, expected);
        assert_eq!(
            message.get("C-4").unwrap().to_string_lossy(),
            "Hemolysed^slightly"
        );
    }

    #[test]
    fn encodes_text_explicitly() {
        let mut message = Message::parse(RESULTS).unwrap();
        message.set_text("P-6.1", "Şahin", WINDOWS_1254).unwrap();
        assert_eq!(message.get("P-6.1").unwrap().raw(), b"\xDEahin");
        assert_eq!(message.get("P-6.1").unwrap().to_text(WINDOWS_1254), "Şahin");
        assert_eq!(
            message.set_text("P-6.1", "Şahin", encoding_rs::WINDOWS_1252),
            Err(PathError::Unencodable("windows-1252"))
        );
    }

    #[test]
    fn adds_and_removes_records() {
        let mut message = Message::parse(b"H|\\^&\rP|1\rL|1").unwrap();
        message
            .insert_record(2, "O")
            .unwrap()
            .set_bytes("2", b"1")
            .unwrap();
        assert_eq!(message.to_bytes(), b"H|\\^&\rP|1\rO|1\rL|1");
        message.push_record("C").unwrap();
        assert_eq!(message.to_bytes(), b"H|\\^&\rP|1\rO|1\rL|1\rC");
        message.remove_record(4).unwrap();
        assert_eq!(message.to_bytes(), b"H|\\^&\rP|1\rO|1\rL|1");
        assert!(message.remove_record(0).is_err());
        assert!(message.push_record("H").is_err());
        assert!(message.push_record("TOOLONG").is_err());
        assert!(message.set_raw("Q-3", b"x").is_err());
    }

    #[test]
    fn creates_new_messages() {
        let mut message = Message::new(Delimiters::default()).unwrap();
        message.set_bytes("H-5", b"OXIM").unwrap();
        message
            .push_record("L")
            .unwrap()
            .set_bytes("2", b"1")
            .unwrap();
        assert_eq!(message.to_bytes(), b"H|\\^&|||OXIM\rL|1\r");
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(Message::parse(b""), Err(ParseError::Empty));
        assert_eq!(Message::parse(b"P|1"), Err(ParseError::MissingHeader));
        assert!(matches!(
            Message::parse(b"H|\\"),
            Err(ParseError::InvalidDelimiters(_))
        ));
        let options = ParseOptions {
            max_records: 2,
            ..ParseOptions::default()
        };
        assert_eq!(
            Message::parse_with(b"H|\\^&\rP|1\rL|1", &options),
            Err(ParseError::LimitExceeded("records"))
        );
    }

    #[test]
    fn standard_terminators_keep_line_feeds_as_data() {
        let options = ParseOptions {
            record_terminators: RecordTerminators::Standard,
            ..ParseOptions::default()
        };
        let input = b"H|\\^&\rC|1|I|line one\nline two|G\rL|1\r";
        let message = Message::parse_with(input, &options).unwrap();
        assert_eq!(message.record_count(), 3);
        assert_eq!(message.get("C-4").unwrap(), "line one\nline two");
        assert_eq!(message.to_bytes(), input);
    }
}
