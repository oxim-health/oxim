use crate::error::ParseError;

/// The delimiter set of a message, declared by MSH-1 (field separator) and
/// MSH-2 (encoding characters).
///
/// MSH-2 is positional: component, repetition, escape, subcomponent and
/// (HL7 v2.7+) truncation. Messages may declare fewer than five characters,
/// which is why the trailing delimiters are optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Delimiters {
    /// Field separator (MSH-1), usually `|`.
    pub field: u8,
    /// Component separator, usually `^`.
    pub component: u8,
    /// Repetition separator, usually `~`.
    pub repetition: u8,
    /// Escape character, usually `\`.
    pub escape: Option<u8>,
    /// Subcomponent separator, usually `&`.
    pub subcomponent: Option<u8>,
    /// Truncation character (HL7 v2.7+), usually `#`.
    pub truncation: Option<u8>,
}

impl Default for Delimiters {
    /// The conventional `|^~\&` delimiter set without a truncation character.
    fn default() -> Self {
        Self {
            field: b'|',
            component: b'^',
            repetition: b'~',
            escape: Some(b'\\'),
            subcomponent: Some(b'&'),
            truncation: None,
        }
    }
}

impl Delimiters {
    /// Reads MSH-1 and MSH-2 from the start of a header segment
    /// (`MSH`, `FHS` or `BHS`).
    pub(crate) fn from_header(line: &[u8]) -> Result<Self, ParseError> {
        let Some(&field) = line.get(3) else {
            return Err(ParseError::InvalidDelimiters("missing field separator"));
        };
        let rest = line.get(4..).unwrap_or_default();
        let len = rest
            .iter()
            .position(|&b| b == field || b == b'\r' || b == b'\n')
            .unwrap_or(rest.len());
        let encoding = &rest[..len];
        if encoding.len() < 2 {
            return Err(ParseError::InvalidDelimiters(
                "MSH-2 must declare at least component and repetition separators",
            ));
        }
        if encoding.len() > 5 {
            return Err(ParseError::InvalidDelimiters(
                "MSH-2 declares more than five encoding characters",
            ));
        }
        let delimiters = Self {
            field,
            component: encoding[0],
            repetition: encoding[1],
            escape: encoding.get(2).copied(),
            subcomponent: encoding.get(3).copied(),
            truncation: encoding.get(4).copied(),
        };
        delimiters.validate()?;
        Ok(delimiters)
    }

    /// Checks that every delimiter is a printable, non-alphanumeric ASCII
    /// character, that no two delimiters are equal, and that the optional
    /// delimiters can be written positionally in MSH-2.
    pub fn validate(&self) -> Result<(), ParseError> {
        if self.escape.is_none() && (self.subcomponent.is_some() || self.truncation.is_some()) {
            return Err(ParseError::InvalidDelimiters(
                "subcomponent and truncation characters require an escape character",
            ));
        }
        if self.subcomponent.is_none() && self.truncation.is_some() {
            return Err(ParseError::InvalidDelimiters(
                "a truncation character requires a subcomponent separator",
            ));
        }
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

    /// The MSH-2 encoding characters in positional order.
    pub fn encoding_characters(&self) -> Vec<u8> {
        let mut out = vec![self.component, self.repetition];
        out.extend(
            [self.escape, self.subcomponent, self.truncation]
                .into_iter()
                .map_while(|b| b),
        );
        out
    }

    /// Whether `byte` must be escaped when it appears inside a value.
    pub(crate) fn is_special(&self, byte: u8) -> bool {
        byte == b'\r' || byte == b'\n' || self.all().contains(&byte)
    }

    fn all(&self) -> Vec<u8> {
        let mut all = vec![self.field];
        all.extend(self.encoding_characters());
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_standard_delimiters() {
        let d = Delimiters::from_header(b"MSH|^~\\&|APP").unwrap();
        assert_eq!(d, Delimiters::default());
        assert_eq!(d.encoding_characters(), b"^~\\&");
    }

    #[test]
    fn reads_truncation_character() {
        let d = Delimiters::from_header(b"MSH|^~\\&#|APP").unwrap();
        assert_eq!(d.truncation, Some(b'#'));
        assert_eq!(d.encoding_characters(), b"^~\\&#");
    }

    #[test]
    fn accepts_short_encoding_characters() {
        let d = Delimiters::from_header(b"MSH|^~|APP").unwrap();
        assert_eq!(d.escape, None);
        assert_eq!(d.subcomponent, None);
    }

    #[test]
    fn accepts_non_standard_delimiters() {
        let d = Delimiters::from_header(b"MSH#*!$%#APP").unwrap();
        assert_eq!(d.field, b'#');
        assert_eq!(d.component, b'*');
        assert_eq!(d.escape, Some(b'$'));
    }

    #[test]
    fn rejects_invalid_delimiters() {
        for header in [
            &b"MSH"[..],
            b"MSH|",
            b"MSH||",
            b"MSH|^|",
            b"MSH|^^\\&|",
            b"MSH|^~\\&#@|",
            b"MSH|A~\\&|",
            b"MSH|^ \\&|",
            b"MSH|^~\\~|",
        ] {
            assert!(
                Delimiters::from_header(header).is_err(),
                "{:?} should be rejected",
                String::from_utf8_lossy(header)
            );
        }
    }

    #[test]
    fn stops_at_line_end() {
        let d = Delimiters::from_header(b"MSH|^~\r").unwrap();
        assert_eq!(d.encoding_characters(), b"^~");
    }
}
