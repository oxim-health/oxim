//! Shared data types for OXIM.
//!
//! - [`Envelope`]: a message as received, with its [`MessageId`], channel,
//!   connector, [`Timestamp`] and [`DataType`].
//! - Identifiers: time-ordered [`MessageId`]s (ULIDs) and validated names
//!   for channels, connectors and devices.
//! - The normalized clinical model ([`ClinicalContent`] and friends): a
//!   small, FHIR-aligned subset that every protocol maps to and from.
//! - [`ClinicalDateTime`] and [`Decimal`], which keep clinical times and
//!   numbers exactly as recorded.
//!
//! Like every OXIM library crate, this crate performs no I/O and never reads
//! the clock or a random source.

mod clinical;
mod decimal;
mod envelope;
mod id;
mod time;

pub use clinical::{
    AdministrativeSex, ClinicalContent, CodeableConcept, Coding, Comparator, Device, DeviceEvent,
    EventSeverity, HumanName, Identifier, Observation, ObservationStatus, ObservationValue, Order,
    OrderControl, OrderGroup, Patient, Priority, QcResult, Quantity, ReferenceRange, ResultGroup,
    Specimen, SpecimenQuery,
};
pub use decimal::{Decimal, InvalidDecimal};
pub use envelope::{
    DataType, DestinationStatus, Envelope, MessageStatus, UnknownDataType, UnknownStatus,
};
pub use id::{
    ChannelId, ConnectorId, DeviceId, InvalidMessageId, InvalidName, MessageId, MessageIdGenerator,
};
pub use time::{ClinicalDateTime, InvalidDateTime, Precision, Timestamp};
