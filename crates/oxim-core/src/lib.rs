//! The OXIM channel runtime.
//!
//! A channel connects one source to any number of destinations:
//!
//! ```text
//! source connector ──submit──▶ store (durable) ──▶ processor ──▶ per-destination queues ──▶ destination workers
//!        ▲                         │                  parse, normalize,          │                    │
//!        └──── acknowledge ◀───────┘                  filter, transform, encode  └── retry policy ◀───┘
//! ```
//!
//! - A source connector hands each message to [`SourceContext::submit`],
//!   which returns only after the message is stored durably; the connector
//!   then acknowledges the sender (ADR 0004).
//! - The processor parses the message ([`Document`]), optionally maps it to
//!   the normalized clinical model, runs channel filters and transformers,
//!   then each destination's own filters, transformers and encoder, and
//!   records everything atomically.
//! - One worker per destination delivers its queue in order, retrying with
//!   exponential backoff (ADR 0005).
//! - After a crash or redeploy, in-flight deliveries return to their queues
//!   and unprocessed messages are processed: delivery is at-least-once.
//!
//! Connector, filter, transformer, normalizer and encoder types are looked
//! up by name in a [`Registry`], so configuration files ([`ChannelConfig`])
//! can use components from any crate.

mod clock;
pub mod config;
mod connector;
mod document;
mod engine;
mod error;
mod pipeline;
mod registry;
pub mod steps;
mod store_actor;

pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{
    ChannelConfig, DestinationConfig, ResponseConfig, ResponseMode, Settings, SourceConfig,
    StepConfig,
};
pub use connector::{
    DestinationConnector, PendingReply, Reply, SourceConnector, SourceContext, SubmitInfo,
};
pub use document::{Document, DocumentParser};
pub use engine::{Engine, EngineOptions};
pub use error::{ConnectorError, EngineError, SendError, StepError};
pub use pipeline::{
    CompiledDestination, CompiledPipeline, Encoded, Encoder, Filter, MessageContext, Normalizer,
    PassthroughEncoder, Transformer,
};
pub use registry::Registry;
pub use store_actor::StoreHandle;

/// Re-exported so connector crates implement the traits with the same
/// macro version.
pub use async_trait::async_trait;
