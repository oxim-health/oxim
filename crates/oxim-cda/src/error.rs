//! Error types.

use oxim_formats::XmlError;
use thiserror::Error;

/// Returned when a document cannot be read or written.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CdaError {
    /// The bytes are not well-formed XML.
    #[error(transparent)]
    Xml(#[from] XmlError),
    /// The root element is not `ClinicalDocument`.
    #[error("the root element is {0:?}, not ClinicalDocument")]
    NotCda(String),
    /// The document has no laboratory results to map.
    #[error("the document has no laboratory results")]
    NoResults,
    /// The content cannot be written as a laboratory report.
    #[error("{0}")]
    Unsupported(String),
}

/// Result alias for this crate.
pub type CdaResult<T> = Result<T, CdaError>;
