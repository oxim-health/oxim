//! Query identifiers and C-FIND matching (PS3.4 C.2.2.2).
//!
//! An identifier can be written three ways wherever OXIM takes one (the
//! payload of `dicom-find`, `dicom-move`, `dicom-get`, `dicomweb-qido` and
//! `dicomweb-wado` deliveries):
//!
//! - a JSON map of attribute names to values, for example
//!   `{"QueryRetrieveLevel": "STUDY", "PatientID": "SYN-0001",
//!   "StudyDate": "20240101-20240131", "StudyInstanceUID": null}`: names
//!   are keywords or tags, `null` or `""` asks for the attribute as a
//!   return key, arrays are multiple values and objects (or arrays of
//!   objects) are sequence items;
//! - a DICOM JSON object (PS3.18 F.2);
//! - a DICOM data set, as a Part 10 object.

use dicom_core::header::Header;
use dicom_core::value::{DataSetSequence, Value};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use serde_json::Value as Json;

use crate::error::DicomError;
use crate::object::{self, parse_selector, value_text};
use crate::part10;
use crate::steps::{dictionary_vr, typed_value};

/// A text element of `vr`; an empty text gives an empty value.
pub(crate) fn text_element(tag: Tag, vr: VR, text: &str) -> DataElement<InMemDicomObject> {
    let value = if text.is_empty() {
        PrimitiveValue::Empty
    } else {
        match vr {
            VR::LT | VR::ST | VR::UT | VR::UR => PrimitiveValue::Str(text.to_owned()),
            _ => PrimitiveValue::Strs(text.split('\\').map(str::to_owned).collect()),
        }
    };
    DataElement::new(tag, vr, value)
}

/// A sequence element.
pub(crate) fn sequence(tag: Tag, items: Vec<InMemDicomObject>) -> DataElement<InMemDicomObject> {
    DataElement::new(tag, VR::SQ, DataSetSequence::from(items))
}

/// The text of a top-level attribute without padding.
pub(crate) fn text(dataset: &InMemDicomObject, tag: Tag) -> Option<String> {
    dataset
        .get(tag)
        .and_then(|element| value_text(element.value()))
        .filter(|text| !text.is_empty())
}

/// The items of a top-level sequence.
pub(crate) fn items(dataset: &InMemDicomObject, tag: Tag) -> &[InMemDicomObject] {
    dataset
        .get(tag)
        .and_then(|element| element.value().items())
        .unwrap_or_default()
}

/// Whether a JSON object is a DICOM JSON data set: every key is eight
/// hexadecimal digits and every value an object with a `vr`.
fn is_dicom_json(map: &serde_json::Map<String, Json>) -> bool {
    !map.is_empty()
        && map.iter().all(|(key, value)| {
            key.len() == 8
                && key.bytes().all(|b| b.is_ascii_hexdigit())
                && value.get("vr").is_some()
        })
}

fn json_text(name: &str, value: &Json) -> Result<String, DicomError> {
    match value {
        Json::Null => Ok(String::new()),
        Json::String(text) => Ok(text.clone()),
        Json::Number(number) => Ok(number.to_string()),
        Json::Bool(flag) => Ok(flag.to_string()),
        Json::Array(values) => values
            .iter()
            .map(|value| json_text(name, value))
            .collect::<Result<Vec<_>, _>>()
            .map(|parts| parts.join("\\")),
        Json::Object(_) => Err(DicomError::Invalid(format!(
            "{name}: an object is only allowed for sequences"
        ))),
    }
}

/// Builds a data set from a map of attribute names to values.
pub fn from_keywords(map: &serde_json::Map<String, Json>) -> Result<InMemDicomObject, DicomError> {
    let mut dataset = InMemDicomObject::new_empty();
    for (name, value) in map {
        let selector = parse_selector(name)?;
        let mut steps = selector.iter();
        let (Some(dicom_core::ops::AttributeSelectorStep::Tag(tag)), None) =
            (steps.next(), steps.next())
        else {
            return Err(DicomError::Invalid(format!(
                "{name}: nested paths are written as nested objects"
            )));
        };
        let tag = *tag;
        let vr = dictionary_vr(tag).ok_or_else(|| {
            DicomError::Invalid(format!(
                "{name}: not an attribute of the standard dictionary"
            ))
        })?;
        if vr == VR::SQ {
            let items = match value {
                Json::Null => Vec::new(),
                Json::Object(item) => vec![from_keywords(item)?],
                Json::Array(values) => values
                    .iter()
                    .map(|value| match value {
                        Json::Object(item) => from_keywords(item),
                        _ => Err(DicomError::Invalid(format!(
                            "{name}: sequence items must be objects"
                        ))),
                    })
                    .collect::<Result<_, _>>()?,
                _ => {
                    return Err(DicomError::Invalid(format!(
                        "{name}: a sequence needs an object or a list of objects"
                    )));
                }
            };
            dataset.put(sequence(tag, items));
            continue;
        }
        let text = json_text(name, value)?;
        let value =
            typed_value(vr, &text).map_err(|e| DicomError::Invalid(format!("{name}: {e}")))?;
        dataset.put(DataElement::new(tag, vr, value));
    }
    Ok(dataset)
}

/// Parses an identifier from a delivery payload: a JSON map of attribute
/// names, a DICOM JSON object or a Part 10 object.
pub fn parse_identifier(payload: &[u8]) -> Result<InMemDicomObject, DicomError> {
    let trimmed = payload.trim_ascii_start();
    if trimmed.starts_with(b"{") {
        let json: Json = serde_json::from_slice(trimmed)
            .map_err(|e| DicomError::Invalid(format!("the identifier is not valid JSON: {e}")))?;
        let Json::Object(map) = json else {
            return Err(DicomError::invalid("the identifier must be a JSON object"));
        };
        if is_dicom_json(&map) {
            return dicom_json::from_value::<InMemDicomObject>(Json::Object(map))
                .map_err(|e| DicomError::Invalid(format!("invalid DICOM JSON: {e}")));
        }
        return from_keywords(&map);
    }
    let part10 = part10::parse(payload)?;
    object::read_dataset(part10.dataset, &part10.meta.transfer_syntax)
}

/// A data set as DICOM JSON (PS3.18 F.2). Bulk data is written inline.
pub fn to_json(dataset: &InMemDicomObject) -> Result<Json, DicomError> {
    dicom_json::to_value(dataset)
        .map_err(|e| DicomError::Invalid(format!("cannot write DICOM JSON: {e}")))
}

/// The value of a Query/Retrieve Level (0008,0052), upper case.
pub(crate) fn level(identifier: &InMemDicomObject) -> Option<String> {
    text(identifier, tags::QUERY_RETRIEVE_LEVEL).map(|level| level.trim().to_ascii_uppercase())
}

/// Options of the matching rules.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MatchOptions {
    /// Whether person names match regardless of letter case.
    pub(crate) fuzzy_names: bool,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self { fuzzy_names: true }
    }
}

/// `*` matches any run of characters, `?` any single character.
fn wildcard(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(position) = star {
            p = position + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// A date, time or date-time reduced to comparable digits.
fn digits(text: &str) -> String {
    text.chars()
        .take_while(|c| *c != '.' && *c != '+' && *c != '-')
        .filter(char::is_ascii_digit)
        .collect()
}

fn in_range(query: &str, candidate: &str) -> bool {
    let Some((low, high)) = query.split_once('-') else {
        return false;
    };
    let value = digits(candidate);
    if value.is_empty() {
        return false;
    }
    // Pad the bounds to the candidate's precision: a lower bound starts at
    // zeros, an upper bound ends at nines (1200 means up to 12:00:59).
    let width = value.len();
    let bound = |text: &str, fill: char| {
        let mut bound = digits(text);
        while bound.len() < width {
            bound.push(fill);
        }
        bound.truncate(width);
        bound
    };
    let low = low.trim();
    let high = high.trim();
    (low.is_empty() || value >= bound(low, '0')) && (high.is_empty() || value <= bound(high, '9'))
}

/// Whether one candidate value satisfies one query value.
fn value_matches(vr: VR, query: &str, candidate: &str, options: MatchOptions) -> bool {
    let fold = |text: &str| {
        if vr == VR::PN && options.fuzzy_names {
            text.to_uppercase()
        } else {
            text.to_owned()
        }
    };
    if matches!(vr, VR::DA | VR::TM | VR::DT) && query.contains('-') {
        return in_range(query, candidate);
    }
    let query = fold(query.trim());
    let candidate = fold(candidate.trim());
    if vr != VR::UI && (query.contains('*') || query.contains('?')) {
        let pattern: Vec<char> = query.chars().collect();
        let text: Vec<char> = candidate.chars().collect();
        return wildcard(&pattern, &text);
    }
    if vr == VR::PN {
        // "DOE^JANE" matches "DOE^JANE^^^" and vice versa.
        return query.trim_end_matches('^') == candidate.trim_end_matches('^');
    }
    query == candidate
}

/// Whether any value of the candidate matches any value of the query
/// (list matching).
fn values_match(vr: VR, query: &str, candidate: &str, options: MatchOptions) -> bool {
    query.split('\\').any(|query| {
        candidate
            .split('\\')
            .any(|candidate| value_matches(vr, query, candidate, options))
    })
}

type Element = DataElement<InMemDicomObject>;

/// Matches `candidate` against the query `identifier` and returns the
/// response: every key of the identifier with the candidate's value.
/// Returns `None` when a matching key does not match.
pub(crate) fn match_dataset(
    identifier: &InMemDicomObject,
    candidate: &InMemDicomObject,
    options: MatchOptions,
) -> Option<InMemDicomObject> {
    let mut response = InMemDicomObject::new_empty();
    for key in identifier.iter() {
        let tag = key.header().tag();
        if tag == tags::SPECIFIC_CHARACTER_SET {
            continue;
        }
        if tag == tags::QUERY_RETRIEVE_LEVEL {
            response.put(key.clone());
            continue;
        }
        let found = candidate.get(tag);
        match key.value() {
            Value::Sequence(sequence) => {
                let query_item = sequence.items().first();
                let candidate_items = found
                    .and_then(|element| element.value().items())
                    .unwrap_or_default();
                let items: Vec<InMemDicomObject> = match query_item {
                    // Universal matching: every item, with the keys asked for.
                    None => candidate_items.to_vec(),
                    Some(query_item) => {
                        let matched: Vec<InMemDicomObject> = candidate_items
                            .iter()
                            .filter_map(|item| match_dataset(query_item, item, options))
                            .collect();
                        let universal = query_item.iter().all(is_universal);
                        if matched.is_empty() && !universal {
                            return None;
                        }
                        matched
                    }
                };
                response.put(sequence_element(tag, items));
            }
            Value::Primitive(_) => {
                let query = value_text(key.value()).unwrap_or_default();
                let candidate_text = found
                    .and_then(|element| value_text(element.value()))
                    .unwrap_or_default();
                if !query.is_empty() && !values_match(key.vr(), &query, &candidate_text, options) {
                    return None;
                }
                match found {
                    Some(element) => {
                        response.put(element.clone());
                    }
                    None => {
                        response.put(DataElement::new(tag, key.vr(), PrimitiveValue::Empty));
                    }
                }
            }
            Value::PixelSequence(_) => {}
        }
    }
    Some(response)
}

fn sequence_element(tag: Tag, items: Vec<InMemDicomObject>) -> Element {
    sequence(tag, items)
}

/// Whether a key asks for a value without constraining it.
fn is_universal(element: &Element) -> bool {
    match element.value() {
        Value::Primitive(_) => value_text(element.value()).unwrap_or_default().is_empty(),
        Value::Sequence(sequence) => sequence
            .items()
            .iter()
            .all(|item| item.iter().all(is_universal)),
        Value::PixelSequence(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dataset(pairs: &[(Tag, VR, &str)]) -> InMemDicomObject {
        InMemDicomObject::from_element_iter(
            pairs
                .iter()
                .map(|(tag, vr, value)| text_element(*tag, *vr, value)),
        )
    }

    fn candidate() -> InMemDicomObject {
        let mut candidate = dataset(&[
            (tags::PATIENT_NAME, VR::PN, "SYNTHETIC^PATIENT"),
            (tags::PATIENT_ID, VR::LO, "SYN-0001"),
            (tags::STUDY_DATE, VR::DA, "20240229"),
            (tags::STUDY_TIME, VR::TM, "101500"),
            (tags::STUDY_INSTANCE_UID, VR::UI, "1.2.3"),
            (tags::MODALITIES_IN_STUDY, VR::CS, "CT\\SR"),
        ]);
        candidate.put(sequence(
            tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE,
            vec![dataset(&[
                (tags::MODALITY, VR::CS, "CT"),
                (tags::SCHEDULED_STATION_AE_TITLE, VR::AE, "CT1"),
            ])],
        ));
        candidate
    }

    #[test]
    fn matches_single_values_wildcards_ranges_and_lists() {
        let options = MatchOptions::default();
        for (tag, vr, query, expected) in [
            (tags::PATIENT_ID, VR::LO, "SYN-0001", true),
            (tags::PATIENT_ID, VR::LO, "SYN-0002", false),
            (tags::PATIENT_ID, VR::LO, "", true),
            (tags::PATIENT_NAME, VR::PN, "synthetic^*", true),
            (tags::PATIENT_NAME, VR::PN, "SYNTH?TIC^PATIENT", true),
            (tags::PATIENT_NAME, VR::PN, "OTHER*", false),
            (tags::STUDY_DATE, VR::DA, "20240201-20240301", true),
            (tags::STUDY_DATE, VR::DA, "-20240228", false),
            (tags::STUDY_DATE, VR::DA, "20240229-", true),
            (tags::STUDY_TIME, VR::TM, "1000-1015", true),
            (tags::STUDY_TIME, VR::TM, "1016-", false),
            (tags::STUDY_INSTANCE_UID, VR::UI, "1.2.9\\1.2.3", true),
            (tags::STUDY_INSTANCE_UID, VR::UI, "1.2.*", false),
            (tags::MODALITIES_IN_STUDY, VR::CS, "SR", true),
        ] {
            let query = dataset(&[(tag, vr, query)]);
            assert_eq!(
                match_dataset(&query, &candidate(), options).is_some(),
                expected,
                "{tag} {query:?}"
            );
        }
    }

    #[test]
    fn matches_sequences_and_projects_return_keys() {
        let mut query = dataset(&[
            (tags::PATIENT_ID, VR::LO, ""),
            (tags::ACCESSION_NUMBER, VR::SH, ""),
        ]);
        query.put(sequence(
            tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE,
            vec![dataset(&[
                (tags::MODALITY, VR::CS, "CT"),
                (tags::SCHEDULED_STATION_AE_TITLE, VR::AE, ""),
            ])],
        ));
        let response = match_dataset(&query, &candidate(), MatchOptions::default()).unwrap();
        assert_eq!(
            text(&response, tags::PATIENT_ID).as_deref(),
            Some("SYN-0001")
        );
        assert!(response.get(tags::ACCESSION_NUMBER).is_some());
        assert!(response.get(tags::PATIENT_NAME).is_none());
        let steps = items(&response, tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE);
        assert_eq!(steps.len(), 1);
        assert_eq!(
            text(&steps[0], tags::SCHEDULED_STATION_AE_TITLE).as_deref(),
            Some("CT1")
        );

        query.put(sequence(
            tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE,
            vec![dataset(&[(tags::MODALITY, VR::CS, "MR")])],
        ));
        assert!(match_dataset(&query, &candidate(), MatchOptions::default()).is_none());
    }

    #[test]
    fn parses_identifiers() {
        let identifier = parse_identifier(
            br#"{"QueryRetrieveLevel": "STUDY", "PatientID": "SYN-0001", "StudyDate": "20240101-20240131", "(0020,000D)": null, "ModalitiesInStudy": ["CT", "MR"], "ScheduledProcedureStepSequence": {"Modality": "CT"}}"#,
        )
        .unwrap();
        assert_eq!(level(&identifier).as_deref(), Some("STUDY"));
        assert_eq!(
            text(&identifier, tags::MODALITIES_IN_STUDY).as_deref(),
            Some("CT\\MR")
        );
        assert!(identifier.get(tags::STUDY_INSTANCE_UID).is_some());
        assert_eq!(
            items(&identifier, tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE).len(),
            1
        );

        let json = to_json(&identifier).unwrap();
        let again = parse_identifier(json.to_string().as_bytes()).unwrap();
        assert_eq!(text(&again, tags::PATIENT_ID).as_deref(), Some("SYN-0001"));

        for bad in [
            &br#"{"NoSuchKeyword": "x"}"#[..],
            br#"[1, 2]"#,
            br#"{"PatientID": {"a": 1}}"#,
            b"not an identifier",
        ] {
            assert!(
                parse_identifier(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn wildcards() {
        let m = |p: &str, t: &str| {
            wildcard(
                &p.chars().collect::<Vec<_>>(),
                &t.chars().collect::<Vec<_>>(),
            )
        };
        assert!(m("*", ""));
        assert!(m("A*C", "ABBBC"));
        assert!(m("A?C", "ABC"));
        assert!(!m("A?C", "AC"));
        assert!(m("*B*", "ABC"));
        assert!(!m("A*D", "ABC"));
    }
}
