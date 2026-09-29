use thiserror::Error;

/// Returned when bytes cannot be parsed as an HL7 v2 message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The input was empty.
    #[error("message is empty")]
    Empty,
    /// The input did not start with an `MSH` segment.
    #[error("message must start with an MSH segment")]
    MissingHeader,
    /// MSH-1 or MSH-2 declared an unusable delimiter set.
    #[error("invalid delimiters: {0}")]
    InvalidDelimiters(&'static str),
    /// MSH-18 declares a character set whose multi-byte sequences may contain
    /// delimiter bytes, so the message cannot be split safely at byte level.
    #[error("unsupported character set {0:?}")]
    UnsupportedCharset(String),
    /// The message exceeds a limit configured in `ParseOptions`.
    #[error("message exceeds the configured limit for {0}")]
    LimitExceeded(&'static str),
}

/// Returned when a value cannot be escaped for the message's delimiters.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EscapeError {
    /// The value contains delimiter or line-break bytes but MSH-2 declares no
    /// escape character.
    #[error("value contains delimiters but the message declares no escape character")]
    NoEscapeCharacter,
}

/// Returned when an HL7 character set name (MSH-18) cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CharsetError {
    /// The name is not an HL7 table 0211 value or a recognized encoding label.
    #[error("unknown character set {0:?}")]
    Unknown(String),
    /// The character set is known but its multi-byte sequences may contain
    /// delimiter bytes (for example Big5 or Shift_JIS), or it is not
    /// ASCII-compatible (UTF-16, UTF-32).
    #[error("unsupported character set {0:?}")]
    Unsupported(String),
}

/// Returned when a path is malformed or cannot be applied to a message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PathError {
    /// The path text is not valid path syntax.
    #[error("invalid path {0:?}")]
    Syntax(String),
    /// A path index is zero or larger than `MAX_INDEX`.
    #[error("path index must be between 1 and {max}")]
    IndexOutOfRange {
        /// The largest accepted index.
        max: usize,
    },
    /// The addressed segment occurrence does not exist.
    #[error("segment {0} not found")]
    SegmentNotFound(String),
    /// A segment identifier is not three ASCII letters or digits, or names a
    /// header segment that cannot be added or removed.
    #[error("invalid segment identifier {0:?}")]
    InvalidSegmentId(String),
    /// MSH-1 and MSH-2 define the delimiters and cannot be edited in place.
    #[error("MSH-1 and MSH-2 cannot be edited")]
    ReadOnly,
    /// The path addresses a level whose delimiter is not declared in MSH-2.
    #[error("the message declares no {0} delimiter")]
    MissingDelimiter(&'static str),
    /// The text contains characters that the message's character set cannot
    /// represent.
    #[error("text cannot be represented in the message character set {0}")]
    Unencodable(&'static str),
    /// The value could not be escaped.
    #[error(transparent)]
    Escape(#[from] EscapeError),
    /// The message declares a character set that cannot be used for encoding.
    #[error(transparent)]
    Charset(#[from] CharsetError),
}
