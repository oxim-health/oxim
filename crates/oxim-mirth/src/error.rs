//! Errors of the importer.

use thiserror::Error;

/// Why an export could not be imported.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MirthError {
    /// The input is not well-formed XML, or uses XML features that are not
    /// accepted (document type declarations, custom entities).
    #[error("invalid XML: {0}")]
    Xml(String),
    /// The XML is not a Mirth Connect export this importer knows.
    #[error(
        "<{0}> is not a Mirth Connect export; expected <channel>, <channelGroup>, <serverConfiguration>, <codeTemplateLibrary> or <list>"
    )]
    NotAnExport(String),
    /// A channel was converted into YAML that OXIM rejects. This is a bug
    /// in the importer; the message names the channel.
    #[error("internal error: the channel {channel:?} was converted into invalid YAML: {message}")]
    Internal {
        /// The channel.
        channel: String,
        /// What OXIM reported.
        message: String,
    },
    /// The report could not be written as JSON.
    #[error("cannot write the report: {0}")]
    Report(String),
}
