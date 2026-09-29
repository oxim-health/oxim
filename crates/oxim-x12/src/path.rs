//! Addresses of values inside an interchange, such as `NM1[2]-3` or
//! `CLM05-01`.

use std::fmt;
use std::str::FromStr;

use crate::error::PathError;

/// The largest accepted occurrence, element, repetition or component index.
/// Bounds the padding an edit can create.
pub const MAX_INDEX: usize = 4096;

/// Address of a value in an interchange.
///
/// Two notations are accepted:
///
/// - The path notation shared with HL7 v2 and ASTM: `NM1-3`, `NM1[2]-3`
///   (second `NM1` segment), `CLM-5.1` (first component), `HI-1[2].2`
///   (second repetition, second component). The dotted form `NM1.3` is
///   accepted for the element. Element `0` is the segment identifier.
/// - X12 reference designators: `NM103`, `CLM05-01` (segment identifier,
///   two-digit element position, optional two-digit component position).
///
/// All indexes are 1-based. Without a repetition index, component access
/// refers to the first repetition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path {
    segment: String,
    occurrence: usize,
    element: usize,
    repetition: Option<usize>,
    component: Option<usize>,
}

impl Path {
    /// A path to element `element` of the first segment `segment`.
    pub fn new(segment: &str, element: usize) -> Result<Self, PathError> {
        if !is_segment_id(segment) {
            return Err(PathError::Invalid(segment.to_owned()));
        }
        Ok(Self {
            segment: segment.to_owned(),
            occurrence: 1,
            element: check(element, true).ok_or_else(|| PathError::Invalid(element.to_string()))?,
            repetition: None,
            component: None,
        })
    }

    /// Selects occurrence `n` of the segment.
    pub fn occurrence(mut self, n: usize) -> Result<Self, PathError> {
        self.occurrence = check(n, false).ok_or_else(|| PathError::Invalid(n.to_string()))?;
        Ok(self)
    }

    /// Selects repetition `n` of the element.
    pub fn repetition(mut self, n: usize) -> Result<Self, PathError> {
        self.repetition = Some(check(n, false).ok_or_else(|| PathError::Invalid(n.to_string()))?);
        Ok(self)
    }

    /// Selects component `n` of the element.
    pub fn component(mut self, n: usize) -> Result<Self, PathError> {
        self.component = Some(check(n, false).ok_or_else(|| PathError::Invalid(n.to_string()))?);
        Ok(self)
    }

    /// The segment identifier.
    pub fn segment_id(&self) -> &str {
        &self.segment
    }

    /// The 1-based occurrence of the segment.
    pub fn occurrence_index(&self) -> usize {
        self.occurrence
    }

    /// The element position; `0` is the segment identifier.
    pub fn element_index(&self) -> usize {
        self.element
    }

    /// The repetition index, if one was selected.
    pub fn repetition_index(&self) -> Option<usize> {
        self.repetition
    }

    /// The component index, if one was selected.
    pub fn component_index(&self) -> Option<usize> {
        self.component
    }
}

fn check(n: usize, zero_allowed: bool) -> Option<usize> {
    ((zero_allowed || n >= 1) && n <= MAX_INDEX).then_some(n)
}

/// Whether `id` can be a segment identifier: two or three uppercase letters
/// or digits, starting with a letter.
pub(crate) fn is_segment_id(id: &str) -> bool {
    (2..=3).contains(&id.len())
        && id.as_bytes()[0].is_ascii_uppercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

struct Cursor<'a> {
    text: &'a str,
    rest: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn invalid(&self) -> PathError {
        PathError::Invalid(self.text.to_owned())
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.rest.first() == Some(&b) {
            self.rest = &self.rest[1..];
            true
        } else {
            false
        }
    }

    fn number(&mut self, zero_allowed: bool) -> Result<usize, PathError> {
        let digits = self.rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 || digits > 4 {
            return Err(self.invalid());
        }
        let text = std::str::from_utf8(&self.rest[..digits]).map_err(|_| self.invalid())?;
        self.rest = &self.rest[digits..];
        let n = text.parse().map_err(|_| self.invalid())?;
        check(n, zero_allowed).ok_or_else(|| self.invalid())
    }

    fn bracket(&mut self) -> Result<Option<usize>, PathError> {
        if !self.eat(b'[') {
            return Ok(None);
        }
        let n = self.number(false)?;
        if !self.eat(b']') {
            return Err(self.invalid());
        }
        Ok(Some(n))
    }
}

impl FromStr for Path {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        let invalid = || PathError::Invalid(s.to_owned());
        let run = s
            .bytes()
            .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            .count();
        let (head, tail) = s.split_at(run);
        let mut cursor = Cursor {
            text: s,
            rest: tail.as_bytes(),
        };
        if is_segment_id(head) && matches!(tail.bytes().next(), Some(b'[' | b'-' | b'.')) {
            let occurrence = cursor.bracket()?.unwrap_or(1);
            if !(cursor.eat(b'-') || cursor.eat(b'.')) {
                return Err(invalid());
            }
            let element = cursor.number(true)?;
            let repetition = cursor.bracket()?;
            let component = if cursor.eat(b'.') {
                Some(cursor.number(false)?)
            } else {
                None
            };
            if !cursor.rest.is_empty()
                || (element == 0 && (repetition.is_some() || component.is_some()))
            {
                return Err(invalid());
            }
            return Ok(Self {
                segment: head.to_owned(),
                occurrence,
                element,
                repetition,
                component,
            });
        }
        // A reference designator: identifier plus a two-digit position.
        if (4..=5).contains(&run) {
            let (id, position) = head.split_at(run - 2);
            if is_segment_id(id) && position.bytes().all(|b| b.is_ascii_digit()) {
                let element = position.parse().map_err(|_| invalid())?;
                let component = if cursor.eat(b'-') {
                    Some(cursor.number(false)?)
                } else {
                    None
                };
                if !cursor.rest.is_empty() || element == 0 {
                    return Err(invalid());
                }
                return Ok(Self {
                    segment: id.to_owned(),
                    occurrence: 1,
                    element,
                    repetition: None,
                    component,
                });
            }
        }
        Err(invalid())
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.segment)?;
        if self.occurrence != 1 {
            write!(f, "[{}]", self.occurrence)?;
        }
        write!(f, "-{}", self.element)?;
        if let Some(repetition) = self.repetition {
            write!(f, "[{repetition}]")?;
        }
        if let Some(component) = self.component {
            write!(f, ".{component}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Path {
        s.parse().unwrap()
    }

    #[test]
    fn parses_path_notation() {
        let p = parse("NM1[2]-3");
        assert_eq!(
            (p.segment_id(), p.occurrence_index(), p.element_index()),
            ("NM1", 2, 3)
        );
        let p = parse("HI-1[2].2");
        assert_eq!(
            (p.repetition_index(), p.component_index()),
            (Some(2), Some(2))
        );
        assert_eq!(parse("CLM.5"), parse("CLM-5"));
        assert_eq!(parse("ST-0").element_index(), 0);
        assert_eq!(parse("NM1-03").element_index(), 3);
    }

    #[test]
    fn parses_reference_designators() {
        let p = parse("NM103");
        assert_eq!((p.segment_id(), p.element_index()), ("NM1", 3));
        let p = parse("CLM05-01");
        assert_eq!(
            (p.segment_id(), p.element_index(), p.component_index()),
            ("CLM", 5, Some(1))
        );
        assert_eq!(parse("N301").segment_id(), "N3");
        assert_eq!(parse("SV101-02").to_string(), "SV1-1.2");
    }

    #[test]
    fn rejects_invalid_paths() {
        for bad in [
            "",
            "NM1",
            "nm1-3",
            "NM1-",
            "NM1[0]-3",
            "NM1-3.0",
            "NM1-3x",
            "1NM-3",
            "ABCD-3",
            "NM100",
            "NM1-0.1",
            "NM1-99999",
            "NM1[2",
        ] {
            assert!(bad.parse::<Path>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn displays_canonically() {
        assert_eq!(parse("NM1[2]-3[2].1").to_string(), "NM1[2]-3[2].1");
        assert_eq!(parse("NM103").to_string(), "NM1-3");
    }
}
