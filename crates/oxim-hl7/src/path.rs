//! Addresses of values inside a message, such as `PID-5.1` or `OBX[2]-5`.

use std::fmt;
use std::str::FromStr;

use crate::error::PathError;

/// The largest accepted segment occurrence, field, repetition, component or
/// subcomponent index. Bounds the padding an edit can create.
pub const MAX_INDEX: usize = 4096;

/// Address of a value inside a segment: `5`, `5.1`, `5[2].1.3`.
///
/// All indexes are 1-based, following HL7 notation. Without a repetition
/// index, component access refers to the first repetition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldPath {
    field: usize,
    repetition: Option<usize>,
    component: Option<usize>,
    subcomponent: Option<usize>,
}

impl FieldPath {
    /// Creates a path to a whole field.
    pub fn new(field: usize) -> Result<Self, PathError> {
        Ok(Self {
            field: check(field)?,
            repetition: None,
            component: None,
            subcomponent: None,
        })
    }

    /// Selects one repetition of the field.
    pub fn repetition(mut self, repetition: usize) -> Result<Self, PathError> {
        self.repetition = Some(check(repetition)?);
        Ok(self)
    }

    /// Selects one component.
    pub fn component(mut self, component: usize) -> Result<Self, PathError> {
        self.component = Some(check(component)?);
        Ok(self)
    }

    /// Selects one subcomponent. Requires a component; component 1 is used
    /// when none was selected.
    pub fn subcomponent(mut self, subcomponent: usize) -> Result<Self, PathError> {
        self.component.get_or_insert(1);
        self.subcomponent = Some(check(subcomponent)?);
        Ok(self)
    }

    /// The field number.
    pub fn field_number(&self) -> usize {
        self.field
    }

    /// The repetition index, if one was selected.
    pub fn repetition_index(&self) -> Option<usize> {
        self.repetition
    }

    /// The component index, if one was selected.
    pub fn component_index(&self) -> Option<usize> {
        self.component
    }

    /// The subcomponent index, if one was selected.
    pub fn subcomponent_index(&self) -> Option<usize> {
        self.subcomponent
    }

    fn parse_from(cursor: &mut Cursor<'_>) -> Result<Self, PathError> {
        let mut path = Self::new(cursor.index()?)?;
        if let Some(repetition) = cursor.bracket()? {
            path.repetition = Some(repetition);
        }
        if cursor.eat(b'.') {
            path.component = Some(cursor.index()?);
            if cursor.eat(b'.') {
                path.subcomponent = Some(cursor.index()?);
            }
        }
        Ok(path)
    }
}

impl FromStr for FieldPath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        let mut cursor = Cursor::new(s);
        let path = Self::parse_from(&mut cursor)?;
        cursor.finish()?;
        Ok(path)
    }
}

impl fmt::Display for FieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.field)?;
        if let Some(repetition) = self.repetition {
            write!(f, "[{repetition}]")?;
        }
        if let Some(component) = self.component {
            write!(f, ".{component}")?;
        }
        if let Some(subcomponent) = self.subcomponent {
            write!(f, ".{subcomponent}")?;
        }
        Ok(())
    }
}

/// Address of a value in a message: `PID-5.1`, `OBX[2]-5`, `PID-3[2].1`.
///
/// The segment occurrence in brackets is 1-based and defaults to the first
/// occurrence. The dotted form used by some engines (`PID.5.1`) is accepted
/// as well.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path {
    segment: [u8; 3],
    occurrence: usize,
    field: FieldPath,
}

impl Path {
    /// Creates a path from its parts.
    pub fn new(segment: &str, occurrence: usize, field: FieldPath) -> Result<Self, PathError> {
        Ok(Self {
            segment: segment_id(segment.as_bytes())
                .ok_or_else(|| PathError::InvalidSegmentId(segment.to_owned()))?,
            occurrence: check(occurrence)?,
            field,
        })
    }

    /// The segment identifier, for example `b"PID"`.
    pub fn segment_id(&self) -> &[u8; 3] {
        &self.segment
    }

    /// The 1-based occurrence of the segment.
    pub fn occurrence(&self) -> usize {
        self.occurrence
    }

    /// The position inside the segment.
    pub fn field_path(&self) -> &FieldPath {
        &self.field
    }
}

impl FromStr for Path {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        let bytes = s.as_bytes();
        let segment = bytes
            .get(..3)
            .and_then(segment_id)
            .ok_or_else(|| PathError::Syntax(s.to_owned()))?;
        let mut cursor = Cursor::new(s);
        cursor.pos = 3;
        let occurrence = cursor.bracket()?.unwrap_or(1);
        if !(cursor.eat(b'-') || cursor.eat(b'.')) {
            return Err(PathError::Syntax(s.to_owned()));
        }
        let field = FieldPath::parse_from(&mut cursor)?;
        cursor.finish()?;
        Ok(Self {
            segment,
            occurrence,
            field,
        })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.segment))?;
        if self.occurrence != 1 {
            write!(f, "[{}]", self.occurrence)?;
        }
        write!(f, "-{}", self.field)
    }
}

/// Validates a segment identifier: three ASCII letters or digits.
pub(crate) fn segment_id(bytes: &[u8]) -> Option<[u8; 3]> {
    let id: [u8; 3] = bytes.try_into().ok()?;
    id.iter().all(u8::is_ascii_alphanumeric).then_some(id)
}

fn check(index: usize) -> Result<usize, PathError> {
    if (1..=MAX_INDEX).contains(&index) {
        Ok(index)
    } else {
        Err(PathError::IndexOutOfRange { max: MAX_INDEX })
    }
}

struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn syntax(&self) -> PathError {
        PathError::Syntax(self.text.to_owned())
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        let matched = self.peek() == Some(byte);
        if matched {
            self.pos += 1;
        }
        matched
    }

    fn index(&mut self) -> Result<usize, PathError> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        let digits = &self.text[start..self.pos];
        if digits.is_empty() {
            return Err(self.syntax());
        }
        let value = digits
            .parse::<usize>()
            .map_err(|_| PathError::IndexOutOfRange { max: MAX_INDEX })?;
        check(value)
    }

    fn bracket(&mut self) -> Result<Option<usize>, PathError> {
        if !self.eat(b'[') {
            return Ok(None);
        }
        let value = self.index()?;
        if !self.eat(b']') {
            return Err(self.syntax());
        }
        Ok(Some(value))
    }

    fn finish(&self) -> Result<(), PathError> {
        if self.pos == self.text.len() {
            Ok(())
        } else {
            Err(self.syntax())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_message_paths() {
        let path: Path = "OBX[2]-5[3].1.2".parse().unwrap();
        assert_eq!(path.segment_id(), b"OBX");
        assert_eq!(path.occurrence(), 2);
        let field = path.field_path();
        assert_eq!(field.field_number(), 5);
        assert_eq!(field.repetition_index(), Some(3));
        assert_eq!(field.component_index(), Some(1));
        assert_eq!(field.subcomponent_index(), Some(2));
        assert_eq!(path.to_string(), "OBX[2]-5[3].1.2");
    }

    #[test]
    fn accepts_dotted_form() {
        let dotted: Path = "PID.5.1".parse().unwrap();
        let dashed: Path = "PID-5.1".parse().unwrap();
        assert_eq!(dotted, dashed);
        assert_eq!(dotted.to_string(), "PID-5.1");
    }

    #[test]
    fn rejects_invalid_paths() {
        for text in [
            "",
            "PID",
            "PID-",
            "PID-0",
            "PID-5.",
            "PID-5..1",
            "PID-5.1.2.3",
            "PID[0]-1",
            "PID[1-1",
            "PI-5",
            "P D-5",
            "PID-5[2",
            "PID-5x",
            "PID-99999999999999999999999",
        ] {
            assert!(text.parse::<Path>().is_err(), "{text:?} should be rejected");
        }
    }

    #[test]
    fn limits_indexes() {
        assert!(format!("PID-{MAX_INDEX}").parse::<Path>().is_ok());
        assert_eq!(
            format!("PID-{}", MAX_INDEX + 1).parse::<Path>(),
            Err(PathError::IndexOutOfRange { max: MAX_INDEX })
        );
    }

    #[test]
    fn builds_field_paths() {
        let path = FieldPath::new(3).unwrap().subcomponent(2).unwrap();
        assert_eq!(path.to_string(), "3.1.2");
        assert_eq!("3.1.2".parse::<FieldPath>().unwrap(), path);
    }
}
