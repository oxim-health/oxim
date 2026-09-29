//! Transmissions: parsing, serialization and edits.

use std::borrow::Cow;
use std::fmt;

use crate::error::{ParseError, PathError};
use crate::header::{HeaderKind, REQUEST_HEADER_LEN, RESPONSE_HEADER_LEN};
use crate::path::{Path, is_field_id, is_segment_id};

/// The segment separator (`0x1E`); it precedes every segment.
pub const SEGMENT_SEPARATOR: u8 = 0x1E;
/// The group separator (`0x1D`); it precedes every transaction group.
pub const GROUP_SEPARATOR: u8 = 0x1D;
/// The field separator (`0x1C`); it precedes every field.
pub const FIELD_SEPARATOR: u8 = 0x1C;

fn is_separator(b: u8) -> bool {
    matches!(b, SEGMENT_SEPARATOR | GROUP_SEPARATOR | FIELD_SEPARATOR)
}

/// Options for [`Transmission::parse_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseOptions {
    /// The largest number of segments accepted.
    pub max_segments: usize,
    /// The largest number of fields accepted in one segment.
    pub max_fields_per_segment: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            max_segments: 10_000,
            max_fields_per_segment: 1_000,
        }
    }
}

/// Storage for one segment. `fields[0]` holds the bytes between the
/// segment separator and the first field separator (normally none); every
/// other entry is a field: a two-character identifier and its value.
/// `separated` is false for data that follows a group separator without a
/// segment separator, which is kept as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Segment {
    pub(crate) separated: bool,
    pub(crate) fields: Vec<Vec<u8>>,
}

impl Segment {
    fn new(id: &str) -> Self {
        let mut segment = Self {
            separated: true,
            fields: vec![Vec::new()],
        };
        segment.push_field("AM", id.get(2..).unwrap_or_default().as_bytes());
        segment
    }

    fn push_field(&mut self, id: &str, value: &[u8]) {
        let mut field = id.as_bytes().to_vec();
        field.extend_from_slice(value);
        self.fields.push(field);
    }

    fn fields(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.fields
            .iter()
            .skip(1)
            .map(|field| field.split_at(field.len().min(2)))
    }

    /// `AM` plus the value of field `AM`, for example `AM07`.
    fn id(&self) -> Option<String> {
        let (_, value) = self.fields().find(|(id, _)| *id == b"AM")?;
        let id = format!("AM{}", String::from_utf8_lossy(value));
        is_segment_id(&id).then_some(id)
    }

    fn position(&self, field: &str, repetition: usize) -> Option<usize> {
        self.fields
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, f)| f.get(..2) == Some(field.as_bytes()))
            .nth(repetition - 1)
            .map(|(index, _)| index)
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        if self.separated {
            out.push(SEGMENT_SEPARATOR);
        }
        for (i, field) in self.fields.iter().enumerate() {
            if i > 0 {
                out.push(FIELD_SEPARATOR);
            }
            out.extend_from_slice(field);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Item {
    /// A group separator: the start of the next transaction group.
    Group,
    Segment(Segment),
}

/// A read-only view of one segment.
#[derive(Debug, Clone, Copy)]
pub struct SegmentRef<'a> {
    segment: &'a Segment,
    transaction: Option<usize>,
    index: usize,
}

impl<'a> SegmentRef<'a> {
    /// The segment identifier, such as `AM07`, or `None` when the segment
    /// has no valid `AM` field.
    pub fn id(&self) -> Option<String> {
        self.segment.id()
    }

    /// The 0-based transaction group the segment belongs to, or `None` for
    /// transmission-level segments before the first group.
    pub fn transaction(&self) -> Option<usize> {
        self.transaction
    }

    /// The position of the segment among all segments (0-based).
    pub fn index(&self) -> usize {
        self.index
    }

    /// The fields in order as `(identifier, value)` text pairs.
    pub fn fields(&self) -> impl Iterator<Item = (Cow<'a, str>, Cow<'a, str>)> + 'a {
        self.segment
            .fields()
            .map(|(id, value)| (String::from_utf8_lossy(id), String::from_utf8_lossy(value)))
    }

    /// The value of occurrence `repetition` (1-based) of field `id`.
    pub fn field(&self, id: &str, repetition: usize) -> Option<Cow<'a, str>> {
        let position = self.segment.position(id, repetition.max(1))?;
        Some(String::from_utf8_lossy(&self.segment.fields[position][2..]))
    }
}

/// Problems found by [`Transmission::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Issue {
    /// The header's transaction count (`A9`) differs from the number of
    /// transaction groups.
    TransactionCount {
        /// The declared count.
        declared: String,
        /// The number of groups.
        actual: usize,
    },
    /// A segment has no valid segment identification field (`AM`) first.
    MissingSegmentId {
        /// The 0-based segment position.
        segment: usize,
    },
    /// A segment contains data before its first field separator, or data
    /// follows a group separator without a segment separator.
    StrayData {
        /// The 0-based segment position.
        segment: usize,
    },
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TransactionCount { declared, actual } => write!(
                f,
                "the header declares {declared:?} transactions but {actual} groups were found"
            ),
            Self::MissingSegmentId { segment } => {
                write!(
                    f,
                    "segment {} does not start with a segment identifier",
                    segment + 1
                )
            }
            Self::StrayData { segment } => {
                write!(f, "segment {} has data before its first field", segment + 1)
            }
        }
    }
}

/// An NCPDP Telecommunication Standard transmission (version D.0; the 5.1
/// framing is compatible): a fixed-width header followed by segments, with
/// a group separator before each transaction.
///
/// ```text
/// header <SS><FS>AM04<FS>C2…  <GS><SS><FS>AM07<FS>EM1<FS>D2…<SS><FS>AM11…
///        transmission level   transaction 1
/// ```
///
/// The transmission keeps the header and every field as raw bytes, so
/// [`Transmission::to_bytes`] reproduces the parsed input byte for byte
/// until it is edited, and edits only rewrite the fields they touch.
/// Transport framing (for example STX/ETX or length prefixes) must be
/// removed before parsing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Transmission {
    kind: HeaderKind,
    header: Vec<u8>,
    items: Vec<Item>,
}

impl Transmission {
    /// Parses a transmission with the default [`ParseOptions`].
    pub fn parse(input: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with(input, &ParseOptions::default())
    }

    /// Parses a transmission. The header length tells requests (56 bytes)
    /// from responses (31 bytes).
    pub fn parse_with(input: &[u8], options: &ParseOptions) -> Result<Self, ParseError> {
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        let header_len = input
            .iter()
            .position(|&b| b == SEGMENT_SEPARATOR || b == GROUP_SEPARATOR)
            .unwrap_or(input.len());
        let kind = match header_len {
            REQUEST_HEADER_LEN => HeaderKind::Request,
            RESPONSE_HEADER_LEN => HeaderKind::Response,
            other => return Err(ParseError::HeaderLength(other)),
        };
        let mut items = Vec::new();
        let mut segments = 0;
        let mut position = header_len;
        while position < input.len() {
            if input[position] == GROUP_SEPARATOR {
                items.push(Item::Group);
                position += 1;
                continue;
            }
            if segments == options.max_segments {
                return Err(ParseError::LimitExceeded("segments"));
            }
            segments += 1;
            let separated = input[position] == SEGMENT_SEPARATOR;
            let start = if separated { position + 1 } else { position };
            let end = input[start..]
                .iter()
                .position(|&b| b == SEGMENT_SEPARATOR || b == GROUP_SEPARATOR)
                .map_or(input.len(), |at| start + at);
            let mut fields = Vec::new();
            for field in input[start..end].split(|&b| b == FIELD_SEPARATOR) {
                if fields.len() == options.max_fields_per_segment {
                    return Err(ParseError::LimitExceeded("fields per segment"));
                }
                fields.push(field.to_vec());
            }
            items.push(Item::Segment(Segment { separated, fields }));
            position = end;
        }
        Ok(Self {
            kind,
            header: input[..header_len].to_vec(),
            items,
        })
    }

    /// A transmission with a header of `kind` filled with spaces and no
    /// segments.
    pub fn new(kind: HeaderKind) -> Self {
        Self {
            kind,
            header: vec![b' '; kind.header_len()],
            items: Vec::new(),
        }
    }

    /// Serializes the transmission.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.header.clone();
        for item in &self.items {
            match item {
                Item::Group => out.push(GROUP_SEPARATOR),
                Item::Segment(segment) => segment.write_to(&mut out),
            }
        }
        out
    }

    /// Whether this is a request or a response.
    pub fn kind(&self) -> HeaderKind {
        self.kind
    }

    /// The raw header bytes.
    pub fn header_bytes(&self) -> &[u8] {
        &self.header
    }

    /// A header field without its padding spaces, for example `A1` (BIN
    /// number) or `A3` (transaction code). Empty fields are `None`.
    pub fn header_value(&self, id: &str) -> Option<String> {
        let field = self.kind.field(id)?;
        let raw = self.header.get(field.start..field.start + field.len)?;
        let text = String::from_utf8_lossy(raw).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }

    /// Stores `text` in a header field, padded with spaces on the right.
    pub fn set_header(&mut self, id: &str, text: &str) -> Result<(), PathError> {
        let field = self
            .kind
            .field(id)
            .ok_or_else(|| PathError::NoHeaderField(id.to_owned()))?;
        let bytes = text.as_bytes();
        if bytes.len() > field.len {
            return Err(PathError::TooLong(field.len));
        }
        if bytes.iter().any(|&b| is_separator(b)) {
            return Err(PathError::Separator);
        }
        let slot = &mut self.header[field.start..field.start + field.len];
        slot.fill(b' ');
        slot[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// The number of transaction groups.
    pub fn transaction_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item, Item::Group))
            .count()
    }

    /// All segments in order.
    pub fn segments(&self) -> impl Iterator<Item = SegmentRef<'_>> + '_ {
        let mut transaction: Option<usize> = None;
        let mut index = 0;
        self.items.iter().filter_map(move |item| match item {
            Item::Group => {
                transaction = Some(transaction.map_or(0, |t| t + 1));
                None
            }
            Item::Segment(segment) => {
                index += 1;
                Some(SegmentRef {
                    segment,
                    transaction,
                    index: index - 1,
                })
            }
        })
    }

    /// Occurrence `occurrence` (1-based) of segment `id`, for example the
    /// Claim segment (`AM07`) of the second transaction.
    pub fn segment(&self, id: &str, occurrence: usize) -> Option<SegmentRef<'_>> {
        self.segments()
            .filter(|segment| segment.id().as_deref() == Some(id))
            .nth(occurrence.checked_sub(1)?)
    }

    /// The segments of transaction group `transaction` (0-based), or of the
    /// transmission level when `None`.
    pub fn segments_of(
        &self,
        transaction: Option<usize>,
    ) -> impl Iterator<Item = SegmentRef<'_>> + '_ {
        self.segments()
            .filter(move |segment| segment.transaction() == transaction)
    }

    fn item_index(&self, id: &str, occurrence: usize) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, Item::Segment(s) if s.id().as_deref() == Some(id)))
            .nth(occurrence.checked_sub(1)?)
            .map(|(index, _)| index)
    }

    /// The text at `path`, or `None` when the path is invalid or the value
    /// absent. See [`Path`] for the syntax.
    pub fn get(&self, path: &str) -> Option<String> {
        self.get_path(&path.parse().ok()?)
    }

    /// The text at `path`, or `None` when it is absent.
    pub fn get_path(&self, path: &Path) -> Option<String> {
        match path {
            Path::Header { field } => self.header_value(field),
            Path::Field {
                segment,
                occurrence,
                field,
                repetition,
            } => self
                .segment(segment, *occurrence)?
                .field(field, *repetition)
                .map(Cow::into_owned),
        }
    }

    /// Stores `text` at `path`. A missing field is appended to the segment
    /// (as the next occurrence); the segment must exist.
    pub fn set(&mut self, path: &str, text: &str) -> Result<(), PathError> {
        self.set_path(&path.parse()?, text)
    }

    /// Stores `text` at `path`.
    pub fn set_path(&mut self, path: &Path, text: &str) -> Result<(), PathError> {
        match path {
            Path::Header { field } => self.set_header(field, text),
            Path::Field {
                segment,
                occurrence,
                field,
                repetition,
            } => {
                if field == "AM" {
                    return Err(PathError::ReadOnly);
                }
                if text.bytes().any(is_separator) {
                    return Err(PathError::Separator);
                }
                let index = self
                    .item_index(segment, *occurrence)
                    .ok_or_else(|| PathError::NoSegment(path.to_string()))?;
                let Some(Item::Segment(target)) = self.items.get_mut(index) else {
                    return Err(PathError::NoSegment(path.to_string()));
                };
                match target.position(field, *repetition) {
                    Some(position) => {
                        let mut value = field.as_bytes().to_vec();
                        value.extend_from_slice(text.as_bytes());
                        target.fields[position] = value;
                    }
                    None => {
                        let existing = target
                            .fields()
                            .filter(|(id, _)| *id == field.as_bytes())
                            .count();
                        if *repetition != existing + 1 {
                            return Err(PathError::Gap(*repetition));
                        }
                        target.push_field(field, text.as_bytes());
                    }
                }
                Ok(())
            }
        }
    }

    /// Appends a group separator, starting the next transaction group.
    pub fn push_group(&mut self) {
        self.items.push(Item::Group);
    }

    /// Appends a segment `id` (for example `AM07`) with the given fields,
    /// each an identifier and a value.
    pub fn push_segment(&mut self, id: &str, fields: &[(&str, &str)]) -> Result<(), PathError> {
        if !is_segment_id(id) {
            return Err(PathError::Invalid(id.to_owned()));
        }
        let mut segment = Segment::new(id);
        for (field, value) in fields {
            if !is_field_id(field) || *field == "AM" {
                return Err(PathError::Invalid((*field).to_owned()));
            }
            if value.bytes().any(is_separator) {
                return Err(PathError::Separator);
            }
            segment.push_field(field, value.as_bytes());
        }
        self.items.push(Item::Segment(segment));
        Ok(())
    }

    /// Structural problems: a transaction count that differs from the
    /// number of groups and segments without a leading identifier.
    pub fn validate(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let actual = self.transaction_count();
        let declared = self.header_value("A9").unwrap_or_default();
        if declared.parse::<usize>().ok() != Some(actual) {
            issues.push(Issue::TransactionCount { declared, actual });
        }
        for segment in self.segments() {
            if !segment.segment.separated || !segment.segment.fields[0].is_empty() {
                issues.push(Issue::StrayData {
                    segment: segment.index(),
                });
            }
            let first = segment.segment.fields().next();
            let valid = first.is_some_and(|(id, _)| id == b"AM") && segment.id().is_some();
            if !valid {
                issues.push(Issue::MissingSegmentId {
                    segment: segment.index(),
                });
            }
        }
        issues
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A synthetic D.0 billing request: header, Insurance and Patient
    /// segments, one transaction with Claim and Pricing segments.
    pub(crate) fn claim() -> Vec<u8> {
        let mut out = b"999999D0B1PCN1234567".to_vec();
        out.extend_from_slice(b"101");
        out.extend_from_slice(b"1234567893     ");
        out.extend_from_slice(b"20260929");
        out.extend_from_slice(b"SYNTHVEND1");
        let segment = |out: &mut Vec<u8>, fields: &[&str]| {
            out.push(SEGMENT_SEPARATOR);
            for field in fields {
                out.push(FIELD_SEPARATOR);
                out.extend_from_slice(field.as_bytes());
            }
        };
        segment(&mut out, &["AM04", "C2SYN0001", "C1GROUP1", "C301"]);
        segment(
            &mut out,
            &[
                "AM01",
                "CX99",
                "CYPAT-1",
                "C419800101",
                "C52",
                "CAJANE",
                "CBDOE",
            ],
        );
        out.push(GROUP_SEPARATOR);
        segment(
            &mut out,
            &[
                "AM07",
                "EM1",
                "D2000000123456",
                "E103",
                "D700000000001",
                "E730000",
                "D30",
                "D530",
                "D61",
                "D80",
                "DE20260929",
            ],
        );
        segment(&mut out, &["AM11", "D9150{", "DC100{", "DU250{", "DQ250{"]);
        out
    }

    #[test]
    fn round_trips_and_reads_values() {
        let input = claim();
        assert_eq!(input.iter().position(|&b| b == SEGMENT_SEPARATOR), Some(56));
        let t = Transmission::parse(&input).unwrap();
        assert_eq!(t.to_bytes(), input);
        assert_eq!(t.kind(), HeaderKind::Request);
        assert_eq!(t.header_value("A1").as_deref(), Some("999999"));
        assert_eq!(t.header_value("A3").as_deref(), Some("B1"));
        assert_eq!(t.header_value("B1").as_deref(), Some("1234567893"));
        assert_eq!(t.get("HDR.D1").as_deref(), Some("20260929"));
        assert_eq!(t.get("AM07.D2").as_deref(), Some("000000123456"));
        assert_eq!(t.get("AM01.CB").as_deref(), Some("DOE"));
        assert_eq!(t.get("AM07.AM").as_deref(), Some("07"));
        assert_eq!(t.get("AM07[2].D2"), None);
        assert_eq!(t.transaction_count(), 1);
        let claim = t.segment("AM07", 1).unwrap();
        assert_eq!(claim.transaction(), Some(0));
        assert_eq!(t.segment("AM04", 1).unwrap().transaction(), None);
        assert_eq!(t.segments_of(Some(0)).count(), 2);
        assert!(t.validate().is_empty(), "{:?}", t.validate());
    }

    #[test]
    fn edits_fields_and_header() {
        let mut t = Transmission::parse(&claim()).unwrap();
        t.set("AM07.D2", "000000999999").unwrap();
        t.set("AM07.DK", "1").unwrap();
        t.set("HDR.A4", "PCN9").unwrap();
        assert_eq!(t.get("AM07.D2").as_deref(), Some("000000999999"));
        assert_eq!(t.get("AM07.DK").as_deref(), Some("1"));
        assert_eq!(t.header_value("A4").as_deref(), Some("PCN9"));
        assert_eq!(&t.to_bytes()[10..20], b"PCN9      ");
        let reparsed = Transmission::parse(&t.to_bytes()).unwrap();
        assert_eq!(reparsed, t);
        assert_eq!(t.set("AM07.AM", "08"), Err(PathError::ReadOnly));
        assert_eq!(t.set("AM07.D2", "A\u{1c}B"), Err(PathError::Separator));
        assert_eq!(t.set("AM07.D2[3]", "X"), Err(PathError::Gap(3)));
        assert!(matches!(
            t.set("AM09.AB", "X"),
            Err(PathError::NoSegment(_))
        ));
        assert_eq!(t.set("HDR.A1", "1234567"), Err(PathError::TooLong(6)));
        assert!(matches!(
            t.set("HDR.F1", "A"),
            Err(PathError::NoHeaderField(_))
        ));
    }

    #[test]
    fn rejects_unknown_headers_and_reports_issues() {
        assert_eq!(Transmission::parse(b""), Err(ParseError::Empty));
        assert_eq!(
            Transmission::parse(b"SHORT\x1e\x1cAM01"),
            Err(ParseError::HeaderLength(5))
        );
        let mut input = claim();
        input[20] = b'2';
        let t = Transmission::parse(&input).unwrap();
        assert_eq!(
            t.validate(),
            [Issue::TransactionCount {
                declared: "2".into(),
                actual: 1
            }]
        );
    }

    #[test]
    fn builds_transmissions() {
        let mut t = Transmission::new(HeaderKind::Response);
        t.set_header("A2", "D0").unwrap();
        t.set_header("A9", "1").unwrap();
        t.push_group();
        t.push_segment("AM21", &[("AN", "P"), ("F3", "AUTH1")])
            .unwrap();
        let bytes = t.to_bytes();
        assert_eq!(bytes.len(), 31 + 2 + 1 + 5 + 3 + 1 + 7);
        let reparsed = Transmission::parse(&bytes).unwrap();
        assert_eq!(reparsed.get("AM21.AN").as_deref(), Some("P"));
        assert!(reparsed.validate().is_empty());
        assert!(t.push_segment("XX01", &[]).is_err());
        assert!(t.push_segment("AM20", &[("AM", "20")]).is_err());
    }
}
