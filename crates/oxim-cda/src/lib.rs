//! HL7 CDA R2 documents.
//!
//! `oxim-cda` works on the lossless XML documents of `oxim-formats`, so a
//! CDA document that passes through OXIM unmodified keeps every byte:
//!
//! - [`header`] reads the document header: identifier, type code, title,
//!   time, confidentiality, language, patients (`recordTarget`), authors
//!   and custodian.
//! - [`sections`] reads the structured body: section codes, titles,
//!   narrative text, entries and nested sections.
//! - [`lab_results`] maps laboratory result entries to normalized
//!   `Results` (see the [`results`] module for the rules).
//! - [`encode_lab_report`] writes a minimal CDA R2 laboratory report from
//!   normalized results (see the [`generate`] module).
//! - [`register`] adds the `cda` normalizer and the `cda-lab-report`
//!   encoder to an engine registry.
//!
//! Values are carried, never interpreted (ADR 0011): numbers keep their
//! exact text, abnormal flags pass through as codes, units are never
//! converted. Code system OIDs are mapped to the URIs of the normalized
//! model (LOINC, SNOMED CT, UCUM, HL7 interpretation codes; others become
//! `urn:oid:`), see [`codes`]. Namespace URIs are not resolved: elements are matched by
//! local name.
//!
//! ```
//! use oxim_cda::{header, lab_results};
//! use oxim_formats::{XmlDocument, XmlOptions};
//!
//! let xml = br#"<ClinicalDocument xmlns="urn:hl7-org:v3" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
//!   <code code="11502-2" codeSystem="2.16.840.1.113883.6.1"/>
//!   <title>Laboratory Report</title>
//!   <recordTarget><patientRole><id root="2.999.1" extension="SYN-1"/></patientRole></recordTarget>
//!   <component><structuredBody><component><section>
//!     <entry><observation classCode="OBS" moodCode="EVN">
//!       <code code="2345-7" codeSystem="2.16.840.1.113883.6.1" displayName="Glucose"/>
//!       <value xsi:type="PQ" value="5.40" unit="mmol/L"/>
//!     </observation></entry>
//!   </section></component></structuredBody></component>
//! </ClinicalDocument>"#;
//! let document = XmlDocument::parse(xml, &XmlOptions::default())?;
//! assert_eq!(header(&document)?.title.as_deref(), Some("Laboratory Report"));
//! let results = lab_results(&document)?;
//! # let _ = results;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod codes;
mod error;
pub mod generate;
mod header;
pub mod results;
mod steps;

pub use error::{CdaError, CdaResult};
pub use generate::{CdaEncoding, encode_lab_report, message_uuid};
pub use header::{Author, CdaHeader, Entry, Organization, Section, header, sections};
pub use results::lab_results;
pub use steps::{CdaLabReportEncoder, CdaNormalizer, cda_settings, register};
