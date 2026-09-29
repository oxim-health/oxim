use oxim_model::ChannelId;
use oxim_store::StoreError;
use thiserror::Error;

/// Errors returned by the engine.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The message store failed.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// A channel configuration is invalid.
    #[error("invalid configuration: {0}")]
    Config(String),
    /// A configuration names a connector or step type that is not registered.
    #[error("unknown {kind} type {name:?}")]
    UnknownType {
        /// What kind of component, for example `source connector`.
        kind: &'static str,
        /// The type name from the configuration.
        name: String,
    },
    /// The channel is already running.
    #[error("channel {0} is already deployed")]
    AlreadyDeployed(ChannelId),
    /// The channel is not running.
    #[error("channel {0} is not deployed")]
    NotDeployed(ChannelId),
    /// The engine or channel is stopping.
    #[error("the engine is shutting down")]
    ShuttingDown,
}

/// An error raised by a pipeline step (parser, filter, transformer,
/// normalizer or encoder). The message is marked as errored and can be
/// reprocessed after the cause is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{step}: {message}")]
pub struct StepError {
    /// The step that failed, for example `parse` or `filter[0]`.
    pub step: String,
    /// What went wrong.
    pub message: String,
}

impl StepError {
    /// Creates a step error.
    pub fn new(step: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            step: step.into(),
            message: message.into(),
        }
    }
}

/// An error returned by a destination connector for one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct SendError {
    /// What went wrong.
    pub message: String,
    /// Whether retrying cannot help, for example because the receiver
    /// rejected the message (HL7 `AR`). Permanent errors fail the delivery
    /// immediately; other errors follow the retry policy.
    pub permanent: bool,
}

impl SendError {
    /// A temporary failure, such as a refused connection or a timeout.
    pub fn temporary(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permanent: false,
        }
    }

    /// A permanent failure, such as a rejection by the receiver.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permanent: true,
        }
    }
}

/// An error that stops a source connector. The engine logs it and restarts
/// the connector after a delay.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ConnectorError(pub String);
