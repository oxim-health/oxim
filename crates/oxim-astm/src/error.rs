use thiserror::Error;

/// Returned when bytes cannot be parsed as an ASTM E1394 (LIS02) message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The input was empty.
    #[error("message is empty")]
    Empty,
    /// The input did not start with a header (`H`) record.
    #[error("message must start with a header (H) record")]
    MissingHeader,
    /// The header declared an unusable delimiter set.
    #[error("invalid delimiters: {0}")]
    InvalidDelimiters(&'static str),
    /// The message exceeds a limit configured in `ParseOptions`.
    #[error("message exceeds the configured limit for {0}")]
    LimitExceeded(&'static str),
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
    /// The addressed record occurrence does not exist.
    #[error("record {0} not found")]
    RecordNotFound(String),
    /// A record type is not one to three ASCII letters or digits, or names
    /// the header record, which cannot be added or removed.
    #[error("invalid record type {0:?}")]
    InvalidRecordType(String),
    /// Field 1 (record type) of every record and field 2 (delimiter
    /// definition) of the header cannot be edited in place.
    #[error("the record type and the delimiter definition cannot be edited")]
    ReadOnly,
    /// The text contains characters the chosen encoding cannot represent.
    #[error("text cannot be represented in the encoding {0}")]
    Unencodable(&'static str),
}

/// Returned when an LIS01 frame is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FrameError {
    /// The input does not start with STX.
    #[error("frame does not start with STX")]
    MissingStx,
    /// The frame number is not an ASCII digit from `0` to `7`.
    #[error("invalid frame number {0:#04x}")]
    InvalidFrameNumber(u8),
    /// The frame text contains a character LIS01 reserves for link control.
    #[error("restricted character {byte:#04x} at offset {position}")]
    RestrictedCharacter {
        /// The offending byte.
        byte: u8,
        /// Its offset in the frame or message text.
        position: usize,
    },
    /// The frame text is longer than the configured maximum.
    #[error("frame text exceeds the maximum length")]
    TooLong,
    /// The checksum characters are not hexadecimal digits.
    #[error("malformed checksum")]
    MalformedChecksum,
    /// The checksum does not match the frame contents.
    #[error("checksum mismatch: expected {expected:02X}, received {received:02X}")]
    BadChecksum {
        /// The checksum computed from the frame.
        expected: u8,
        /// The checksum sent with the frame.
        received: u8,
    },
    /// The checksum is not followed by CR LF.
    #[error("frame is not terminated by CR LF")]
    MissingTrailer,
    /// A link control character (STX, ENQ or EOT) arrived before the frame
    /// ended.
    #[error("frame interrupted by control character {0:#04x}")]
    Interrupted(u8),
}

/// Returned when a message cannot be queued for transmission.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SendError {
    /// The message is empty.
    #[error("message is empty")]
    Empty,
    /// The message is larger than `SessionConfig::max_message_len`.
    #[error("message exceeds the maximum length")]
    TooLarge,
    /// The message contains a character LIS01 reserves for link control.
    /// Records must be separated by CR only.
    #[error("restricted character {byte:#04x} at offset {position}")]
    RestrictedCharacter {
        /// The offending byte.
        byte: u8,
        /// Its offset in the message.
        position: usize,
    },
}
