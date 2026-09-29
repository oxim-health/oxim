//! Sans-IO POCT1-A device messaging.
//!
//! POCT1-A (CLSI POCT01) connects point-of-care devices such as blood gas
//! analyzers and glucose meters to a data manager. Each message is an XML
//! document whose root element names the message type (`HEL.R01`,
//! `DST.R01`, `OBS.R01`, `ACK.R01`, ...), and most values are carried in a
//! `V` attribute (`<HDR.control_id V="12"/>`).
//!
//! This crate provides:
//!
//! - [`Splitter`]: finds document boundaries in a TCP byte stream, bounded
//!   and incremental, reporting junk and oversized documents.
//! - [`Message`]: parses one document into an [`Element`] tree while keeping
//!   the original bytes, rejects document type declarations and unknown
//!   entities, and offers typed views of headers, device information, status,
//!   acknowledgments and [`Observation`]s.
//! - [`builder`]: acknowledgments, requests, end-of-topic, termination,
//!   directives and keep-alives.
//! - [`HostConversation`]: the host (data manager) side of a conversation as
//!   a sans-IO state machine with timeouts.
//!
//! The crate performs no I/O and never reads the clock.
//!
//! ```
//! use oxim_poct1a::{Message, SplitEvent, Splitter};
//!
//! let mut splitter = Splitter::default();
//! splitter.push(br#"<OBS.R01><HDR><HDR.control_id V="7"/></HDR><SVC><PT>
//!   <PT.patient_id V="P1"/>
//!   <OBS><OBS.observation_id V="GLU"/><OBS.value V="5.4" U="mmol/L"/></OBS>
//! </PT></SVC></OBS.R01>"#);
//!
//! let Some(SplitEvent::Document(bytes)) = splitter.next_event() else {
//!     panic!("expected a document");
//! };
//! let message = Message::parse(&bytes)?;
//! assert_eq!(message.control_id(), Some("7"));
//! let observation = message.observations()[0];
//! assert_eq!(observation.observation_id(), Some("GLU"));
//! assert_eq!((observation.value(), observation.unit()), (Some("5.4"), Some("mmol/L")));
//! # Ok::<(), oxim_poct1a::ParseError>(())
//! ```

pub mod builder;
mod content;
pub mod conversation;
mod element;
mod error;
mod message;
mod splitter;

pub use builder::Header;
pub use content::{AckInfo, AckType, DeviceInfo, DeviceStatus, Observation, ObservationKind};
pub use conversation::{
    CloseReason, ConversationState, DataAcknowledgment, DeliveryOutcome, HostConfig,
    HostConversation, Now, Output,
};
pub use element::{Attribute, Descendants, Element, Node};
pub use error::{BuildError, ConversationError, ParseError};
pub use message::{Message, MessageKind, ParseOptions, WriteOptions};
pub use splitter::{DiscardReason, SplitEvent, Splitter, SplitterOptions};
