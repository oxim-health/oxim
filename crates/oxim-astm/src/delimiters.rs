use crate::error::ParseError;

/// The delimiter set of a message, declared at the start of the header
/// record: `H|\^&` declares the field (`|`), repeat (`\`), component (`^`)
/// and escape (`&`) delimiters.
///
/// ASTM E1394 has no subcomponent level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Delimiters {
    /// Field delimiter, usually `|`.
    pub field: u8,
    /// Repeat delimiter, usually `\`.
    pub repeat: u8,
    /// Component delimiter, usually `^`.
    pub component: u8,
    /// Escape delimiter, usually `&`.
    pub escape: u8,
}

impl Default for Delimiters {
    /// The conventional `|\^&` delimiter set.
    fn default() -> Self {
        Self {
            field: b'|',
            repeat: b'\\',
            component: b'^',
            escape: b'&',
        }
    }
}

impl Delimiters {
    /// Reads the delimiters from the start of a header record.
    pub(crate) fn from_header(line: &[u8]) -> Result<Self, ParseError> {
        let Some(&field) = line.get(1) else {
            return Err(ParseError::InvalidDelimiters("missing field delimiter"));
        };
        let rest = line.get(2..).unwrap_or_default();
        let len = rest
            .iter()
            .position(|&b| b == field || b == b'\r' || b == b'\n')
            .unwrap_or(rest.len());
        let &[repeat, component, escape] = &rest[..len] else {
            return Err(ParseError::InvalidDelimiters(
                "the delimiter definition must declare repeat, component and escape delimiters",
            ));
        };
        let delimiters = Self {
            field,
            repeat,
            component,
            escape,
        };
        delimiters.validate()?;
        Ok(delimiters)
    }

    /// Checks that every delimiter is a printable, non-alphanumeric ASCII
    /// character and that no two delimiters are equal.
    pub fn validate(&self) -> Result<(), ParseError> {
        let all = self.all();
        for (i, &b) in all.iter().enumerate() {
            if !b.is_ascii_graphic() || b.is_ascii_alphanumeric() {
                return Err(ParseError::InvalidDelimiters(
                    "delimiters must be printable, non-alphanumeric ASCII characters",
                ));
            }
            if all[..i].contains(&b) {
                return Err(ParseError::InvalidDelimiters("delimiters must be distinct"));
            }
        }
        Ok(())
    }

    /// The header's field 2 content after the field delimiter: repeat,
    /// component and escape delimiters.
    pub fn definition(&self) -> [u8; 3] {
        [self.repeat, self.component, self.escape]
    }

    /// Whether `byte` must be escaped when it appears inside a value.
    pub(crate) fn is_special(&self, byte: u8) -> bool {
        byte == b'\r' || byte == b'\n' || self.all().contains(&byte)
    }

    fn all(&self) -> [u8; 4] {
        [self.field, self.repeat, self.component, self.escape]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_standard_delimiters() {
        assert_eq!(
            Delimiters::from_header(b"H|\\^&|||LIS").unwrap(),
            Delimiters::default()
        );
        assert_eq!(
            Delimiters::from_header(b"H|\\^&\r").unwrap(),
            Delimiters::default()
        );
        assert_eq!(
            Delimiters::from_header(b"H|\\^&").unwrap(),
            Delimiters::default()
        );
    }

    #[test]
    fn reads_custom_delimiters() {
        let d = Delimiters::from_header(b"H!~*%!!LIS").unwrap();
        assert_eq!(
            (d.field, d.repeat, d.component, d.escape),
            (b'!', b'~', b'*', b'%')
        );
        assert_eq!(d.definition(), *b"~*%");
    }

    #[test]
    fn rejects_invalid_delimiters() {
        for header in [
            &b"H"[..],
            b"H|",
            b"H|\\^",
            b"H|\\^&#|",
            b"H|\\\\&|",
            b"H|A^&|",
            b"H| ^&|",
        ] {
            assert!(
                Delimiters::from_header(header).is_err(),
                "{:?}",
                String::from_utf8_lossy(header)
            );
        }
    }
}
