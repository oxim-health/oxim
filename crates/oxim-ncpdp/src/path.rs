//! Addresses of values: `HDR.A1`, `AM07.D2`, `AM07[2].D2`, `AM08.E4[2]`.

use std::fmt;
use std::str::FromStr;

use crate::error::PathError;

/// The largest accepted occurrence index.
pub const MAX_INDEX: usize = 4096;

/// Address of a value in a transmission.
///
/// - `HDR.A1` — a field of the fixed-width transaction header, named by its
///   data dictionary identifier (see [`REQUEST_HEADER`](crate::REQUEST_HEADER)
///   and [`RESPONSE_HEADER`](crate::RESPONSE_HEADER)).
/// - `AM07.D2` — field `D2` (Prescription/Service Reference Number) of the
///   first Claim segment (`AM07`).
/// - `AM07[2].D2` — the same field in the second Claim segment, for example
///   of the second transaction.
/// - `AM08.E4[2]` — the second occurrence of a repeating field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Path {
    /// A header field.
    Header {
        /// The field identifier, for example `A1`.
        field: String,
    },
    /// A segment field.
    Field {
        /// The segment identifier, for example `AM07`.
        segment: String,
        /// The 1-based occurrence of the segment.
        occurrence: usize,
        /// The field identifier, for example `D2`.
        field: String,
        /// The 1-based occurrence of the field within the segment.
        repetition: usize,
    },
}

/// Whether `id` is a two-character field identifier.
pub(crate) fn is_field_id(id: &str) -> bool {
    id.len() == 2
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Whether `id` is a segment identifier: `AM` and two digits.
pub(crate) fn is_segment_id(id: &str) -> bool {
    id.len() == 4 && id.starts_with("AM") && id[2..].bytes().all(|b| b.is_ascii_digit())
}

fn split_index(text: &str) -> Option<(&str, Option<usize>)> {
    match text.split_once('[') {
        None => Some((text, None)),
        Some((name, rest)) => {
            let digits = rest.strip_suffix(']')?;
            if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let n: usize = digits.parse().ok()?;
            (1..=MAX_INDEX).contains(&n).then_some((name, Some(n)))
        }
    }
}

impl FromStr for Path {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, PathError> {
        let invalid = || PathError::Invalid(s.to_owned());
        let (segment, field) = s.split_once('.').ok_or_else(invalid)?;
        let (segment, occurrence) = split_index(segment).ok_or_else(invalid)?;
        let (field, repetition) = split_index(field).ok_or_else(invalid)?;
        if !is_field_id(field) {
            return Err(invalid());
        }
        if segment == "HDR" {
            if occurrence.is_some() || repetition.is_some() {
                return Err(invalid());
            }
            return Ok(Self::Header {
                field: field.to_owned(),
            });
        }
        if !is_segment_id(segment) {
            return Err(invalid());
        }
        Ok(Self::Field {
            segment: segment.to_owned(),
            occurrence: occurrence.unwrap_or(1),
            field: field.to_owned(),
            repetition: repetition.unwrap_or(1),
        })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header { field } => write!(f, "HDR.{field}"),
            Self::Field {
                segment,
                occurrence,
                field,
                repetition,
            } => {
                f.write_str(segment)?;
                if *occurrence != 1 {
                    write!(f, "[{occurrence}]")?;
                }
                write!(f, ".{field}")?;
                if *repetition != 1 {
                    write!(f, "[{repetition}]")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_paths() {
        assert_eq!(
            "HDR.A1".parse::<Path>().unwrap(),
            Path::Header { field: "A1".into() }
        );
        assert_eq!(
            "AM07[2].D2".parse::<Path>().unwrap(),
            Path::Field {
                segment: "AM07".into(),
                occurrence: 2,
                field: "D2".into(),
                repetition: 1
            }
        );
        assert_eq!(
            "AM08.E4[2]".parse::<Path>().unwrap().to_string(),
            "AM08.E4[2]"
        );
        for bad in [
            "",
            "AM07",
            "AM07.D",
            "AM7.D2",
            "XX07.D2",
            "HDR[2].A1",
            "AM07[0].D2",
            "AM07.D2[x]",
            "am07.D2",
            "AM07.d2",
            "AM07.D2.E1",
        ] {
            assert!(bad.parse::<Path>().is_err(), "{bad:?}");
        }
    }
}
