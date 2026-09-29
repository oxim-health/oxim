//! DICOM routing for OXIM.
//!
//! Channels with `data_type: dicom` carry DICOM objects as Part 10 bytes
//! (see [`part10`]). This crate adds:
//!
//! | Type | Kind | What it does |
//! |---|---|---|
//! | `dicom-scp` | source | Storage SCP: receives objects with C-STORE, answers C-ECHO ([`DicomScp`]) |
//! | `dicom-scu` | destination | Storage SCU: sends each object with C-STORE ([`DicomScu`]) |
//! | `dicomweb-stow` | destination | DICOMweb STOW-RS ([`StowDestination`]) |
//! | `dicom-tag` | filter | Keeps messages by the value of an attribute ([`DicomTagFilter`]) |
//! | `dicom-set` | transformer | Sets and removes attributes ([`DicomSet`]) |
//! | `dicom-deidentify` | transformer | PS3.15 Basic Profile de-identification ([`Deidentify`]) |
//!
//! ```yaml
//! id: ct-to-pacs
//! source:
//!   type: dicom-scp
//!   data_type: dicom
//!   settings:
//!     listen: 0.0.0.0:11112
//!     ae_title: OXIM
//!     calling_ae_titles: [CT1]
//! destinations:
//!   - id: pacs
//!     type: dicom-scu
//!     settings:
//!       target: pacs.example:104
//!       called_ae_title: PACS
//! ```
//!
//! Objects are held in memory in full while they are received, stored,
//! filtered, transformed and sent; the `max_object_size` of the SCP bounds
//! them (512 MiB by default). Pixel data is never decoded: compressed
//! objects are forwarded as they are, and only native (uncompressed) data
//! sets are converted between transfer syntaxes. Deflated transfer
//! syntaxes are not supported.
//!
//! The clinical model has no imaging resources, so there is no DICOM
//! normalizer: route with [`DicomTagFilter`] and the `dicom.*` message
//! metadata instead.

mod deidentify;
pub mod dimse;
mod error;
mod net;
mod object;
pub mod part10;
mod scan;
mod scp;
mod scu;
mod steps;
mod stow;
pub mod uids;

pub use deidentify::{Deidentify, DeidentifySettings};
pub use error::DicomError;
pub use object::{DicomObject, parse_selector};
pub use scp::{DEFAULT_MAX_OBJECT_SIZE, DicomScp, DicomScpSettings};
pub use scu::{DicomScu, DicomScuSettings, StoreResponse};
pub use steps::{DicomSet, DicomSetSettings, DicomTagFilter, DicomTagFilterSettings};
pub use stow::{StowDestination, StowSettings};

/// Registers the DICOM source, destinations and steps.
pub fn register(registry: &mut oxim_core::Registry) {
    scp::register(registry);
    scu::register(registry);
    stow::register(registry);
    steps::register(registry);
    deidentify::register(registry);
}
