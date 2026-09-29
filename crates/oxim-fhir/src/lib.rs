//! HL7 FHIR R4 (4.0.1) for OXIM.
//!
//! - [`Resource`]: typed Patient, Specimen, ServiceRequest, Observation,
//!   DiagnosticReport, Device, Bundle and OperationOutcome, with the
//!   datatypes they use ([`datatypes`]). Other resource types are kept as
//!   JSON objects. Elements and extensions that are not modeled are kept
//!   in each type's `extra` map, so they survive a round trip, and
//!   [`FhirDecimal`] keeps the exact text of decimals (`5.40` stays
//!   `5.40`).
//! - [`validate`]: structural checks returning OperationOutcome issues.
//! - [`encode_bundle`]: the normalized model → a transaction (or
//!   collection) Bundle; [`normalize`]: FHIR → the normalized model. The
//!   [`to_fhir`] and [`from_fhir`] modules document the mapping.
//! - [`summarize_response`]: the outcome of a transaction response Bundle
//!   or an OperationOutcome.
//! - [`register`]: the normalizer for [`DataType::Fhir`](oxim_model::DataType::Fhir)
//!   and the `fhir-bundle` and `fhir-resource-json` encoders.
//!
//! Only the JSON format is supported; FHIR XML is future work. Values are
//! carried, never interpreted (ADR 0011). Like every OXIM library crate,
//! this crate performs no I/O: bundles are sent to a FHIR server by the
//! `http` destination.
//!
//! ```yaml
//! source:
//!   type: mllp
//!   listen: 0.0.0.0:2575
//!   data_type: hl7v2
//!   normalize: true
//! destinations:
//!   - id: fhir
//!     type: http
//!     url: https://fhir.example.test/r4
//!     content_type: application/fhir+json
//!     encoder:
//!       type: fhir-bundle
//!       patient_identifier_system: urn:oid:2.16.840.1.113883.19.5
//!       utc_offset: 180
//! ```

pub mod datatypes;
mod decimal;
mod error;
pub mod from_fhir;
mod ids;
mod resources;
mod response;
mod steps;
pub mod to_fhir;
mod validate;

pub use decimal::FhirDecimal;
pub use error::{FhirError, FhirResult};
pub use from_fhir::normalize;
pub use ids::{entry_url, entry_uuid};
pub use resources::{
    Bundle, BundleEntry, BundleRequest, BundleResponse, Device, DeviceName, DeviceVersion,
    DiagnosticReport, Issue, Observation, ObservationReferenceRange, OperationOutcome, Patient,
    Resource, ServiceRequest, Specimen, SpecimenCollection, SpecimenContainer,
};
pub use response::{EntryOutcome, ResponseSummary, summarize, summarize_response};
pub use steps::{FhirBundleEncoder, FhirNormalizer, FhirResourceEncoder, fhir_settings, register};
pub use to_fhir::{BundleType, FhirEncoding, encode_bundle};
pub use validate::{validate, validate_to_outcome};
