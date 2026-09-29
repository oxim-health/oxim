use thiserror::Error;

/// Errors of FHIR parsing, mapping and encoding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum FhirError {
    /// The input is not valid FHIR JSON for the expected structure.
    #[error("invalid FHIR JSON: {0}")]
    Json(String),
    /// The content is valid but cannot be represented in the target form.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A required element is missing or a value is out of range.
    #[error("invalid content: {0}")]
    Invalid(String),
}

impl From<serde_json::Error> for FhirError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error.to_string())
    }
}

/// Result alias for this crate.
pub type FhirResult<T> = Result<T, FhirError>;
