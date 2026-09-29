//! Interchanges: parsing, serialization and edits.

use crate::delimiters::Delimiters;
use crate::envelope::{Envelope, Issue};
use crate::error::{ParseError, PathError};
use crate::path::{Path, is_segment_id};
use crate::segment::{Segment, SegmentMut, SegmentRef, set_raw};
use crate::value::Value;

/// Options for [`Interchange::parse_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseOptions {
    /// Reject interchanges whose envelope has issues (missing trailers,
    /// mismatched control numbers or counts). Lenient parsing keeps them;
    /// [`Interchange::validate`] reports them.
    pub strict: bool,
    /// The largest number of segments accepted.
    pub max_segments: usize,
    /// The largest number of elements accepted in one segment.
    pub max_elements_per_segment: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            strict: false,
            max_segments: 1_000_000,
            max_elements_per_segment: 1_000,
        }
    }
}

impl ParseOptions {
    /// Options that reject envelope issues.
    pub fn strict() -> Self {
        Self {
            strict: true,
            ..Self::default()
        }
    }
}

/// An ASC X12 interchange (`ISA` … `IEA`), or several concatenated.
///
/// Every segment is kept as raw elements together with its terminator and
/// the line breaks that follow it, so [`Interchange::to_bytes`] reproduces
/// the parsed input byte for byte until it is edited, and edits only
/// rewrite the elements they touch. Whitespace and a UTF-8 byte order mark
/// before `ISA` are kept as well.
///
/// X12 has no escape mechanism: values containing a delimiter cannot be
/// written and are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Interchange {
    prefix: Vec<u8>,
    delimiters: Delimiters,
    segments: Vec<Segment>,
}

fn is_line_space(b: u8) -> bool {
    matches!(b, b'\r' | b'\n' | b' ' | b'\t')
}

impl Interchange {
    /// Parses leniently with the default [`ParseOptions`].
    pub fn parse(input: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with(input, &ParseOptions::default())
    }

    /// Parses an interchange. The delimiters come from the leading `ISA`
    /// segment.
    pub fn parse_with(input: &[u8], options: &ParseOptions) -> Result<Self, ParseError> {
        let mut start = 0;
        if input.starts_with(b"\xEF\xBB\xBF") {
            start = 3;
        }
        while input.get(start).is_some_and(|&b| is_line_space(b)) {
            start += 1;
        }
        let body = &input[start..];
        if body.is_empty() {
            return Err(ParseError::Empty);
        }
        let delimiters = Delimiters::from_isa(body)?;
        let mut segments = Vec::new();
        let mut position = 0;
        while position < body.len() {
            if segments.len() == options.max_segments {
                return Err(ParseError::LimitExceeded("segments"));
            }
            let (content, terminated, mut next) =
                match memchr::memchr(delimiters.segment, &body[position..]) {
                    Some(at) => (&body[position..position + at], true, position + at + 1),
                    None => (&body[position..], false, body.len()),
                };
            let suffix_start = next;
            if terminated {
                while body.get(next).is_some_and(|&b| is_line_space(b)) {
                    next += 1;
                }
            }
            let mut elements = Vec::new();
            for element in content.split(|&b| b == delimiters.element) {
                if elements.len() == options.max_elements_per_segment {
                    return Err(ParseError::LimitExceeded("elements per segment"));
                }
                elements.push(element.to_vec());
            }
            segments.push(Segment {
                elements,
                terminated,
                suffix: body[suffix_start..next].to_vec(),
            });
            position = next;
        }
        let interchange = Self {
            prefix: input[..start].to_vec(),
            delimiters,
            segments,
        };
        if options.strict {
            let issues = interchange.validate();
            if !issues.is_empty() {
                return Err(ParseError::Envelope(issues));
            }
        }
        Ok(interchange)
    }

    /// Builds an interchange from segments given as element texts, where
    /// the first element of each is the segment identifier. The first
    /// segment must be `ISA`; its elements 11 (when the delimiters include
    /// a repetition separator) and 16 are set from `delimiters`. Element
    /// texts may contain component and repetition separators but not the
    /// element separator or segment terminator. Each segment is followed by
    /// `line_break` (for example `b"\n"`, or empty).
    pub fn from_elements<S: AsRef<str>>(
        delimiters: Delimiters,
        segments: &[Vec<S>],
        line_break: &[u8],
    ) -> Result<Self, PathError> {
        delimiters
            .validate()
            .map_err(|_| PathError::Unsupported("invalid delimiters"))?;
        if !line_break.iter().all(|&b| is_line_space(b)) {
            return Err(PathError::Unsupported("the line break must be whitespace"));
        }
        let mut out = Vec::with_capacity(segments.len());
        for (index, elements) in segments.iter().enumerate() {
            let id = elements.first().map_or("", AsRef::as_ref);
            if (index == 0) != (id == "ISA") || !is_segment_id(id) {
                return Err(PathError::Invalid(id.to_owned()));
            }
            let mut raw: Vec<Vec<u8>> = Vec::with_capacity(elements.len());
            for text in elements {
                let text = text.as_ref().as_bytes();
                if let Some(&b) = text
                    .iter()
                    .find(|&&b| b == delimiters.element || b == delimiters.segment)
                {
                    return Err(PathError::Delimiter(char::from(b)));
                }
                raw.push(text.to_vec());
            }
            if index == 0 {
                raw.resize(17, Vec::new());
                raw.truncate(17);
                if let Some(repetition) = delimiters.repetition {
                    raw[11] = vec![repetition];
                }
                raw[16] = vec![delimiters.component];
            }
            out.push(Segment {
                elements: raw,
                terminated: true,
                suffix: line_break.to_vec(),
            });
        }
        if out.is_empty() {
            return Err(PathError::Invalid("no segments".into()));
        }
        Ok(Self {
            prefix: Vec::new(),
            delimiters,
            segments: out,
        })
    }

    /// Serializes the interchange.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out);
        out
    }

    /// Appends the serialized interchange to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.prefix);
        for segment in &self.segments {
            segment.write_to(&self.delimiters, out);
        }
    }

    /// The delimiters declared by `ISA`.
    pub fn delimiters(&self) -> &Delimiters {
        &self.delimiters
    }

    /// The `ISA` segment.
    pub fn header(&self) -> SegmentRef<'_> {
        // Parsing and building guarantee a leading ISA segment.
        SegmentRef::new(&self.segments[0], &self.delimiters, 0)
    }

    /// All segments in order.
    pub fn segments(&self) -> impl Iterator<Item = SegmentRef<'_>> + '_ {
        self.segments
            .iter()
            .enumerate()
            .map(|(index, segment)| SegmentRef::new(segment, &self.delimiters, index))
    }

    /// The number of segments.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// The segment at `index` (0-based).
    pub fn segment_at(&self, index: usize) -> Option<SegmentRef<'_>> {
        self.segments
            .get(index)
            .map(|segment| SegmentRef::new(segment, &self.delimiters, index))
    }

    /// Occurrence `occurrence` (1-based) of the segments with identifier
    /// `id`.
    pub fn segment(&self, id: &str, occurrence: usize) -> Option<SegmentRef<'_>> {
        self.segments()
            .filter(|segment| segment.is(id))
            .nth(occurrence.checked_sub(1)?)
    }

    /// The segments with identifier `id`.
    pub fn segments_named<'a>(&'a self, id: &'a str) -> impl Iterator<Item = SegmentRef<'a>> + 'a {
        self.segments().filter(move |segment| segment.is(id))
    }

    fn index_of(&self, id: &str, occurrence: usize) -> Option<usize> {
        self.segment(id, occurrence).map(|segment| segment.index())
    }

    /// A mutable view of the segment at `index`.
    pub fn segment_at_mut(&mut self, index: usize) -> Option<SegmentMut<'_>> {
        let delimiters = &self.delimiters;
        self.segments
            .get_mut(index)
            .map(|segment| SegmentMut::new(segment, delimiters, index))
    }

    /// The value at `path`, or `None` when the path is invalid or the value
    /// is absent. See [`Path`] for the syntax.
    pub fn get(&self, path: &str) -> Option<Value<'_>> {
        self.get_path(&path.parse().ok()?)
    }

    /// The value at `path`, or `None` when it is absent.
    pub fn get_path(&self, path: &Path) -> Option<Value<'_>> {
        let segment = self.segment(path.segment_id(), path.occurrence_index())?;
        let mut value = segment.element(path.element_index())?;
        if let Some(repetition) = path.repetition_index() {
            value = value.repetition(repetition)?;
        }
        if let Some(component) = path.component_index() {
            value = value.component(component)?;
        }
        Some(value)
    }

    /// Stores `text` at `path`. The text must not contain a delimiter; the
    /// segment must exist.
    pub fn set(&mut self, path: &str, text: &str) -> Result<(), PathError> {
        self.delimiters.check_text(text.as_bytes())?;
        self.set_raw_path(&path.parse()?, text.as_bytes())
    }

    /// Stores `raw` at `path`; component and repetition separators inside
    /// `raw` keep their meaning.
    pub fn set_raw(&mut self, path: &str, raw: &[u8]) -> Result<(), PathError> {
        self.set_raw_path(&path.parse()?, raw)
    }

    /// Stores `raw` at `path`.
    pub fn set_raw_path(&mut self, path: &Path, raw: &[u8]) -> Result<(), PathError> {
        let index = self
            .index_of(path.segment_id(), path.occurrence_index())
            .ok_or_else(|| PathError::NoSegment(path.to_string()))?;
        set_raw(
            &mut self.segments[index],
            &self.delimiters,
            path.element_index(),
            path.repetition_index(),
            path.component_index(),
            raw,
        )
    }

    /// Inserts an empty segment `id` at `index` and returns it. The new
    /// segment takes the line break of the segment before it.
    pub fn insert_segment(&mut self, index: usize, id: &str) -> Result<SegmentMut<'_>, PathError> {
        if !is_segment_id(id) || id == "ISA" {
            return Err(PathError::Invalid(id.to_owned()));
        }
        if index == 0 || index > self.segments.len() {
            return Err(PathError::Invalid(format!("segment position {index}")));
        }
        let before = &mut self.segments[index - 1];
        before.terminated = true;
        let suffix = before.suffix.clone();
        self.segments.insert(
            index,
            Segment {
                elements: vec![id.as_bytes().to_vec()],
                terminated: true,
                suffix,
            },
        );
        let delimiters = &self.delimiters;
        Ok(SegmentMut::new(
            &mut self.segments[index],
            delimiters,
            index,
        ))
    }

    /// Appends an empty segment `id` and returns it.
    pub fn push_segment(&mut self, id: &str) -> Result<SegmentMut<'_>, PathError> {
        let index = self.segments.len();
        self.insert_segment(index, id)
    }

    /// Removes the segment at `index`. The leading `ISA` cannot be removed.
    pub fn remove_segment(&mut self, index: usize) -> Result<(), PathError> {
        if index == 0 || index >= self.segments.len() {
            return Err(PathError::Invalid(format!("segment position {index}")));
        }
        self.segments.remove(index);
        Ok(())
    }

    /// The envelope structure: interchanges, functional groups and
    /// transaction sets.
    pub fn envelope(&self) -> Envelope {
        Envelope::of(self).0
    }

    /// Envelope issues: missing trailers, control numbers that differ
    /// between header and trailer, wrong counts and segments outside a
    /// transaction set.
    pub fn validate(&self) -> Vec<Issue> {
        Envelope::of(self).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const SAMPLE: &[u8] = b"ISA*00*          *00*          *ZZ*SUBMITTER      *ZZ*RECEIVER       *260929*1200*^*00501*000000001*0*T*:~\n\
GS*HC*SUBMITTER*RECEIVER*20260929*1200*1*X*005010X222A1~\n\
ST*837*0001*005010X222A1~\n\
BHT*0019*00*REF47517*20260929*1200*CH~\n\
NM1*41*2*SAMPLE CLINIC*****46*TGJ23~\n\
NM1*IL*1*DOE*JANE****MI*SYN12345~\n\
CLM*A37YH556*21***11:B:1*Y*A*Y*I~\n\
HI*ABK:I10^ABF:E119~\n\
SE*7*0001~\n\
GE*1*1~\n\
IEA*1*000000001~\n";

    #[test]
    fn round_trips_and_reads_values() {
        let x = Interchange::parse(SAMPLE).unwrap();
        assert_eq!(x.to_bytes(), SAMPLE);
        assert_eq!(x.segment_count(), 11);
        assert_eq!(x.get("NM1[2]-3").unwrap(), "DOE");
        assert_eq!(x.get("NM103").unwrap(), "SAMPLE CLINIC");
        assert_eq!(x.get("CLM05-01").unwrap(), "11");
        assert_eq!(x.get("CLM-5.3").unwrap(), "1");
        assert_eq!(x.get("HI-1[2].2").unwrap(), "E119");
        assert_eq!(x.get("ST-0").unwrap(), "ST");
        assert!(x.get("NM1[3]-3").is_none());
        assert!(x.validate().is_empty(), "{:?}", x.validate());
        assert_eq!(x.header().trimmed(6).as_deref(), Some("SUBMITTER"));
    }

    #[test]
    fn keeps_prefix_and_unterminated_tails() {
        let mut input = b"\xEF\xBB\xBF\r\n".to_vec();
        input.extend_from_slice(&SAMPLE[..SAMPLE.len() - 2]);
        let x = Interchange::parse(&input).unwrap();
        assert_eq!(x.to_bytes(), input);
        assert!(!x.segments().last().unwrap().is_terminated());
    }

    #[test]
    fn edits_only_the_touched_values() {
        let mut x = Interchange::parse(SAMPLE).unwrap();
        x.set("NM1[2]-4", "JANET").unwrap();
        x.set("CLM05-02", "A").unwrap();
        let text = String::from_utf8(x.to_bytes()).unwrap();
        assert!(text.contains("NM1*IL*1*DOE*JANET****MI*SYN12345~\n"));
        assert!(text.contains("CLM*A37YH556*21***11:A:1*Y"));
        assert_eq!(x.set("NM1-3", "A*B"), Err(PathError::Delimiter('*')));
        assert_eq!(x.set("NM1-3", "A:B"), Err(PathError::Delimiter(':')));
        x.set_raw("NM1-3", b"A:B").unwrap();
        assert!(matches!(x.set("N4-1", "X"), Err(PathError::NoSegment(_))));
        assert_eq!(x.set("ISA-16", ">"), Err(PathError::ReadOnly));
    }

    #[test]
    fn inserts_and_removes_segments() {
        let mut x = Interchange::parse(SAMPLE).unwrap();
        let se = x.segment("SE", 1).unwrap().index();
        x.insert_segment(se, "REF").unwrap().set(1, "EA").unwrap();
        assert!(x.to_bytes().windows(8).any(|w| w == b"REF*EA~\n"));
        assert_eq!(x.validate().len(), 1);
        x.remove_segment(se).unwrap();
        assert!(x.validate().is_empty());
        assert!(x.remove_segment(0).is_err());
        assert!(x.insert_segment(1, "isa").is_err());
    }

    #[test]
    fn builds_interchanges_from_elements() {
        let isa = vec![
            "ISA",
            "00",
            "          ",
            "00",
            "          ",
            "ZZ",
            "A              ",
            "ZZ",
            "B              ",
            "260929",
            "1200",
            "",
            "00501",
            "000000002",
            "0",
            "T",
            "",
        ];
        let x = Interchange::from_elements(
            Delimiters::default(),
            &[
                isa,
                vec!["TA1", "000000001", "260929", "1200", "A", "000"],
                vec!["IEA", "0", "000000002"],
            ],
            b"\n",
        )
        .unwrap();
        let bytes = x.to_bytes();
        assert!(bytes.starts_with(b"ISA*00*"));
        assert!(bytes.ends_with(
            b"*^*00501*000000002*0*T*:~\nTA1*000000001*260929*1200*A*000~\nIEA*0*000000002~\n"
        ));
        assert_eq!(Interchange::parse(&bytes).unwrap(), x);
        assert!(Interchange::from_elements(Delimiters::default(), &[vec!["GS"]], b"").is_err());
    }

    #[test]
    fn rejects_non_interchanges() {
        assert_eq!(Interchange::parse(b""), Err(ParseError::Empty));
        assert_eq!(Interchange::parse(b" \n"), Err(ParseError::Empty));
        assert_eq!(Interchange::parse(b"GS*HC~"), Err(ParseError::MissingIsa));
        let mut broken = SAMPLE.to_vec();
        broken.truncate(broken.len() - 17);
        assert!(Interchange::parse(&broken).is_ok());
        assert!(matches!(
            Interchange::parse_with(&broken, &ParseOptions::strict()),
            Err(ParseError::Envelope(_))
        ));
    }
}
