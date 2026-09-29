//! DICOM for OXIM: storage, query/retrieve, modality worklist, performed
//! procedure steps, storage commitment, DICOMweb and de-identification.
//!
//! Channels with `data_type: dicom` carry DICOM objects as Part 10 bytes
//! (see [`part10`]). This crate adds:
//!
//! | Type | Kind | What it does |
//! |---|---|---|
//! | `dicom-scp` | source | Storage SCP (C-STORE, C-ECHO), optionally storage commitment ([`DicomScp`]) |
//! | `dicom-qr-scp` | source | C-FIND SCP (Patient and Study Root) over the instance index |
//! | `dicom-mwl-scp` | source | Modality Worklist SCP, optionally with MPPS |
//! | `dicom-mpps-scp` | source | Modality Performed Procedure Step SCP (N-CREATE, N-SET) |
//! | `dicom-retrieved` | source | Stores what `dicom-get` and `dicomweb-wado` retrieve ([`DicomRetrieved`]) |
//! | `dicom-scu` | destination | Storage SCU (C-STORE), optionally with storage commitment ([`DicomScu`]) |
//! | `dicom-find` | destination | C-FIND SCU ([`DicomFind`]) |
//! | `dicom-move` | destination | C-MOVE SCU ([`DicomRetrieve`]) |
//! | `dicom-get` | destination | C-GET SCU ([`DicomRetrieve`]) |
//! | `dicomweb-stow` | destination | DICOMweb STOW-RS ([`StowDestination`]) |
//! | `dicomweb-qido` | destination | DICOMweb QIDO-RS ([`QidoDestination`]) |
//! | `dicomweb-wado` | destination | DICOMweb WADO-RS ([`WadoDestination`]) |
//! | `dicom-tag` | filter | Keeps messages by the value of an attribute ([`DicomTagFilter`]) |
//! | `dicom-set` | transformer | Sets and removes attributes ([`DicomSet`]) |
//! | `dicom-deidentify` | transformer | PS3.15 Basic Profile de-identification ([`Deidentify`]) |
//! | `dicom-index` | transformer | Records objects in the instance index ([`DicomIndex`]) |
//! | `worklist-from-orders` | transformer | Schedules worklist items from normalized orders ([`WorklistFromOrders`]) |
//! | `hl7v2-orm-status` | encoder | A performed procedure step as an HL7 v2 status update ([`OrmStatusEncoder`]) |
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
//! Every network component supports TLS with the settings of
//! [`oxim_connectors::tls`]. Objects are held in memory in full while they
//! are received, stored, filtered, transformed and sent; the
//! `max_object_size` of the SCP bounds them (512 MiB by default). Pixel
//! data is never decoded: compressed objects are forwarded as they are, and
//! only native (uncompressed) data sets are converted between transfer
//! syntaxes. Deflated transfer syntaxes are not supported.
//!
//! The components of one engine share a [`DicomEnvironment`]: the instance
//! index, the worklist and the inboxes of `dicom-retrieved` sources.

mod assoc;
mod deidentify;
mod dicomweb;
pub mod dimse;
mod environment;
mod error;
mod index;
mod mpps;
mod net;
mod object;
pub mod part10;
pub mod query;
mod retrieve;
mod scan;
mod scp;
mod scu;
mod steps;
mod stow;
pub mod uids;
mod web;
mod worklist;

pub use deidentify::{Deidentify, DeidentifySettings};
pub use dicomweb::{QidoDestination, QidoLevel, QidoSettings, WadoDestination, WadoSettings};
pub use environment::{CommitmentReport, DicomEnvironment};
pub use error::DicomError;
pub use index::{DicomIndex, DicomIndexSettings, IndexStore, IndexedInstance};
pub use mpps::{OrmStatusEncoder, OrmStatusSettings};
pub use object::{DicomObject, parse_selector};
pub use retrieve::{
    Counts, DicomFind, DicomFindSettings, DicomRetrieve, DicomRetrieveSettings, DicomRetrieved,
    DicomRetrievedSettings, FindResult, Model,
};
pub use scp::{DEFAULT_MAX_OBJECT_SIZE, DicomScp, DicomScpSettings, DicomServiceSettings};
pub use scu::{CommitmentSettings, DicomScu, DicomScuSettings, StoreResponse};
pub use steps::{DicomSet, DicomSetSettings, DicomTagFilter, DicomTagFilterSettings};
pub use stow::{StowDestination, StowSettings};
pub use worklist::{
    ProcedureCode, WorklistFromOrders, WorklistFromOrdersSettings, WorklistItem, WorklistStore,
};

/// Registers the DICOM components with an in-memory
/// [`DicomEnvironment`].
pub fn register(registry: &mut oxim_core::Registry) {
    register_with(registry, &DicomEnvironment::in_memory());
}

/// Registers the DICOM components sharing `environment`.
pub fn register_with(registry: &mut oxim_core::Registry, environment: &DicomEnvironment) {
    scp::register(registry, environment);
    scu::register(registry, environment);
    retrieve::register(registry, environment);
    stow::register(registry);
    dicomweb::register(registry, environment);
    steps::register(registry);
    deidentify::register(registry);
    index::register(registry, environment);
    worklist::register(registry, environment);
    mpps::register(registry);
}
