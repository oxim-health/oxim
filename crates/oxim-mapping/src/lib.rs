//! Mappings between protocol messages and the OXIM normalized clinical
//! model.
//!
//! Every protocol OXIM speaks is mapped to [`oxim_model::ClinicalContent`]
//! and back, so a result from an ASTM analyzer and one from a POCT device
//! leave OXIM in the same shape:
//!
//! | Module | From messages | To messages |
//! |---|---|---|
//! | [`astm`] | results, QC, orders and host queries | orders and host query answers (`H`, `P`, `O`, `L`) |
//! | [`hl7`] | `ORU`/`OUL` results, `ORM`/`OML` orders, `QRY`/`QBP` queries | `ORU^R01` results and QC, `OML^O21` orders, `OML^O33` work orders, `RSP^K11` query answers |
//! | [`poct1a`] | `OBS` observations and QC, `DST`/`EVS` device events | not needed: devices do not accept results |
//!
//! Each module documents its field-by-field rules as tables. Values are
//! carried, never interpreted (ADR 0011): numbers stay exact decimal text,
//! times keep their recorded precision, abnormal flags pass through as
//! codes, and units are never converted.
//!
//! [`register`] adds the normalizers and the encoders `hl7v2-oru-r01`,
//! `hl7v2-oml-o21`, `hl7v2-oml-o33`, `hl7v2-rsp-k11`, `astm-orders`,
//! `astm-query-response` and `clinical-json` to an engine [`Registry`](oxim_core::Registry), so
//! channels can write:
//!
//! ```yaml
//! source:
//!   type: astm-tcp
//!   data_type: astm
//!   normalize: true
//! destinations:
//!   - id: lis
//!     type: mllp
//!     encoder:
//!       type: hl7v2-oru-r01
//!       sending_application: OXIM
//!       receiving_application: LIS
//!       utc_offset: 180
//! ```
//!
//! Host queries are answered with `astm-query-response` (ASTM analyzers) or
//! `hl7v2-rsp-k11` (HL7 analyzers, IHE LAW), from the orders that the
//! `answer-query` step of `oxim-lab` finds; IHE LAW analyzers receive the
//! work orders themselves as `OML^O33` (`hl7v2-oml-o33`).

pub mod astm;
pub mod codes;
mod error;
pub mod hl7;
pub mod poct1a;
mod steps;
pub mod values;

pub use error::{MappingError, MappingResult};
pub use steps::{
    AstmNormalizer, AstmOrdersEncoder, ClinicalJsonEncoder, Hl7Normalizer, OmlEncoder, OruEncoder,
    Poct1aNormalizer, QueryResponseEncoder, WorkOrderEncoder, astm_settings, hl7_settings,
    register,
};
