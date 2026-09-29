use thiserror::Error;

/// Errors of the DICOM helpers of this crate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum DicomError {
    /// The bytes are not a well-formed DICOM object or command.
    #[error("invalid DICOM data: {0}")]
    Invalid(String),
    /// The data is well formed but uses something this crate does not
    /// handle, such as a deflated transfer syntax.
    #[error("unsupported DICOM data: {0}")]
    Unsupported(String),
    /// An association could not be established or broke down.
    #[error("DICOM association: {0}")]
    Association(String),
    /// The peer answered with a failure status.
    #[error("DICOM status {status:#06x}: {message}")]
    Status {
        /// The DIMSE status code.
        status: u16,
        /// What the status means, with the peer's error comment if any.
        message: String,
    },
}

impl DicomError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}
