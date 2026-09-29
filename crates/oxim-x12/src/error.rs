//! Error types.

use thiserror::Error;

use crate::envelope::Issue;

/// Returned when bytes cannot be parsed as an X12 interchange.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The input is empty or contains only whitespace.
    #[error("the input is empty")]
    Empty,
    /// The input does not start with an `ISA` segment.
    #[error("an X12 interchange must start with an ISA segment")]
    MissingIsa,
    /// The `ISA` segment is too short to declare the delimiters.
    #[error("the ISA segment is incomplete: {0}")]
    IncompleteIsa(&'static str),
    /// The delimiters declared by `ISA` are unusable.
    #[error("invalid delimiters: {0}")]
    InvalidDelimiters(&'static str),
    /// The input exceeds a limit from [`ParseOptions`](crate::ParseOptions).
    #[error("the interchange exceeds the limit for {0}")]
    LimitExceeded(&'static str),
    /// Strict parsing found envelope errors.
    #[error("the interchange envelope is invalid: {}", describe(.0))]
    Envelope(Vec<Issue>),
}

fn describe(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Returned when a path is invalid or cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PathError {
    /// The text is not a valid path.
    #[error("invalid X12 path {0:?}")]
    Invalid(String),
    /// The segment does not exist.
    #[error("segment {0} does not exist")]
    NoSegment(String),
    /// The value cannot be written, such as a segment identifier or a
    /// delimiter declaration in `ISA`.
    #[error("the value is read-only")]
    ReadOnly,
    /// The text contains a delimiter; X12 has no escape mechanism.
    #[error("the value contains the delimiter {0:?}, which X12 cannot escape")]
    Delimiter(char),
    /// The data type cannot hold the value, for example a repetition in an
    /// interchange that declares no repetition separator.
    #[error("{0}")]
    Unsupported(&'static str),
}
