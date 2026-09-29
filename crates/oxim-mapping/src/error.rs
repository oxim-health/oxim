use thiserror::Error;

/// Returned when a message cannot be mapped to or from the normalized model.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum MappingError {
    /// The message type carries no content this crate maps, for example an
    /// HL7 `ACK` or a POCT1-A `HEL`.
    #[error("unsupported message: {0}")]
    Unsupported(String),
    /// The normalized content has the wrong kind for the encoder, for
    /// example orders given to a result encoder.
    #[error("cannot encode {found} content as {encoder}")]
    WrongContent {
        /// The encoder.
        encoder: &'static str,
        /// The kind of content that was given.
        found: &'static str,
    },
    /// The message has no normalized content; the channel source must set
    /// `normalize: true`.
    #[error("the message has no normalized content; set `normalize: true` on the channel source")]
    NotNormalized,
    /// A value could not be written, for example because the message
    /// character set cannot represent it.
    #[error("cannot write {path}: {detail}")]
    Write {
        /// Where the value was written.
        path: String,
        /// What went wrong.
        detail: String,
    },
    /// An encoder setting is invalid.
    #[error("invalid setting {name}: {detail}")]
    Setting {
        /// The setting.
        name: &'static str,
        /// What is wrong with it.
        detail: String,
    },
}

/// Result alias for mapping operations.
pub type MappingResult<T> = Result<T, MappingError>;
