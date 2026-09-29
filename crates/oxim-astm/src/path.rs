//! Addresses of values inside a message, such as `R-4`, `R[2]-3.4` or
//! `P-6[2].1`.

use std::fmt;
use std::str::FromStr;

use crate::error::PathError;

/// The largest accepted record occurrence, field, repetition or component
/// index. Bounds the padding an edit can create.
pub const MAX_INDEX: usize = 4096;

/// Address of a value inside a record: `4`, `3.4`, `6[2].1`.
///
/// Indexes are 1-based and follow ASTM E1394 numbering, in which the record
/// type is field 1 (see [`Message`](crate::Message)). Without a repetition
/// index, component access refers to the first repetition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldPath {
    field: usize,
    repetition: Option<usize>,
    component: Option<usize>,
}

impl FieldPath {
    /// Creates a path to a whole field.
    pub fn new(field: usize) -> Result<Self, PathError> {
        Ok(Self {
            field: check(field)?,
            repetition: None,
            component: None,
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

    fn parse_from(cursor: &mut Cursor<'_>) -> Result<Self, PathError> {
        let mut path = Self::new(cursor.index()?)?;
        path.repetition = cursor.bracket()?;
        if cursor.eat(b'.') {
            path.component = Some(cursor.index()?);
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
        Ok(())
    }
}

/// Address of a value in a message: `R-4`, `R[2]-3.4`, `P-6[2].1`.
///
/// The record occurrence in brackets is 1-based and defaults to the first
/// occurrence. The dotted form common in ASTM documentation (`R.4`, `O.5.4`)
/// is accepted as well.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path {
    record_type: String,
    occurrence: usize,
    field: FieldPath,
}

impl Path {
    /// Creates a path from its parts.
    pub fn new(record_type: &str, occurrence: usize, field: FieldPath) -> Result<Self, PathError> {
        if !is_record_type(record_type.as_bytes()) {
            return Err(PathError::InvalidRecordType(record_type.to_owned()));
        }
        Ok(Self {
            record_type: record_type.to_owned(),
            occurrence: check(occurrence)?,
            field,
        })
    }

    /// The record type, for example `R`.
    pub fn record_type(&self) -> &str {
        &self.record_type
    }

    /// The 1-based occurrence of the record.
    pub fn occurrence(&self) -> usize {
        self.occurrence
    }

    /// The position inside the record.
    pub fn field_path(&self) -> &FieldPath {
        &self.field
    }
}

impl FromStr for Path {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        let len = s
            .bytes()
            .position(|b| !b.is_ascii_alphanumeric())
            .ok_or_else(|| PathError::Syntax(s.to_owned()))?;
        let record_type = &s[..len];
        if !is_record_type(record_type.as_bytes()) {
            return Err(PathError::Syntax(s.to_owned()));
        }
        let mut cursor = Cursor::new(s);
        cursor.pos = len;
        let occurrence = cursor.bracket()?.unwrap_or(1);
        if !(cursor.eat(b'-') || cursor.eat(b'.')) {
            return Err(PathError::Syntax(s.to_owned()));
        }
        let field = FieldPath::parse_from(&mut cursor)?;
        cursor.finish()?;
        Ok(Self {
            record_type: record_type.to_owned(),
            occurrence,
            field,
        })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.record_type)?;
        if self.occurrence != 1 {
            write!(f, "[{}]", self.occurrence)?;
        }
        write!(f, "-{}", self.field)
    }
}

/// Record types are one to three ASCII letters or digits.
pub(crate) fn is_record_type(bytes: &[u8]) -> bool {
    (1..=3).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_alphanumeric)
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
    fn parses_paths() {
        let path: Path = "R[2]-3[2].4".parse().unwrap();
        assert_eq!(path.record_type(), "R");
        assert_eq!(path.occurrence(), 2);
        assert_eq!(path.field_path().field_number(), 3);
        assert_eq!(path.field_path().repetition_index(), Some(2));
        assert_eq!(path.field_path().component_index(), Some(4));
        assert_eq!(path.to_string(), "R[2]-3[2].4");
        assert_eq!(
            "R.4".parse::<Path>().unwrap(),
            "R-4".parse::<Path>().unwrap()
        );
        assert_eq!("O.5.4".parse::<Path>().unwrap().to_string(), "O-5.4");
    }

    #[test]
    fn rejects_invalid_paths() {
        for text in [
            "", "R", "R-", "R-0", "R-1.", "R-1.2.3", "R[0]-1", "RRRR-1", "R-1[2", "R-1x", "-1",
            "R[1-1",
        ] {
            assert!(text.parse::<Path>().is_err(), "{text:?}");
        }
    }

    #[test]
    fn limits_indexes() {
        assert!(format!("R-{MAX_INDEX}").parse::<Path>().is_ok());
        assert_eq!(
            format!("R-{}", MAX_INDEX + 1).parse::<Path>(),
            Err(PathError::IndexOutOfRange { max: MAX_INDEX })
        );
    }
}
