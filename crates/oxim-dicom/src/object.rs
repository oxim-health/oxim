//! DICOM objects parsed into memory for filters and transformers.

use std::panic::{AssertUnwindSafe, catch_unwind};

use dicom_core::dictionary::DataDictionary;
use dicom_core::ops::AttributeSelector;
use dicom_core::{DicomValue, Tag};
use dicom_dictionary_std::{StandardDataDictionary, tags};
use dicom_object::InMemDicomObject;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex, TransferSyntaxRegistry};

use crate::error::DicomError;
use crate::part10::{self, FileMeta};
use crate::scan::{self, Syntax};

/// Runs a dicom-rs operation, turning a panic on malformed input into an
/// error instead of taking the channel down.
fn guarded<T>(what: &str, operation: impl FnOnce() -> T) -> Result<T, DicomError> {
    catch_unwind(AssertUnwindSafe(operation))
        .map_err(|_| DicomError::Invalid(format!("{what} failed unexpectedly")))
}

/// Parses an attribute selector: a keyword (`PatientID`), a tag
/// (`(0010,0020)`, `0010,0020`, `00100020`) or a nested path
/// (`RequestAttributesSequence[0].AccessionNumber`).
pub fn parse_selector(text: &str) -> Result<AttributeSelector, DicomError> {
    StandardDataDictionary
        .parse_selector(text.trim())
        .map_err(|e| DicomError::Invalid(format!("invalid attribute {text:?}: {e}")))
}

/// Decodes a data set in `transfer_syntax`, after checking its structure.
pub(crate) fn read_dataset(
    dataset: &[u8],
    transfer_syntax: &str,
) -> Result<InMemDicomObject, DicomError> {
    scan::check(dataset, Syntax::of(transfer_syntax)?)?;
    let registry = TransferSyntaxRegistry;
    let ts = registry.get(transfer_syntax).ok_or_else(|| {
        DicomError::Unsupported(format!("unknown transfer syntax {transfer_syntax}"))
    })?;
    guarded("decoding the data set", || {
        InMemDicomObject::read_dataset_with_ts(dataset, ts).map_err(|e| e.to_string())
    })?
    .map_err(|e| DicomError::Invalid(format!("cannot decode the data set: {e}")))
}

/// Encodes a data set in `transfer_syntax`. Sequences and items are written
/// with undefined length, so edits never leave stale lengths behind.
pub(crate) fn write_dataset(
    dataset: &InMemDicomObject,
    transfer_syntax: &str,
) -> Result<Vec<u8>, DicomError> {
    let registry = TransferSyntaxRegistry;
    let ts = registry.get(transfer_syntax).ok_or_else(|| {
        DicomError::Unsupported(format!("unknown transfer syntax {transfer_syntax}"))
    })?;
    let mut out = Vec::new();
    guarded("encoding the data set", || {
        dataset
            .write_dataset_with_ts(&mut out, ts)
            .map_err(|e| e.to_string())
    })?
    .map_err(|e| DicomError::Invalid(format!("cannot encode the data set: {e}")))?;
    Ok(out)
}

/// The text of a primitive value without padding; multiple values are
/// separated by `\`.
pub(crate) fn value_text(
    value: &DicomValue<InMemDicomObject, dicom_object::mem::InMemFragment>,
) -> Option<String> {
    let text = value.to_str().ok()?;
    Some(
        text.split('\\')
            .map(|part| part.trim_matches(['\0', ' ']))
            .collect::<Vec<_>>()
            .join("\\"),
    )
}

/// A Part 10 object decoded into memory.
///
/// Objects are held in memory in full, including pixel data; the
/// `max_object_size` of the `dicom-scp` source bounds what is received.
#[derive(Debug, Clone)]
pub struct DicomObject {
    /// The file meta information.
    pub meta: FileMeta,
    /// The data set.
    pub dataset: InMemDicomObject,
}

impl DicomObject {
    /// Parses a Part 10 object. Deflated and unknown transfer syntaxes are
    /// not supported.
    pub fn parse(bytes: &[u8]) -> Result<Self, DicomError> {
        let part10 = part10::parse(bytes)?;
        let dataset = read_dataset(part10.dataset, &part10.meta.transfer_syntax)?;
        Ok(Self {
            meta: part10.meta,
            dataset,
        })
    }

    /// Encodes the object as Part 10 in its transfer syntax. The meta
    /// information takes the SOP class and instance UIDs of the data set.
    pub fn to_bytes(&self) -> Result<Vec<u8>, DicomError> {
        let dataset = write_dataset(&self.dataset, &self.meta.transfer_syntax)?;
        let mut meta = self.meta.clone();
        if let Some(uid) = self.text_at(tags::SOP_CLASS_UID) {
            meta.sop_class_uid = uid;
        }
        if let Some(uid) = self.text_at(tags::SOP_INSTANCE_UID) {
            meta.sop_instance_uid = uid;
        }
        Ok(part10::encode(&meta, &dataset))
    }

    fn text_at(&self, tag: Tag) -> Option<String> {
        self.dataset
            .get(tag)
            .and_then(|element| value_text(element.value()))
    }

    /// Whether the attribute at `selector` is present.
    pub fn contains(&self, selector: &AttributeSelector) -> bool {
        self.dataset.value_at(selector.clone()).is_ok()
    }

    /// The text of the attribute at `selector`, if it is present and not
    /// a sequence. Multiple values are separated by `\`.
    pub fn text(&self, selector: &AttributeSelector) -> Option<String> {
        self.dataset
            .value_at(selector.clone())
            .ok()
            .and_then(value_text)
    }
}
