use thiserror::Error;

/// Returned when bytes cannot be parsed as a POCT1-A message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ParseError {
    /// The document exceeds a limit configured in `ParseOptions`.
    #[error("document exceeds the configured limit for {0}")]
    LimitExceeded(&'static str),
    /// The document declares an encoding that is not ASCII-compatible or
    /// not known (UTF-16 and UTF-32 are not supported).
    #[error("unsupported encoding {0:?}")]
    UnsupportedEncoding(String),
    /// The bytes are not valid in the declared (or default UTF-8) encoding.
    #[error("the document is not valid {0}")]
    InvalidEncoding(&'static str),
    /// The document contains a construct that is rejected for security
    /// reasons, such as a document type declaration.
    #[error("forbidden construct: {0}")]
    Forbidden(&'static str),
    /// The document references an entity other than the five predefined
    /// entities or a character reference.
    #[error("unknown entity reference &{0};")]
    UnknownEntity(String),
    /// The document has no root element.
    #[error("the document has no root element")]
    NoRootElement,
    /// The document has more than one root element.
    #[error("the document has more than one root element")]
    MultipleRootElements,
    /// An element is not closed before the end of the document.
    #[error("element <{0}> is not closed")]
    UnclosedElement(String),
    /// Character data appears outside the root element.
    #[error("text outside the root element")]
    TextOutsideRoot,
    /// An element or attribute name, or a character, is not allowed in
    /// XML 1.0.
    #[error("invalid content: {0}")]
    InvalidContent(BuildError),
    /// The document is not well-formed XML.
    #[error("malformed XML at byte {position}: {message}")]
    Syntax {
        /// Offset in the decoded document where the error was detected.
        position: u64,
        /// Description from the XML reader.
        message: String,
    },
}

/// Returned when a message cannot be built from an element tree.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BuildError {
    /// An element or attribute name is not a valid XML name.
    #[error("invalid XML name {0:?}")]
    InvalidName(String),
    /// A value contains a character that XML 1.0 cannot represent.
    #[error("{context} contains the character U+{code:04X}, which XML 1.0 cannot represent")]
    InvalidCharacter {
        /// Where the character was found.
        context: String,
        /// The offending code point.
        code: u32,
    },
    /// An element has two attributes with the same name.
    #[error("element <{element}> has a duplicate attribute {attribute:?}")]
    DuplicateAttribute {
        /// The element name.
        element: String,
        /// The repeated attribute name.
        attribute: String,
    },
}

/// Returned when the host cannot perform a conversation action.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ConversationError {
    /// The conversation has ended.
    #[error("the conversation is closed")]
    Closed,
    /// The device has not yet completed the hello and status exchange.
    #[error("the device has not completed the hello and status exchange")]
    NotReady,
    /// Another host message is still waiting for its acknowledgment.
    #[error("another host message is waiting for an acknowledgment")]
    Busy,
    /// No delivery with this control ID is waiting for an acknowledgment.
    #[error("no delivery with control ID {0:?} is waiting for an acknowledgment")]
    UnknownDelivery(String),
    /// A message could not be built.
    #[error(transparent)]
    Build(#[from] BuildError),
}
