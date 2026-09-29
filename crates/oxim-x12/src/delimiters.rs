//! The delimiters an interchange declares in its `ISA` segment.

use crate::error::{ParseError, PathError};

/// The four X12 delimiters.
///
/// They are declared positionally by the `ISA` segment: the element
/// separator is the byte after `ISA`, the component separator is `ISA-16`,
/// the segment terminator is the byte after `ISA-16`, and the repetition
/// separator is `ISA-11` when that element holds a delimiter (version 00402
/// and later) rather than the standards identifier `U` of older versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Delimiters {
    /// Separates elements (commonly `*`).
    pub element: u8,
    /// Separates components of a composite element (commonly `:`).
    pub component: u8,
    /// Separates repetitions (commonly `^`); `None` before version 00402.
    pub repetition: Option<u8>,
    /// Terminates segments (commonly `~`).
    pub segment: u8,
}

impl Default for Delimiters {
    fn default() -> Self {
        Self {
            element: b'*',
            component: b':',
            repetition: Some(b'^'),
            segment: b'~',
        }
    }
}

impl Delimiters {
    /// Checks that the delimiters are distinct and cannot be confused with
    /// data: no letters, digits or spaces, and only the segment terminator
    /// may be a line break.
    pub fn validate(&self) -> Result<(), ParseError> {
        let mut all = vec![self.element, self.component, self.segment];
        all.extend(self.repetition);
        for (i, &b) in all.iter().enumerate() {
            if b.is_ascii_alphanumeric() || b == b' ' {
                return Err(ParseError::InvalidDelimiters(
                    "delimiters must not be letters, digits or spaces",
                ));
            }
            if all[..i].contains(&b) {
                return Err(ParseError::InvalidDelimiters("delimiters must be distinct"));
            }
        }
        let data_delimiters = [Some(self.element), Some(self.component), self.repetition];
        if data_delimiters
            .into_iter()
            .flatten()
            .any(|b| b == b'\r' || b == b'\n')
        {
            return Err(ParseError::InvalidDelimiters(
                "only the segment terminator may be a line break",
            ));
        }
        Ok(())
    }

    /// Reads the delimiters from the `ISA` segment at the start of `input`.
    pub(crate) fn from_isa(input: &[u8]) -> Result<Self, ParseError> {
        if !input.starts_with(b"ISA") {
            return Err(ParseError::MissingIsa);
        }
        let element = *input
            .get(3)
            .ok_or(ParseError::IncompleteIsa("no element separator"))?;
        let mut separators = 0;
        let mut elements_start = [0usize; 17];
        let mut position = 3;
        while separators < 16 {
            let at = memchr::memchr(element, &input[position..])
                .ok_or(ParseError::IncompleteIsa("fewer than 16 elements"))?;
            position += at;
            separators += 1;
            elements_start[separators] = position + 1;
            position += 1;
        }
        let component = *input
            .get(position)
            .ok_or(ParseError::IncompleteIsa("no component separator (ISA-16)"))?;
        let segment = *input
            .get(position + 1)
            .ok_or(ParseError::IncompleteIsa("no segment terminator"))?;
        let isa11 = &input[elements_start[11]..elements_start[12] - 1];
        let repetition = match isa11 {
            [b] if !b.is_ascii_alphanumeric() && *b != b' ' => Some(*b),
            _ => None,
        };
        let delimiters = Self {
            element,
            component,
            repetition,
            segment,
        };
        delimiters.validate()?;
        Ok(delimiters)
    }

    /// Fails when `text` contains one of the delimiters.
    pub(crate) fn check_text(&self, text: &[u8]) -> Result<(), PathError> {
        let mut all = vec![self.element, self.component, self.segment];
        all.extend(self.repetition);
        match text.iter().find(|b| all.contains(b)) {
            Some(&b) => Err(PathError::Delimiter(char::from(b))),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISA: &[u8] = b"ISA*00*          *00*          *ZZ*SENDER         *ZZ*RECEIVER       *260929*1200*^*00501*000000001*0*T*:~";

    #[test]
    fn reads_the_isa_delimiters() {
        assert_eq!(Delimiters::from_isa(ISA).unwrap(), Delimiters::default());
        let old = b"ISA|00|          |00|          |ZZ|SENDER         |ZZ|RECEIVER       |260929|1200|U|00401|000000001|0|T|>\n";
        let d = Delimiters::from_isa(old).unwrap();
        assert_eq!(
            (d.element, d.component, d.repetition, d.segment),
            (b'|', b'>', None, b'\n')
        );
    }

    #[test]
    fn rejects_bad_isa_segments() {
        assert_eq!(Delimiters::from_isa(b"GS*"), Err(ParseError::MissingIsa));
        assert!(matches!(
            Delimiters::from_isa(b"ISA*00*"),
            Err(ParseError::IncompleteIsa(_))
        ));
        let clash = b"ISA*00*          *00*          *ZZ*SENDER         *ZZ*RECEIVER       *260929*1200*^*00501*000000001*0*T**~";
        assert!(matches!(
            Delimiters::from_isa(clash),
            Err(ParseError::InvalidDelimiters(_))
        ));
    }

    #[test]
    fn detects_delimiters_in_text() {
        let d = Delimiters::default();
        assert!(d.check_text(b"DOE JOHN").is_ok());
        assert_eq!(d.check_text(b"A*B"), Err(PathError::Delimiter('*')));
        assert_eq!(d.check_text(b"A~"), Err(PathError::Delimiter('~')));
    }
}
