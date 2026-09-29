//! The `dicom-deidentify` transformer: the Basic Application Level
//! Confidentiality Profile of PS3.15 annex E, with deterministic UIDs.
//!
//! - Attributes of table E.1-1 are removed (X), emptied (Z) or replaced
//!   with a dummy value (D). Where the profile allows several actions the
//!   attribute is emptied when emptying is allowed and removed otherwise;
//!   emptying keeps the object valid whether the attribute is type 2 or 3.
//! - Every UID is replaced (U), except standard UIDs (root
//!   `1.2.840.10008`) and attributes that name classes or syntaxes, such as
//!   SOP Class UID. A replacement is `2.25.` followed by a UUID derived with
//!   HMAC-SHA-256 from the secret and the original UID, so the same UID
//!   always maps to the same replacement: objects of one study stay one
//!   study, across messages and restarts, while the mapping cannot be
//!   reversed without the secret.
//! - Private attributes, curve data (`50xx`) and overlay data and comments
//!   (`60xx,3000`, `60xx,4000`) are removed.
//! - Patient Name and Patient ID become a pseudonym derived from the secret
//!   and the original Patient ID, so a patient's studies stay linked
//!   (`pseudonymize_patient: false` empties them instead).
//! - Optional date shifting implements the Retain Longitudinal Temporal
//!   Information with Modified Dates option: every DA value and the date
//!   of every DT value is shifted by a fixed number of days or by a
//!   per-patient offset derived from the secret, instead of being removed;
//!   times are kept.
//! - `keep` lists attributes kept unchanged, for options such as Retain
//!   Patient Characteristics (`PatientSex`, `PatientAge`, ...).
//! - Patient Identity Removed, De-identification Method (and its code
//!   sequence) and Longitudinal Temporal Information Modified are set; the
//!   file meta information loses its AE titles.
//!
//! Pixel data is not changed. Objects whose Burned In Annotation is `YES`
//! are rejected unless `allow_burned_in_annotation` is set. Text in
//! structured reports is removed with the Content Sequence rather than
//! cleaned.

use std::collections::BTreeSet;
use std::sync::Arc;

use dicom_core::header::Header;
use dicom_core::value::{C, DataSetSequence};
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use oxim_core::{EngineError, MessageContext, StepConfig, StepError, Transformer};
use ring::hmac;
use serde::Deserialize;

use crate::net::settings;
use crate::object::{DicomObject, parse_selector, value_text};
use crate::steps::{decode, encode, for_each_dataset, remove_private};

fn yes() -> bool {
    true
}

fn default_pseudonym_prefix() -> String {
    "ANON".to_owned()
}

/// Settings of the `dicom-deidentify` transformer.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeidentifySettings {
    /// The secret for UIDs, pseudonyms and per-patient date shifts.
    #[serde(default)]
    pub secret: Option<String>,
    /// Environment variable holding the secret, read when the channel is
    /// deployed.
    #[serde(default)]
    pub secret_env: Option<String>,
    /// Shift every date by this many days.
    #[serde(default)]
    pub date_shift_days: Option<i32>,
    /// Shift the dates of each patient by a per-patient offset between
    /// minus and plus this many days.
    #[serde(default)]
    pub date_shift_max_days: Option<u32>,
    /// Replace Patient Name and Patient ID with a pseudonym (`true`) or
    /// empty them (`false`).
    #[serde(default = "yes")]
    pub pseudonymize_patient: bool,
    /// The pseudonym prefix, followed by 16 hexadecimal digits.
    #[serde(default = "default_pseudonym_prefix")]
    pub pseudonym_prefix: String,
    /// Attributes kept unchanged.
    #[serde(default)]
    pub keep: Vec<String>,
    /// Remove private attributes.
    #[serde(default = "yes")]
    pub remove_private: bool,
    /// Accept objects with burned-in annotation, whose pixel data may show
    /// identifying text.
    #[serde(default)]
    pub allow_burned_in_annotation: bool,
}

/// What the profile does with an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// X: remove.
    Remove,
    /// Z: empty.
    Empty,
    /// D: replace with a dummy value.
    Dummy,
}

use Action::{Dummy as D, Empty as Z, Remove as X};

/// Attributes of PS3.15 table E.1-1 (Basic Profile column) other than UIDs,
/// which are all replaced. Retired attributes are included because old
/// objects still carry them.
#[allow(deprecated)]
const PROFILE: &[(Tag, Action)] = &[
    // General study, series and equipment
    (tags::INSTANCE_COERCION_DATE_TIME, X),
    (tags::INSTANCE_CREATION_DATE, X),
    (tags::INSTANCE_CREATION_TIME, X),
    (tags::STUDY_DATE, Z),
    (tags::SERIES_DATE, X),
    (tags::ACQUISITION_DATE, Z),
    (tags::CONTENT_DATE, Z),
    (tags::OVERLAY_DATE, X),
    (tags::CURVE_DATE, X),
    (tags::ACQUISITION_DATE_TIME, Z),
    (tags::STUDY_TIME, Z),
    (tags::SERIES_TIME, X),
    (tags::ACQUISITION_TIME, Z),
    (tags::CONTENT_TIME, Z),
    (tags::OVERLAY_TIME, X),
    (tags::CURVE_TIME, X),
    (tags::ACCESSION_NUMBER, Z),
    (tags::INSTITUTION_NAME, Z),
    (tags::INSTITUTION_ADDRESS, X),
    (tags::INSTITUTION_CODE_SEQUENCE, Z),
    (tags::REFERRING_PHYSICIAN_NAME, Z),
    (tags::REFERRING_PHYSICIAN_ADDRESS, X),
    (tags::REFERRING_PHYSICIAN_TELEPHONE_NUMBERS, X),
    (tags::REFERRING_PHYSICIAN_IDENTIFICATION_SEQUENCE, X),
    (tags::CONSULTING_PHYSICIAN_NAME, Z),
    (tags::CONSULTING_PHYSICIAN_IDENTIFICATION_SEQUENCE, X),
    (tags::TIMEZONE_OFFSET_FROM_UTC, X),
    (tags::STATION_NAME, Z),
    (tags::STUDY_DESCRIPTION, X),
    (tags::SERIES_DESCRIPTION, X),
    (tags::INSTITUTIONAL_DEPARTMENT_NAME, X),
    (tags::INSTITUTIONAL_DEPARTMENT_TYPE_CODE_SEQUENCE, X),
    (tags::PHYSICIANS_OF_RECORD, X),
    (tags::PHYSICIANS_OF_RECORD_IDENTIFICATION_SEQUENCE, X),
    (tags::PERFORMING_PHYSICIAN_NAME, X),
    (tags::PERFORMING_PHYSICIAN_IDENTIFICATION_SEQUENCE, X),
    (tags::NAME_OF_PHYSICIANS_READING_STUDY, X),
    (tags::PHYSICIANS_READING_STUDY_IDENTIFICATION_SEQUENCE, X),
    (tags::OPERATORS_NAME, Z),
    (tags::OPERATOR_IDENTIFICATION_SEQUENCE, X),
    (tags::ADMITTING_DIAGNOSES_DESCRIPTION, X),
    (tags::ADMITTING_DIAGNOSES_CODE_SEQUENCE, X),
    (tags::REFERENCED_STUDY_SEQUENCE, Z),
    (tags::REFERENCED_PERFORMED_PROCEDURE_STEP_SEQUENCE, Z),
    (tags::REFERENCED_PATIENT_SEQUENCE, X),
    (tags::DERIVATION_DESCRIPTION, X),
    (tags::IDENTIFYING_COMMENTS, X),
    (tags::STUDY_ID, Z),
    // Patient
    (tags::PATIENT_NAME, Z),
    (tags::PATIENT_ID, Z),
    (tags::ISSUER_OF_PATIENT_ID, X),
    (tags::TYPE_OF_PATIENT_ID, X),
    (tags::ISSUER_OF_PATIENT_ID_QUALIFIERS_SEQUENCE, X),
    (tags::SOURCE_PATIENT_GROUP_IDENTIFICATION_SEQUENCE, X),
    (tags::GROUP_OF_PATIENTS_IDENTIFICATION_SEQUENCE, X),
    (tags::PATIENT_BIRTH_DATE, Z),
    (tags::PATIENT_BIRTH_TIME, X),
    (tags::PATIENT_BIRTH_DATE_IN_ALTERNATIVE_CALENDAR, X),
    (tags::PATIENT_DEATH_DATE_IN_ALTERNATIVE_CALENDAR, X),
    (tags::PATIENT_ALTERNATIVE_CALENDAR, X),
    (tags::PATIENT_SEX, Z),
    (tags::PATIENT_INSURANCE_PLAN_CODE_SEQUENCE, X),
    (tags::PATIENT_PRIMARY_LANGUAGE_CODE_SEQUENCE, X),
    (tags::PATIENT_PRIMARY_LANGUAGE_MODIFIER_CODE_SEQUENCE, X),
    (tags::OTHER_PATIENT_I_DS, X),
    (tags::OTHER_PATIENT_NAMES, X),
    (tags::OTHER_PATIENT_I_DS_SEQUENCE, X),
    (tags::PATIENT_BIRTH_NAME, X),
    (tags::PATIENT_AGE, X),
    (tags::PATIENT_SIZE, X),
    (tags::PATIENT_WEIGHT, X),
    (tags::PATIENT_ADDRESS, X),
    (tags::INSURANCE_PLAN_IDENTIFICATION, X),
    (tags::PATIENT_MOTHER_BIRTH_NAME, X),
    (tags::MILITARY_RANK, X),
    (tags::BRANCH_OF_SERVICE, X),
    (tags::MEDICAL_RECORD_LOCATOR, X),
    (tags::REFERENCED_PATIENT_PHOTO_SEQUENCE, X),
    (tags::MEDICAL_ALERTS, X),
    (tags::ALLERGIES, X),
    (tags::COUNTRY_OF_RESIDENCE, X),
    (tags::REGION_OF_RESIDENCE, X),
    (tags::PATIENT_TELEPHONE_NUMBERS, X),
    (tags::PATIENT_TELECOM_INFORMATION, X),
    (tags::ETHNIC_GROUP, X),
    (tags::OCCUPATION, X),
    (tags::SMOKING_STATUS, X),
    (tags::ADDITIONAL_PATIENT_HISTORY, X),
    (tags::PREGNANCY_STATUS, X),
    (tags::LAST_MENSTRUAL_DATE, X),
    (tags::PATIENT_RELIGIOUS_PREFERENCE, X),
    (tags::PATIENT_SEX_NEUTERED, Z),
    (tags::RESPONSIBLE_PERSON, X),
    (tags::RESPONSIBLE_ORGANIZATION, X),
    (tags::PATIENT_COMMENTS, X),
    // Devices and acquisition
    (tags::DEVICE_SERIAL_NUMBER, Z),
    (tags::PLATE_ID, X),
    (tags::GENERATOR_ID, X),
    (tags::CASSETTE_ID, X),
    (tags::GANTRY_ID, X),
    (tags::UNIQUE_DEVICE_IDENTIFIER, X),
    (tags::UDI_SEQUENCE, X),
    (tags::PROTOCOL_NAME, X),
    (tags::ACQUISITION_DEVICE_PROCESSING_DESCRIPTION, X),
    (tags::ACQUISITION_COMMENTS, X),
    (tags::DETECTOR_ID, X),
    (tags::ACQUISITION_PROTOCOL_DESCRIPTION, X),
    (tags::CONTRIBUTION_DESCRIPTION, X),
    (tags::DATE_OF_LAST_CALIBRATION, X),
    (tags::TIME_OF_LAST_CALIBRATION, X),
    (tags::DATE_OF_LAST_DETECTOR_CALIBRATION, X),
    (tags::MODIFYING_DEVICE_ID, X),
    (tags::MODIFYING_DEVICE_MANUFACTURER, X),
    (tags::MODIFIED_IMAGE_DESCRIPTION, X),
    (tags::IMAGE_COMMENTS, X),
    (tags::FRAME_COMMENTS, X),
    (tags::IMAGE_PRESENTATION_COMMENTS, X),
    (tags::ICON_IMAGE_SEQUENCE, X),
    // Study scheduling and visits
    (tags::STUDY_ID_ISSUER, X),
    (tags::SCHEDULED_STUDY_START_DATE, X),
    (tags::SCHEDULED_STUDY_START_TIME, X),
    (tags::SCHEDULED_STUDY_LOCATION, X),
    (tags::SCHEDULED_STUDY_LOCATION_AE_TITLE, X),
    (tags::REASON_FOR_STUDY, X),
    (tags::REQUESTING_PHYSICIAN, X),
    (tags::REQUESTING_SERVICE, X),
    (tags::REQUESTED_PROCEDURE_DESCRIPTION, Z),
    (tags::REQUESTED_CONTRAST_AGENT, X),
    (tags::STUDY_COMMENTS, X),
    (tags::REFERENCED_PATIENT_ALIAS_SEQUENCE, X),
    (tags::ADMISSION_ID, X),
    (tags::ISSUER_OF_ADMISSION_ID, X),
    (tags::SCHEDULED_PATIENT_INSTITUTION_RESIDENCE, X),
    (tags::ADMITTING_DATE, X),
    (tags::ADMITTING_TIME, X),
    (tags::DISCHARGE_DIAGNOSIS_DESCRIPTION, X),
    (tags::SPECIAL_NEEDS, X),
    (tags::SERVICE_EPISODE_ID, X),
    (tags::ISSUER_OF_SERVICE_EPISODE_ID, X),
    (tags::SERVICE_EPISODE_DESCRIPTION, X),
    (tags::CURRENT_PATIENT_LOCATION, X),
    (tags::PATIENT_INSTITUTION_RESIDENCE, X),
    (tags::PATIENT_STATE, X),
    (tags::VISIT_COMMENTS, X),
    // Procedure steps and requests
    (tags::SCHEDULED_STATION_AE_TITLE, X),
    (tags::SCHEDULED_PROCEDURE_STEP_START_DATE, X),
    (tags::SCHEDULED_PROCEDURE_STEP_START_TIME, X),
    (tags::SCHEDULED_PROCEDURE_STEP_END_DATE, X),
    (tags::SCHEDULED_PROCEDURE_STEP_END_TIME, X),
    (tags::SCHEDULED_PERFORMING_PHYSICIAN_NAME, X),
    (tags::SCHEDULED_PROCEDURE_STEP_DESCRIPTION, X),
    (
        tags::SCHEDULED_PERFORMING_PHYSICIAN_IDENTIFICATION_SEQUENCE,
        X,
    ),
    (tags::SCHEDULED_STATION_NAME, X),
    (tags::SCHEDULED_PROCEDURE_STEP_LOCATION, X),
    (tags::PRE_MEDICATION, X),
    (tags::PERFORMED_STATION_AE_TITLE, X),
    (tags::PERFORMED_STATION_NAME, X),
    (tags::PERFORMED_LOCATION, X),
    (tags::PERFORMED_PROCEDURE_STEP_START_DATE, X),
    (tags::PERFORMED_PROCEDURE_STEP_START_TIME, X),
    (tags::PERFORMED_PROCEDURE_STEP_END_DATE, X),
    (tags::PERFORMED_PROCEDURE_STEP_END_TIME, X),
    (tags::PERFORMED_PROCEDURE_STEP_ID, X),
    (tags::PERFORMED_PROCEDURE_STEP_DESCRIPTION, X),
    (tags::REQUEST_ATTRIBUTES_SEQUENCE, X),
    (tags::COMMENTS_ON_THE_PERFORMED_PROCEDURE_STEP, X),
    (tags::ACQUISITION_CONTEXT_SEQUENCE, X),
    (tags::REQUESTED_PROCEDURE_ID, X),
    (tags::PATIENT_TRANSPORT_ARRANGEMENTS, X),
    (tags::REQUESTED_PROCEDURE_LOCATION, X),
    (tags::NAMES_OF_INTENDED_RECIPIENTS_OF_RESULTS, X),
    (
        tags::INTENDED_RECIPIENTS_OF_RESULTS_IDENTIFICATION_SEQUENCE,
        X,
    ),
    (tags::PERSON_ADDRESS, X),
    (tags::PERSON_TELEPHONE_NUMBERS, X),
    (tags::PERSON_TELECOM_INFORMATION, X),
    (tags::REQUESTED_PROCEDURE_COMMENTS, X),
    (tags::REASON_FOR_THE_IMAGING_SERVICE_REQUEST, X),
    (tags::ORDER_ENTERED_BY, X),
    (tags::ORDER_ENTERER_LOCATION, X),
    (tags::ORDER_CALLBACK_PHONE_NUMBER, X),
    (tags::ORDER_CALLBACK_TELECOM_INFORMATION, X),
    (tags::PLACER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST, Z),
    (tags::FILLER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST, Z),
    (tags::IMAGING_SERVICE_REQUEST_COMMENTS, X),
    (
        tags::CONFIDENTIALITY_CONSTRAINT_ON_PATIENT_DATA_DESCRIPTION,
        X,
    ),
    // Structured reporting, annotations and results
    (tags::VERIFYING_ORGANIZATION, X),
    (tags::VERIFYING_OBSERVER_SEQUENCE, D),
    (tags::VERIFYING_OBSERVER_NAME, D),
    (tags::AUTHOR_OBSERVER_SEQUENCE, X),
    (tags::PARTICIPANT_SEQUENCE, X),
    (tags::CUSTODIAL_ORGANIZATION_SEQUENCE, X),
    (tags::VERIFYING_OBSERVER_IDENTIFICATION_CODE_SEQUENCE, Z),
    (tags::PERSON_NAME, D),
    (tags::CONTENT_SEQUENCE, X),
    (tags::GRAPHIC_ANNOTATION_SEQUENCE, D),
    (tags::CONTENT_CREATOR_NAME, Z),
    (tags::CONTENT_CREATOR_IDENTIFICATION_CODE_SEQUENCE, X),
    (tags::TOPIC_SUBJECT, X),
    (tags::TOPIC_AUTHOR, X),
    (tags::TOPIC_KEYWORDS, X),
    (tags::STRUCTURE_SET_NAME, X),
    (tags::TEXT_COMMENTS, X),
    (tags::RESULTS_ID, X),
    (tags::RESULTS_ID_ISSUER, X),
    (tags::INTERPRETATION_RECORDER, X),
    (tags::INTERPRETATION_TRANSCRIBER, X),
    (tags::INTERPRETATION_TEXT, X),
    (tags::INTERPRETATION_AUTHOR, X),
    (tags::INTERPRETATION_APPROVER_SEQUENCE, X),
    (tags::PHYSICIAN_APPROVING_INTERPRETATION, X),
    (tags::INTERPRETATION_DIAGNOSIS_DESCRIPTION, X),
    (tags::RESULTS_DISTRIBUTION_LIST_SEQUENCE, X),
    (tags::DISTRIBUTION_NAME, X),
    (tags::DISTRIBUTION_ADDRESS, X),
    (tags::INTERPRETATION_ID_ISSUER, X),
    (tags::IMPRESSIONS, X),
    (tags::RESULTS_COMMENTS, X),
    // Signatures and audit
    (tags::DIGITAL_SIGNATURES_SEQUENCE, X),
    (tags::MAC, X),
    (tags::REFERENCED_DIGITAL_SIGNATURE_SEQUENCE, X),
    (tags::REFERENCED_SOP_INSTANCE_MAC_SEQUENCE, X),
    (tags::ORIGINAL_ATTRIBUTES_SEQUENCE, X),
    (tags::MODIFIED_ATTRIBUTES_SEQUENCE, X),
    (tags::DATA_SET_TRAILING_PADDING, X),
];

/// UID attributes that name classes, syntaxes or schemes rather than
/// instances; they are never replaced.
const CLASS_UIDS: &[Tag] = &[
    tags::SOP_CLASS_UID,
    tags::REFERENCED_SOP_CLASS_UID,
    tags::AFFECTED_SOP_CLASS_UID,
    tags::REQUESTED_SOP_CLASS_UID,
    tags::MEDIA_STORAGE_SOP_CLASS_UID,
    tags::TRANSFER_SYNTAX_UID,
    tags::IMPLEMENTATION_CLASS_UID,
    tags::CODING_SCHEME_UID,
    tags::MAPPING_RESOURCE_UID,
    tags::CONTEXT_UID,
    tags::SOP_CLASSES_IN_STUDY,
    tags::RELATED_GENERAL_SOP_CLASS_UID,
    tags::ORIGINAL_SPECIALIZED_SOP_CLASS_UID,
    tags::REFERENCED_SOP_CLASS_UID_IN_FILE,
    tags::REFERENCED_TRANSFER_SYNTAX_UID_IN_FILE,
];

/// A deterministic replacement UID: `2.25.` and a version 8 UUID made of
/// the first 128 bits of HMAC-SHA-256(secret, `uid:` + original).
pub(crate) fn replacement_uid(key: &hmac::Key, uid: &str) -> String {
    let digest = hmac::sign(key, format!("uid:{uid}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_ref()[..16]);
    bytes[6] = (bytes[6] & 0x0F) | 0x80;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    format!("2.25.{}", u128::from_be_bytes(bytes))
}

fn digest_u64(key: &hmac::Key, purpose: &str, value: &str) -> u64 {
    let digest = hmac::sign(key, format!("{purpose}:{value}").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest.as_ref()[..8]);
    u64::from_be_bytes(bytes)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Shifts a `YYYYMMDD` date by `days`; `None` if the text is not a valid
/// full date or the result leaves years 1..=9999.
pub(crate) fn shift_date(text: &str, days: i64) -> Option<String> {
    if text.len() != 8 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i64 = text[..4].parse().ok()?;
    let month: u32 = text[4..6].parse().ok()?;
    let day: u32 = text[6..8].parse().ok()?;
    if !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let count = days_from_civil(year, month, day);
    // Reject days beyond the end of the month.
    if civil_from_days(count) != (year, month, day) {
        return None;
    }
    let (year, month, day) = civil_from_days(count.checked_add(days)?);
    (1..=9999)
        .contains(&year)
        .then(|| format!("{year:04}{month:02}{day:02}"))
}

/// Shifts the date of a DT value, keeping its time and offset.
fn shift_date_time(text: &str, days: i64) -> Option<String> {
    let date = shift_date(text.get(..8)?, days)?;
    Some(format!("{date}{}", &text[8..]))
}

fn dummy(vr: VR) -> PrimitiveValue {
    let text = |value: &str| PrimitiveValue::from(value);
    match vr {
        VR::DA => text("19000101"),
        VR::TM => text("000000"),
        VR::DT => text("19000101000000"),
        VR::AS => text("000D"),
        VR::IS | VR::DS => text("0"),
        VR::AE | VR::CS | VR::LO | VR::LT | VR::PN | VR::SH | VR::ST | VR::UC | VR::UT => {
            text("ANONYMIZED")
        }
        _ => PrimitiveValue::Empty,
    }
}

/// What to do with one element.
enum Change {
    Remove,
    Replace(VR, PrimitiveValue),
}

/// De-identifies DICOM objects.
pub struct Deidentify {
    key: hmac::Key,
    date_shift: DateShift,
    pseudonymize_patient: bool,
    pseudonym_prefix: String,
    keep: BTreeSet<Tag>,
    remove_private: bool,
    allow_burned_in_annotation: bool,
}

impl std::fmt::Debug for Deidentify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deidentify")
            .field("date_shift", &self.date_shift)
            .field("keep", &self.keep)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy)]
enum DateShift {
    None,
    Fixed(i64),
    PerPatient(u32),
}

impl Deidentify {
    /// Validates the settings and creates the transformer.
    pub fn new(settings: DeidentifySettings) -> Result<Self, EngineError> {
        let config =
            |message: String| EngineError::Config(format!("dicom-deidentify step: {message}"));
        let secret = match (settings.secret, settings.secret_env) {
            (Some(_), Some(_)) => return Err(config("set secret or secret_env, not both".into())),
            (Some(secret), None) => secret,
            (None, Some(variable)) => std::env::var(&variable)
                .map_err(|_| config(format!("environment variable {variable} is not set")))?,
            (None, None) => return Err(config("a secret or secret_env is required".into())),
        };
        if secret.len() < 16 {
            return Err(config("the secret must have at least 16 characters".into()));
        }
        let date_shift = match (settings.date_shift_days, settings.date_shift_max_days) {
            (Some(_), Some(_)) => {
                return Err(config(
                    "set date_shift_days or date_shift_max_days, not both".into(),
                ));
            }
            (Some(days), None) => DateShift::Fixed(i64::from(days)),
            (None, Some(max)) => DateShift::PerPatient(max),
            (None, None) => DateShift::None,
        };
        let prefix = settings.pseudonym_prefix.trim().to_owned();
        if prefix.len() > 32
            || !prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(config(
                "pseudonym_prefix must be at most 32 letters, digits, - or _".into(),
            ));
        }
        let keep = settings
            .keep
            .iter()
            .map(|name| {
                let selector = parse_selector(name).map_err(|e| config(e.to_string()))?;
                match selector.iter().collect::<Vec<_>>().as_slice() {
                    [dicom_core::ops::AttributeSelectorStep::Tag(tag)] => Ok(*tag),
                    _ => Err(config(format!("keep takes plain attributes, not {name:?}"))),
                }
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes()),
            date_shift,
            pseudonymize_patient: settings.pseudonymize_patient,
            pseudonym_prefix: prefix,
            keep,
            remove_private: settings.remove_private,
            allow_burned_in_annotation: settings.allow_burned_in_annotation,
        })
    }

    fn text(dataset: &InMemDicomObject, tag: Tag) -> Option<String> {
        dataset
            .get(tag)
            .and_then(|element| value_text(element.value()))
    }

    /// De-identifies a decoded object.
    pub fn apply_to(&self, object: &mut DicomObject) -> Result<(), String> {
        let dataset = &mut object.dataset;
        if !self.allow_burned_in_annotation
            && Self::text(dataset, tags::BURNED_IN_ANNOTATION).as_deref() == Some("YES")
        {
            return Err(
                "the object has burned-in annotation; its pixel data may identify the patient"
                    .into(),
            );
        }
        let patient_id = Self::text(dataset, tags::PATIENT_ID)
            .filter(|id| !id.is_empty())
            .or_else(|| Self::text(dataset, tags::PATIENT_NAME))
            .unwrap_or_default();
        let shift = match self.date_shift {
            DateShift::None => None,
            DateShift::Fixed(days) => Some(days),
            DateShift::PerPatient(0) => Some(0),
            DateShift::PerPatient(max) => {
                let span = u64::from(max) * 2 + 1;
                let offset = digest_u64(&self.key, "date-shift", &patient_id) % span;
                Some(i64::try_from(offset).unwrap_or_default() - i64::from(max))
            }
        };
        if self.remove_private {
            remove_private(dataset);
        }
        for_each_dataset(dataset, &mut |item| self.clean(item, shift));
        // Pseudonym and de-identification markers at the top level.
        let pseudonym = format!(
            "{}-{:016X}",
            self.pseudonym_prefix,
            digest_u64(&self.key, "patient", &patient_id)
        );
        for tag in [tags::PATIENT_ID, tags::PATIENT_NAME] {
            if self.keep.contains(&tag) {
                continue;
            }
            let vr = if tag == tags::PATIENT_ID {
                VR::LO
            } else {
                VR::PN
            };
            let value = if self.pseudonymize_patient {
                PrimitiveValue::from(pseudonym.as_str())
            } else {
                PrimitiveValue::Empty
            };
            dataset.put(DataElement::new(tag, vr, value));
        }
        let mut methods = vec!["PS3.15 E.1 Basic Profile".to_owned()];
        let mut codes = vec![("113100", "Basic Application Confidentiality Profile")];
        if shift.is_some() {
            methods.push("Retain Longitudinal Modified Dates".to_owned());
            codes.push((
                "113107",
                "Retain Longitudinal Temporal Information Modified Dates Option",
            ));
        }
        if self.pseudonymize_patient {
            methods.push("Patient pseudonymized by OXIM".to_owned());
        }
        dataset.put(DataElement::new(
            tags::PATIENT_IDENTITY_REMOVED,
            VR::CS,
            PrimitiveValue::from("YES"),
        ));
        dataset.put(DataElement::new(
            tags::DEIDENTIFICATION_METHOD,
            VR::LO,
            PrimitiveValue::Strs(methods.into_iter().collect::<C<String>>()),
        ));
        let items: Vec<InMemDicomObject> = codes
            .into_iter()
            .map(|(value, meaning)| {
                InMemDicomObject::from_element_iter([
                    DataElement::new(tags::CODE_VALUE, VR::SH, PrimitiveValue::from(value)),
                    DataElement::new(
                        tags::CODING_SCHEME_DESIGNATOR,
                        VR::SH,
                        PrimitiveValue::from("DCM"),
                    ),
                    DataElement::new(tags::CODE_MEANING, VR::LO, PrimitiveValue::from(meaning)),
                ])
            })
            .collect();
        dataset.put(DataElement::new(
            tags::DEIDENTIFICATION_METHOD_CODE_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(items),
        ));
        dataset.put(DataElement::new(
            tags::LONGITUDINAL_TEMPORAL_INFORMATION_MODIFIED,
            VR::CS,
            PrimitiveValue::from(if shift.is_some() {
                "MODIFIED"
            } else {
                "REMOVED"
            }),
        ));
        object.meta.source_ae_title = None;
        object.meta.sending_ae_title = None;
        object.meta.receiving_ae_title = None;
        Ok(())
    }

    /// Applies the profile to the elements of one data set (not its
    /// nested items).
    fn clean(&self, dataset: &mut InMemDicomObject, shift: Option<i64>) {
        let mut changes = Vec::new();
        for element in dataset.iter() {
            let tag = element.tag();
            let vr = element.vr();
            if self.keep.contains(&tag) {
                continue;
            }
            let curve = tag.group() & 0xFF00 == 0x5000;
            let overlay = tag.group() & 0xFF00 == 0x6000
                && tag.group().is_multiple_of(2)
                && (tag.element() == 0x3000 || tag.element() == 0x4000);
            if curve || overlay {
                changes.push((tag, Change::Remove));
                continue;
            }
            if vr == VR::UI {
                if !CLASS_UIDS.contains(&tag)
                    && let Some(text) = value_text(element.value())
                {
                    let replaced: C<String> = text
                        .split('\\')
                        .map(|uid| {
                            if uid.is_empty() || uid.starts_with("1.2.840.10008.") {
                                uid.to_owned()
                            } else {
                                replacement_uid(&self.key, uid)
                            }
                        })
                        .collect();
                    changes.push((tag, Change::Replace(VR::UI, PrimitiveValue::Strs(replaced))));
                }
                continue;
            }
            if shift.is_some() && vr == VR::TM {
                // Times are kept with the modified dates option.
                continue;
            }
            if let (Some(days), VR::DA | VR::DT) = (shift, vr) {
                let text = value_text(element.value()).unwrap_or_default();
                let shifted: Option<C<String>> = text
                    .split('\\')
                    .map(|value| {
                        if value.is_empty() {
                            Some(String::new())
                        } else if vr == VR::DA {
                            shift_date(value, days)
                        } else {
                            shift_date_time(value, days)
                        }
                    })
                    .collect();
                let value = match shifted {
                    Some(values) if !text.is_empty() => PrimitiveValue::Strs(values),
                    _ => PrimitiveValue::Empty,
                };
                changes.push((tag, Change::Replace(vr, value)));
                continue;
            }
            let Some((_, action)) = PROFILE.iter().find(|(known, _)| *known == tag) else {
                continue;
            };
            let change = match (action, vr) {
                (Action::Remove, _) => Change::Remove,
                (Action::Empty | Action::Dummy, VR::SQ) => {
                    Change::Replace(VR::SQ, PrimitiveValue::Empty)
                }
                (Action::Empty, _) => Change::Replace(vr, PrimitiveValue::Empty),
                (Action::Dummy, _) => Change::Replace(vr, dummy(vr)),
            };
            changes.push((tag, change));
        }
        for (tag, change) in changes {
            match change {
                Change::Remove => {
                    dataset.remove_element(tag);
                }
                Change::Replace(VR::SQ, _) => {
                    dataset.put(DataElement::new(
                        tag,
                        VR::SQ,
                        DataSetSequence::<InMemDicomObject>::empty(),
                    ));
                }
                Change::Replace(vr, value) => {
                    dataset.put(DataElement::new(tag, vr, value));
                }
            }
        }
    }
}

impl Transformer for Deidentify {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let mut object = decode(context, "dicom-deidentify")?;
        self.apply_to(&mut object)
            .map_err(|e| StepError::new("dicom-deidentify", e))?;
        encode(context, &object, "dicom-deidentify")
    }
}

/// Registers the `dicom-deidentify` transformer.
pub(crate) fn register(registry: &mut oxim_core::Registry) {
    registry.add_transformer("dicom-deidentify", |step: &StepConfig| {
        let settings = settings(&step.settings, "dicom-deidentify step")?;
        Ok(Arc::new(Deidentify::new(settings)?) as Arc<dyn Transformer>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_dates() {
        assert_eq!(shift_date("20240229", 1).as_deref(), Some("20240301"));
        assert_eq!(shift_date("20240301", -1).as_deref(), Some("20240229"));
        assert_eq!(shift_date("20231231", 1).as_deref(), Some("20240101"));
        assert_eq!(shift_date("19700101", 0).as_deref(), Some("19700101"));
        assert_eq!(shift_date("20000101", -36_524).as_deref(), Some("19000101"));
        for bad in ["2024", "20240230", "20241301", "2024010A", "00010101"] {
            assert_eq!(shift_date(bad, -1), None, "{bad}");
        }
        assert_eq!(
            shift_date_time("20240229235959.5+0100", 1).as_deref(),
            Some("20240301235959.5+0100")
        );
        for days in [-800_000i64, -1, 0, 1, 59, 400_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn replaces_uids_deterministically() {
        let key = hmac::Key::new(hmac::HMAC_SHA256, b"0123456789abcdef");
        let other = hmac::Key::new(hmac::HMAC_SHA256, b"fedcba9876543210");
        let first = replacement_uid(&key, "1.2.3.4");
        assert_eq!(first, replacement_uid(&key, "1.2.3.4"));
        assert_ne!(first, replacement_uid(&key, "1.2.3.5"));
        assert_ne!(first, replacement_uid(&other, "1.2.3.4"));
        assert!(first.starts_with("2.25."));
        assert!(crate::uids::is_valid_uid(&first), "{first}");
    }

    #[test]
    fn validates_settings() {
        let base = || DeidentifySettings {
            secret: Some("a long enough secret".into()),
            secret_env: None,
            date_shift_days: None,
            date_shift_max_days: None,
            pseudonymize_patient: true,
            pseudonym_prefix: default_pseudonym_prefix(),
            keep: vec!["PatientSex".into()],
            remove_private: true,
            allow_burned_in_annotation: false,
        };
        assert!(Deidentify::new(base()).is_ok());
        let mut short = base();
        short.secret = Some("short".into());
        assert!(Deidentify::new(short).is_err());
        let mut none = base();
        none.secret = None;
        assert!(Deidentify::new(none).is_err());
        let mut both = base();
        both.date_shift_days = Some(3);
        both.date_shift_max_days = Some(3);
        assert!(Deidentify::new(both).is_err());
        let mut nested = base();
        nested.keep = vec!["RequestAttributesSequence[0].AccessionNumber".into()];
        assert!(Deidentify::new(nested).is_err());
        let mut prefix = base();
        prefix.pseudonym_prefix = "has space".into();
        assert!(Deidentify::new(prefix).is_err());
    }
}
