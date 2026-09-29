//! Date and time conversion between HL7, ISO 8601 and simple patterns.

use oxim_core::EngineError;
use oxim_model::ClinicalDateTime;

use crate::settings::config_error;

/// A date/time representation.
///
/// Patterns understand `%Y` (4-digit year), `%m` (month), `%d` (day),
/// `%H` (hour, 24 h), `%M` (minute), `%S` (second), each with exactly two
/// digits except the year, and `%%` for a literal percent sign; every other
/// character must appear literally. A pattern must describe a date from the
/// year down without gaps (`%d.%m.%Y %H:%M` is fine, `%Y %d` is not).
/// Patterns carry no UTC offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateFormat {
    /// HL7 v2 `DTM` (`20260929143005+0300`), also used by ASTM.
    Hl7,
    /// ISO 8601 / FHIR (`2026-09-29T14:30:05+03:00`).
    Iso,
    /// A pattern such as `%d.%m.%Y %H:%M`.
    Pattern(Vec<Token>),
}

/// One element of a date pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// A character that must appear as is.
    Literal(char),
    /// A numeric component.
    Field(Field),
}

/// A numeric date/time component, in order of precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    /// `%Y`
    Year,
    /// `%m`
    Month,
    /// `%d`
    Day,
    /// `%H`
    Hour,
    /// `%M`
    Minute,
    /// `%S`
    Second,
}

impl Field {
    fn width(self) -> usize {
        if self == Self::Year { 4 } else { 2 }
    }
}

const ORDER: [Field; 6] = [
    Field::Year,
    Field::Month,
    Field::Day,
    Field::Hour,
    Field::Minute,
    Field::Second,
];

impl DateFormat {
    /// Parses `hl7`, `iso` or a pattern containing `%`.
    pub fn parse(at: &str, text: &str) -> Result<Self, EngineError> {
        match text {
            "hl7" | "astm" => return Ok(Self::Hl7),
            "iso" | "fhir" => return Ok(Self::Iso),
            _ => {}
        }
        if !text.contains('%') {
            return Err(config_error(
                at,
                format!("unknown date format {text:?}; use hl7, iso or a pattern such as %d.%m.%Y"),
            ));
        }
        let mut tokens = Vec::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                tokens.push(Token::Literal(c));
                continue;
            }
            let field = match chars.next() {
                Some('Y') => Field::Year,
                Some('m') => Field::Month,
                Some('d') => Field::Day,
                Some('H') => Field::Hour,
                Some('M') => Field::Minute,
                Some('S') => Field::Second,
                Some('%') => {
                    tokens.push(Token::Literal('%'));
                    continue;
                }
                other => {
                    return Err(config_error(
                        at,
                        format!(
                            "unsupported pattern element %{} in {text:?}",
                            other.map(String::from).unwrap_or_default()
                        ),
                    ));
                }
            };
            tokens.push(Token::Field(field));
        }
        let mut fields: Vec<Field> = tokens
            .iter()
            .filter_map(|token| match token {
                Token::Field(field) => Some(*field),
                Token::Literal(_) => None,
            })
            .collect();
        fields.sort();
        let expected = &ORDER[..fields.len().min(ORDER.len())];
        if fields.is_empty() || fields != expected {
            return Err(config_error(
                at,
                format!(
                    "pattern {text:?} must contain each of %Y %m %d %H %M %S at most once, from the year down without gaps"
                ),
            ));
        }
        Ok(Self::Pattern(tokens))
    }

    /// Reads a value in this format.
    pub fn read(&self, text: &str) -> Result<ClinicalDateTime, String> {
        match self {
            Self::Hl7 => ClinicalDateTime::parse_hl7(text.trim()).map_err(|e| e.to_string()),
            Self::Iso => ClinicalDateTime::parse_iso(text.trim()).map_err(|e| e.to_string()),
            Self::Pattern(tokens) => read_pattern(tokens, text),
        }
    }

    /// Writes a value in this format. Patterns need every component they
    /// name.
    pub fn write(&self, value: &ClinicalDateTime) -> Result<String, String> {
        match self {
            Self::Hl7 => Ok(value.to_hl7()),
            Self::Iso => Ok(value.to_iso()),
            Self::Pattern(tokens) => {
                let mut out = String::new();
                for token in tokens {
                    match token {
                        Token::Literal(c) => out.push(*c),
                        Token::Field(field) => {
                            let number = match field {
                                Field::Year => Some(value.year()),
                                Field::Month => value.month().map(u16::from),
                                Field::Day => value.day().map(u16::from),
                                Field::Hour => value.hour().map(u16::from),
                                Field::Minute => value.minute().map(u16::from),
                                Field::Second => value.second().map(u16::from),
                            }
                            .ok_or_else(|| {
                                format!(
                                    "{} has no {field:?} for the output pattern",
                                    value.to_iso()
                                )
                            })?;
                            out.push_str(&format!("{number:0width$}", width = field.width()));
                        }
                    }
                }
                Ok(out)
            }
        }
    }
}

fn read_pattern(tokens: &[Token], text: &str) -> Result<ClinicalDateTime, String> {
    let invalid = || format!("{text:?} does not match the date pattern");
    let mut parts: [Option<&str>; 6] = [None; 6];
    let mut rest = text;
    for token in tokens {
        match token {
            Token::Literal(c) => {
                rest = rest.strip_prefix(*c).ok_or_else(invalid)?;
            }
            Token::Field(field) => {
                let width = field.width();
                let digits = rest.get(..width).ok_or_else(invalid)?;
                if !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(invalid());
                }
                parts[ORDER.iter().position(|f| f == field).unwrap_or_default()] = Some(digits);
                rest = &rest[width..];
            }
        }
    }
    if !rest.is_empty() {
        return Err(invalid());
    }
    let hl7: String = parts.iter().map_while(|part| *part).collect();
    ClinicalDateTime::parse_hl7(&hl7).map_err(|_| format!("{text:?} is not a valid date"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(text: &str) -> DateFormat {
        DateFormat::parse("t", text).unwrap()
    }

    #[test]
    fn converts_between_formats_keeping_precision() {
        let hl7 = format("hl7");
        let iso = format("iso");
        let value = hl7.read("20260929143005+0300").unwrap();
        assert_eq!(iso.write(&value).unwrap(), "2026-09-29T14:30:05+03:00");
        assert_eq!(iso.write(&hl7.read("202609").unwrap()).unwrap(), "2026-09");
        let turkish = format("%d.%m.%Y %H:%M");
        let local = turkish.read("29.09.2026 14:30").unwrap();
        assert_eq!(hl7.write(&local).unwrap(), "202609291430");
        assert_eq!(
            turkish.write(&hl7.read("202609291430").unwrap()).unwrap(),
            "29.09.2026 14:30"
        );
        assert!(turkish.write(&hl7.read("20260929").unwrap()).is_err());
        let percent = format("%Y%%%m");
        assert_eq!(
            percent.write(&hl7.read("202609").unwrap()).unwrap(),
            "2026%09"
        );
    }

    #[test]
    fn rejects_bad_patterns_and_values() {
        for bad in [
            "dd.mm.yyyy",
            "%Y %d",
            "%m.%d",
            "%Y %Y",
            "%Q",
            "%",
            "%Y %m %d %S",
        ] {
            assert!(DateFormat::parse("t", bad).is_err(), "{bad:?}");
        }
        let pattern = format("%d.%m.%Y");
        for bad in [
            "29.9.2026",
            "29-09-2026",
            "31.02.2026",
            "29.09.2026 ",
            "aa.bb.cccc",
        ] {
            assert!(pattern.read(bad).is_err(), "{bad:?}");
        }
        assert!(format("hl7").read("2026-09-29").is_err());
    }
}
