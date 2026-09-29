use std::fmt;
use std::str::FromStr;

use encoding_rs::{Encoding, UTF_8};
use memchr::{memchr, memchr2};

use crate::charset::encoding_for_charset;
use crate::delimiters::Delimiters;
use crate::error::{CharsetError, ParseError, PathError};
use crate::escape::escape;
use crate::path::{Path, segment_id};
use crate::segment::{LineEnding, Segment, SegmentMut, SegmentRef, is_header_id, set_raw};
use crate::value::Value;

/// Which byte sequences end a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SegmentTerminators {
    /// CR and CRLF, as the standard prescribes. A lone LF is treated as data,
    /// which suits MLLP senders that embed line feeds in text fields.
    Standard,
    /// CR, CRLF and a lone LF. Suits messages read from files.
    #[default]
    Lenient,
}

/// Options for [`Message::parse_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseOptions {
    /// Which byte sequences end a segment.
    pub segment_terminators: SegmentTerminators,
    /// The largest number of segment lines accepted.
    pub max_segments: usize,
    /// The largest number of fields accepted in one segment.
    pub max_fields_per_segment: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            segment_terminators: SegmentTerminators::default(),
            max_segments: 100_000,
            max_fields_per_segment: 10_000,
        }
    }
}

/// An HL7 v2 message in ER7 (pipe-delimited) encoding.
///
/// The message keeps every segment line as raw, escaped bytes together with
/// its original line ending, so [`Message::to_bytes`] reproduces the parsed
/// input byte for byte until the message is edited, and edits only rewrite
/// the values they touch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message {
    delimiters: Delimiters,
    lines: Vec<Segment>,
}

impl Message {
    /// Parses a message with the default [`ParseOptions`].
    pub fn parse(input: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with(input, &ParseOptions::default())
    }

    /// Parses a message.
    ///
    /// The input must start with `MSH`. Segment identifiers and message
    /// structure are not validated here, so messages from non-conforming
    /// senders can still be read and repaired.
    pub fn parse_with(input: &[u8], options: &ParseOptions) -> Result<Self, ParseError> {
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        if !input.starts_with(b"MSH") {
            return Err(ParseError::MissingHeader);
        }
        let delimiters = Delimiters::from_header(input)?;
        let mut lines = Vec::new();
        for (line, line_ending) in Lines::new(input, options.segment_terminators) {
            if lines.len() == options.max_segments {
                return Err(ParseError::LimitExceeded("segments"));
            }
            let mut fields = Vec::new();
            for field in line.split(|&b| b == delimiters.field) {
                if fields.len() == options.max_fields_per_segment.saturating_add(1) {
                    return Err(ParseError::LimitExceeded("fields per segment"));
                }
                fields.push(field.to_vec());
            }
            lines.push(Segment {
                fields,
                line_ending,
            });
        }
        let message = Self { delimiters, lines };
        if let Err(CharsetError::Unsupported(name)) = message.declared_encoding() {
            return Err(ParseError::UnsupportedCharset(name));
        }
        Ok(message)
    }

    /// Creates a message containing only an MSH segment with the given
    /// delimiters.
    pub fn new(delimiters: Delimiters) -> Result<Self, ParseError> {
        delimiters.validate()?;
        Ok(Self {
            delimiters,
            lines: vec![Segment {
                fields: vec![b"MSH".to_vec(), delimiters.encoding_characters()],
                line_ending: LineEnding::Cr,
            }],
        })
    }

    /// Serializes the message.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.lines.iter().map(|l| l.fields.len() * 8).sum());
        self.write_to(&mut out);
        out
    }

    /// Appends the serialized message to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        for line in &self.lines {
            line.write_to(self.delimiters.field, out);
        }
    }

    /// The delimiters declared in MSH-1 and MSH-2.
    pub fn delimiters(&self) -> &Delimiters {
        &self.delimiters
    }

    /// The MSH segment.
    pub fn header(&self) -> SegmentRef<'_> {
        self.segments()
            .next()
            .unwrap_or_else(|| SegmentRef::new(&self.lines[0], &self.delimiters, 0))
    }

    /// The segments in order, skipping blank lines.
    pub fn segments(&self) -> impl Iterator<Item = SegmentRef<'_>> + '_ {
        self.lines
            .iter()
            .filter(|line| !line.is_blank())
            .enumerate()
            .map(|(index, line)| SegmentRef::new(line, &self.delimiters, index))
    }

    /// The number of segments, not counting blank lines.
    pub fn segment_count(&self) -> usize {
        self.lines.iter().filter(|line| !line.is_blank()).count()
    }

    /// The segments with identifier `id`, in order.
    pub fn segments_named<'a>(&'a self, id: &'a str) -> impl Iterator<Item = SegmentRef<'a>> + 'a {
        self.segments().filter(move |segment| segment.is(id))
    }

    /// The 1-based `occurrence` of segment `id`.
    pub fn segment(&self, id: &str, occurrence: usize) -> Option<SegmentRef<'_>> {
        self.segments()
            .filter(|segment| segment.is(id))
            .nth(occurrence.checked_sub(1)?)
    }

    /// A mutable view of the 1-based `occurrence` of segment `id`.
    pub fn segment_mut(&mut self, id: &str, occurrence: usize) -> Option<SegmentMut<'_>> {
        let index = self.segment(id, occurrence)?.index();
        self.segment_at_mut(index)
    }

    /// A mutable view of the segment at position `index` of
    /// [`Message::segments`].
    pub fn segment_at_mut(&mut self, index: usize) -> Option<SegmentMut<'_>> {
        let line = self.line_index(index)?;
        Some(SegmentMut::new(
            &mut self.lines[line],
            &self.delimiters,
            index,
        ))
    }

    /// The value at a path such as `PID-5.1` or `OBX[2]-5`. Returns `None`
    /// when the path is invalid or the value is absent; parse a [`Path`] to
    /// distinguish the two.
    pub fn get(&self, path: &str) -> Option<Value<'_>> {
        self.get_path(&path.parse().ok()?)
    }

    /// The value at `path`, or `None` when it is absent.
    pub fn get_path(&self, path: &Path) -> Option<Value<'_>> {
        let id = std::str::from_utf8(path.segment_id()).ok()?;
        self.segment(id, path.occurrence())?
            .get_path(path.field_path())
    }

    /// Stores `text` at `path`, encoding it in the message character set
    /// (MSH-18, or UTF-8 when none is declared) and escaping delimiters.
    pub fn set(&mut self, path: &str, text: &str) -> Result<(), PathError> {
        let encoding = self.declared_encoding()?.unwrap_or(UTF_8);
        let (bytes, _, unmappable) = encoding.encode(text);
        if unmappable {
            return Err(PathError::Unencodable(encoding.name()));
        }
        self.set_bytes(path, &bytes)
    }

    /// Escapes `bytes`, which must already be in the message character set,
    /// and stores them at `path`.
    pub fn set_bytes(&mut self, path: &str, bytes: &[u8]) -> Result<(), PathError> {
        let raw = escape(bytes, &self.delimiters)?.into_owned();
        self.set_raw(path, &raw)
    }

    /// Stores already-escaped `raw` bytes at `path`. Delimiters inside `raw`
    /// keep their structural meaning.
    pub fn set_raw(&mut self, path: &str, raw: &[u8]) -> Result<(), PathError> {
        self.set_raw_path(&path.parse()?, raw)
    }

    /// Stores already-escaped `raw` bytes at `path`.
    pub fn set_raw_path(&mut self, path: &Path, raw: &[u8]) -> Result<(), PathError> {
        let id = String::from_utf8_lossy(path.segment_id()).into_owned();
        let index = self
            .segment(&id, path.occurrence())
            .ok_or_else(|| PathError::SegmentNotFound(path.to_string()))?
            .index();
        let line = self
            .line_index(index)
            .ok_or_else(|| PathError::SegmentNotFound(path.to_string()))?;
        set_raw(
            &mut self.lines[line],
            &self.delimiters,
            path.field_path(),
            raw,
        )
    }

    /// Appends an empty segment and returns a mutable view of it.
    pub fn push_segment(&mut self, id: &str) -> Result<SegmentMut<'_>, PathError> {
        let count = self.segment_count();
        self.insert_segment(count, id)
    }

    /// Inserts an empty segment so it becomes position `index` of
    /// [`Message::segments`]. Position 0 is reserved for MSH.
    pub fn insert_segment(&mut self, index: usize, id: &str) -> Result<SegmentMut<'_>, PathError> {
        let id_bytes = segment_id(id.as_bytes())
            .filter(|id| !is_header_id(id))
            .ok_or_else(|| PathError::InvalidSegmentId(id.to_owned()))?;
        let count = self.segment_count();
        if index == 0 || index > count {
            return Err(PathError::IndexOutOfRange { max: count });
        }
        let ending = self.preferred_line_ending();
        let line = match self.line_index(index) {
            Some(line) => line,
            None => {
                // Appending: the previous last line must now be terminated.
                if let Some(last) = self.lines.last_mut()
                    && last.line_ending == LineEnding::None
                {
                    last.line_ending = ending;
                    self.lines.push(Segment::new(&id_bytes, LineEnding::None));
                    return self
                        .segment_at_mut(index)
                        .ok_or(PathError::IndexOutOfRange { max: count });
                }
                self.lines.len()
            }
        };
        self.lines.insert(line, Segment::new(&id_bytes, ending));
        self.segment_at_mut(index)
            .ok_or(PathError::IndexOutOfRange { max: count })
    }

    /// Removes the segment at position `index` of [`Message::segments`].
    /// The MSH segment cannot be removed.
    pub fn remove_segment(&mut self, index: usize) -> Result<(), PathError> {
        let count = self.segment_count();
        if index == 0 || index >= count {
            return Err(PathError::IndexOutOfRange {
                max: count.saturating_sub(1),
            });
        }
        let line = self
            .line_index(index)
            .ok_or(PathError::IndexOutOfRange { max: count })?;
        let removed = self.lines.remove(line);
        // Keep the "no terminator after the last segment" style.
        if removed.line_ending == LineEnding::None
            && line == self.lines.len()
            && let Some(last) = self.lines.last_mut()
        {
            last.line_ending = LineEnding::None;
        }
        Ok(())
    }

    /// MSH-9, split into message code, trigger event and message structure.
    pub fn message_type(&self) -> Option<MessageType> {
        let value = self.header().field(9)?;
        let part = |i| {
            value
                .component(i)
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        Some(MessageType {
            code: part(1),
            trigger: part(2),
            structure: part(3),
        })
    }

    /// MSH-10, the message control ID.
    pub fn control_id(&self) -> Option<Value<'_>> {
        self.header().field(10)
    }

    /// MSH-12.1, the HL7 version, if present and well formed.
    pub fn version(&self) -> Option<Version> {
        let value = self.header().field(12)?.component(1)?;
        std::str::from_utf8(value.raw()).ok()?.parse().ok()
    }

    /// The character set declared in the first repetition of MSH-18, or
    /// `None` when MSH-18 is empty.
    pub fn declared_encoding(&self) -> Result<Option<&'static Encoding>, CharsetError> {
        let Some(value) = self.header().field(18).and_then(|v| v.repetition(1)) else {
            return Ok(None);
        };
        if value.raw().trim_ascii().is_empty() {
            return Ok(None);
        }
        encoding_for_charset(value.raw()).map(Some)
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
    /// Writes the message as UTF-8 (lossy), with segments separated by
    /// newlines for readability.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.segments().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            let mut bytes = segment.to_bytes();
            bytes.truncate(bytes.len() - segment.line_ending().as_bytes().len());
            f.write_str(&String::from_utf8_lossy(&bytes))?;
        }
        Ok(())
    }
}

/// The message type from MSH-9.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct MessageType {
    /// Message code, for example `ORU`.
    pub code: String,
    /// Trigger event, for example `R01`.
    pub trigger: String,
    /// Message structure, for example `ORU_R01` (HL7 v2.3.1+).
    pub structure: String,
}

/// An HL7 version such as `2.5.1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major version, `2` for HL7 v2.
    pub major: u8,
    /// Minor version.
    pub minor: u8,
    /// Patch version, `0` when absent.
    pub patch: u8,
}

impl Version {
    /// Creates a version.
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

/// Returned when a version string is not `major.minor[.patch]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidVersion;

impl fmt::Display for InvalidVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid HL7 version")
    }
}

impl std::error::Error for InvalidVersion {}

impl FromStr for Version {
    type Err = InvalidVersion;

    fn from_str(s: &str) -> Result<Self, InvalidVersion> {
        let mut parts = s.trim().split('.');
        let mut next = |required: bool| match parts.next() {
            Some(part) => part.parse::<u8>().map_err(|_| InvalidVersion),
            None if required => Err(InvalidVersion),
            None => Ok(0),
        };
        let version = Self::new(next(true)?, next(true)?, next(false)?);
        if parts.next().is_some() {
            return Err(InvalidVersion);
        }
        Ok(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)?;
        if self.patch != 0 {
            write!(f, ".{}", self.patch)?;
        }
        Ok(())
    }
}

/// Splits input into lines and their endings.
struct Lines<'a> {
    rest: &'a [u8],
    mode: SegmentTerminators,
}

impl<'a> Lines<'a> {
    fn new(input: &'a [u8], mode: SegmentTerminators) -> Self {
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
            SegmentTerminators::Standard => memchr(b'\r', self.rest),
            SegmentTerminators::Lenient => memchr2(b'\r', b'\n', self.rest),
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
    use super::*;

    const ORU: &[u8] =
        b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01^ORU_R01|MSG0001|P|2.5.1\r\
PID|1||12345^^^HOSP^MR||Doe^Jane||19800101|F\r\
OBR|1|ORD1|SMP1|GLU^Glucose^L\r\
OBX|1|NM|GLU^Glucose^L||5.4|mmol/L|3.9-6.1|N|||F\r\
OBX|2|NM|HGB^Hemoglobin^L||13.2|g/dL|12-16|N|||F\r";

    #[test]
    fn round_trips_byte_for_byte() {
        for input in [
            ORU,
            b"MSH|^~\\&|A\nPID|1\n",
            b"MSH|^~\\&|A\r\nPID|1\r\n",
            b"MSH|^~\\&|A\rPID|1",
            b"MSH|^~\\&|A\r\rPID|1|||\r\r",
            b"MSH|^~\\&|A|||||||||||||||\r",
            b"MSH|^~\\&|A\rZZ1|\xff\xfe|x\r",
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
    fn reads_values() {
        let message = Message::parse(ORU).unwrap();
        assert_eq!(message.get("PID-5.1").unwrap(), "Doe");
        assert_eq!(message.get("PID-3.4").unwrap(), "HOSP");
        assert_eq!(message.get("OBX[2]-5").unwrap(), "13.2");
        assert_eq!(message.get("MSH-9.2").unwrap(), "R01");
        assert!(message.get("OBX[3]-5").is_none());
        assert!(message.get("not a path").is_none());
        assert_eq!(message.segment_count(), 5);
        assert_eq!(message.segments_named("OBX").count(), 2);
        let kind = message.message_type().unwrap();
        assert_eq!((kind.code.as_str(), kind.trigger.as_str()), ("ORU", "R01"));
        assert_eq!(message.control_id().unwrap(), "MSG0001");
        assert_eq!(message.version(), Some(Version::new(2, 5, 1)));
    }

    /// An MSH segment whose MSH-18 (character set) is `charset`.
    fn header_with_charset(charset: &str) -> String {
        format!("MSH|^~\\&{}{charset}\r", "|".repeat(16))
    }

    #[test]
    fn edits_only_what_it_touches() {
        let mut message = Message::parse(ORU).unwrap();
        message.set("PID-5.2", "Janet^Marie").unwrap();
        let expected = String::from_utf8(ORU.to_vec())
            .unwrap()
            .replace("||Doe^Jane||", "||Doe^Janet\\S\\Marie||");
        assert_eq!(String::from_utf8(message.to_bytes()).unwrap(), expected);
        assert_eq!(
            message.get("PID-5.2").unwrap().to_string_lossy(),
            "Janet^Marie"
        );
    }

    #[test]
    fn adds_and_removes_segments() {
        let mut message = Message::parse(b"MSH|^~\\&|A\rPID|1").unwrap();
        message
            .push_segment("NTE")
            .unwrap()
            .set_bytes("3", b"note")
            .unwrap();
        assert_eq!(message.to_bytes(), b"MSH|^~\\&|A\rPID|1\rNTE|||note");
        message.insert_segment(1, "EVN").unwrap();
        assert_eq!(message.to_bytes(), b"MSH|^~\\&|A\rEVN\rPID|1\rNTE|||note");
        message.remove_segment(3).unwrap();
        assert_eq!(message.to_bytes(), b"MSH|^~\\&|A\rEVN\rPID|1");
        assert!(message.remove_segment(0).is_err());
        assert!(message.insert_segment(0, "PID").is_err());
        assert!(message.push_segment("MSH").is_err());
        assert!(message.push_segment("TOOLONG").is_err());
        assert!(message.set("OBX-5", "x").is_err());
    }

    #[test]
    fn creates_new_messages() {
        let mut message = Message::new(Delimiters::default()).unwrap();
        message.set("MSH-9", "ACK").unwrap();
        let expected = format!("MSH|^~\\&{}ACK\r", "|".repeat(7));
        assert_eq!(message.to_bytes(), expected.as_bytes());
        assert_eq!(message.get("MSH-9").unwrap(), "ACK");
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(Message::parse(b""), Err(ParseError::Empty));
        assert_eq!(Message::parse(b"PID|1"), Err(ParseError::MissingHeader));
        assert!(matches!(
            Message::parse(b"MSH|"),
            Err(ParseError::InvalidDelimiters(_))
        ));
        assert_eq!(
            Message::parse(header_with_charset("BIG-5").as_bytes()),
            Err(ParseError::UnsupportedCharset("BIG-5".into()))
        );
    }

    #[test]
    fn enforces_limits() {
        let options = ParseOptions {
            max_segments: 2,
            ..ParseOptions::default()
        };
        assert_eq!(
            Message::parse_with(b"MSH|^~\\&\rPID\rPV1", &options),
            Err(ParseError::LimitExceeded("segments"))
        );
        let options = ParseOptions {
            max_fields_per_segment: 3,
            ..ParseOptions::default()
        };
        assert!(Message::parse_with(b"MSH|^~\\&|1|2", &options).is_ok());
        assert_eq!(
            Message::parse_with(b"MSH|^~\\&|1|2|3", &options),
            Err(ParseError::LimitExceeded("fields per segment"))
        );
    }

    #[test]
    fn standard_terminators_keep_line_feeds_as_data() {
        let options = ParseOptions {
            segment_terminators: SegmentTerminators::Standard,
            ..ParseOptions::default()
        };
        let input = b"MSH|^~\\&|A\rNTE|1||line one\nline two\r";
        let message = Message::parse_with(input, &options).unwrap();
        assert_eq!(message.segment_count(), 2);
        assert_eq!(message.get("NTE-3").unwrap(), "line one\nline two");
        assert_eq!(message.to_bytes(), input);
    }

    #[test]
    fn encodes_text_in_declared_charset() {
        let turkish = header_with_charset("8859/9") + "PID|1\r";
        let mut message = Message::parse(turkish.as_bytes()).unwrap();
        message.set("PID-5.1", "Şahin").unwrap();
        assert_eq!(message.get("PID-5.1").unwrap().raw(), b"\xDEahin");
        let encoding = message.declared_encoding().unwrap().unwrap();
        assert_eq!(message.get("PID-5.1").unwrap().to_text(encoding), "Şahin");
        let latin1 = header_with_charset("8859/1") + "PID|1\r";
        let mut ascii = Message::parse(latin1.as_bytes()).unwrap();
        assert_eq!(
            ascii.set("PID-5.1", "Şahin"),
            Err(PathError::Unencodable("windows-1252"))
        );
    }

    #[test]
    fn parses_versions() {
        assert_eq!("2.3".parse(), Ok(Version::new(2, 3, 0)));
        assert_eq!("2.5.1".parse(), Ok(Version::new(2, 5, 1)));
        assert!("2".parse::<Version>().is_err());
        assert!("2.5.1.1".parse::<Version>().is_err());
        assert!("v2".parse::<Version>().is_err());
        assert!(Version::new(2, 3, 1) < Version::new(2, 4, 0));
        assert_eq!(Version::new(2, 5, 1).to_string(), "2.5.1");
    }

    #[test]
    fn displays_readably() {
        let message = Message::parse(b"MSH|^~\\&|A\r\nPID|1\r\n").unwrap();
        assert_eq!(message.to_string(), "MSH|^~\\&|A\nPID|1");
    }
}
