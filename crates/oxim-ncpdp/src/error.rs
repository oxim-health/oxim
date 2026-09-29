//! Error types.

use thiserror::Error;

/// Returned when bytes cannot be parsed as a transmission.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The input is empty.
    #[error("the input is empty")]
    Empty,
    /// The fixed header is neither a request (56 bytes) nor a response
    /// (31 bytes) header.
    #[error(
        "the transaction header is {0} bytes; a request header has 56 and a response header 31"
    )]
    HeaderLength(usize),
    /// The input exceeds a limit from [`ParseOptions`](crate::ParseOptions).
    #[error("the transmission exceeds the limit for {0}")]
    LimitExceeded(&'static str),
}

/// Returned when a path is invalid or cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PathError {
    /// The text is not a valid path.
    #[error("invalid NCPDP path {0:?}")]
    Invalid(String),
    /// The segment does not exist.
    #[error("segment {0} does not exist")]
    NoSegment(String),
    /// The header of this transmission has no such field.
    #[error("the header has no field {0}")]
    NoHeaderField(String),
    /// The value does not fit the fixed-width header field.
    #[error("the value is longer than the {0}-byte header field")]
    TooLong(usize),
    /// The text contains a separator character.
    #[error("the value contains a segment, group or field separator")]
    Separator,
    /// The value cannot be written, such as the segment identifier.
    #[error("the value is read-only")]
    ReadOnly,
    /// A repetition index skips over missing occurrences.
    #[error("field occurrence {0} does not exist and is not the next one")]
    Gap(usize),
}
