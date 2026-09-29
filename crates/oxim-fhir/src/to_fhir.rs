//! The normalized model → FHIR.
//!
//! # Results and quality control
//!
//! [`encode_bundle`] writes one Bundle (`transaction` by default, or
//! `collection`) per message:
//!
//! | Model | FHIR |
//! |---|---|
//! | `ResultGroup.patient` | `Patient`: identifiers (type from v2 table 0203), first name, `gender`, `birthDate` (date part), species/breed in the `patient-animal` extension. The first identifier gets `patient_identifier_system` when it has no system. Conditional create on the first identifier with a system; without one, each message creates a Patient, since a bare value could match another patient. Groups with the same patient share one entry. |
//! | `ResultGroup.specimen` | `Specimen`: identifiers, `type`, `collection.collectedDateTime`, `receivedTime`, container description, notes. System and conditional create as for patients (`specimen_identifier_system`). |
//! | `ResultGroup.order` | One `ServiceRequest` per requested test (one without `code` when no test is listed): OXIM identifier, placer (`PLAC`) and filler (`FILL`) identifiers, `status` (`completed` when every result is final, otherwise `active`), `intent: order`, `priority`, `authoredOn`, specimens, notes. |
//! | `Observation` | `Observation` with an OXIM identifier, `basedOn` the requests for its test (all of the group's requests when no test code matches), `status` (same codes), `category` (`observation_category`, default `laboratory`), `code`, `subject`, `effectiveDateTime`, `issued`, `performer` (operator as display), value (see below), `interpretation` (codes without a system get the v3 ObservationInterpretation system, which shares HL7 table 0078 codes), `note`, `method`, `specimen`, `device`, `referenceRange` (limits carry the value's unit). |
//! | Observations of a group | One `DiagnosticReport` (`category` LAB, `code` from the order's first test or LOINC 11502-2 *Laboratory report*, `status` `final`/`corrected`/`preliminary` from the results, `result` references, `presentedForm` for attachment results). |
//! | `Results.device` | `Device` (identifiers, names, model, serial number, version), referenced by every observation; conditional create on the first identifier with a system, the serial number (`urn:oxim:device-serial`) otherwise. A device with neither is left out, since each message would create another Device. |
//! | `QualityControl` | QC observations with an extra `urn:oxim:observation-category#quality-control` category and material, lot, level and expiry in a note; no subject. |
//!
//! | Observation value | FHIR `value[x]` |
//! |---|---|
//! | `Quantity` | `valueQuantity`: exact decimal, comparator, `unit`; `system`/`code` are kept when set, otherwise UCUM is claimed only for units that look like UCUM codes |
//! | `Text` | `valueString` |
//! | `Coded` | `valueCodeableConcept` |
//! | `Range`, `Ratio` | `valueRange`, `valueRatio` |
//! | `Boolean` | `valueBoolean` |
//! | `DateTime` | `valueDateTime` |
//! | `Attachment` | the DiagnosticReport's `presentedForm`; the observation gets a note pointing to it |
//!
//! # Orders
//!
//! `Orders` become Patient, Specimen and one ServiceRequest per test
//! (`status` `revoked` for cancellations, otherwise `active`).
//!
//! # Identity and retries
//!
//! Every `fullUrl` is a `urn:uuid:` derived from the OXIM message id, and
//! requests, observations and reports carry OXIM identifiers
//! (`urn:oxim:service-request`, `urn:oxim:observation`,
//! `urn:oxim:diagnostic-report`) used for conditional creates
//! (`ifNoneExist`), so a retried transaction does not create duplicates.
//!
//! # Times
//!
//! FHIR requires a time zone and seconds whenever a time is given. Times
//! recorded without an offset get the configured `utc_offset`, and times
//! without seconds get `:00`; dates stay dates.

use oxim_model::{
    AdministrativeSex, ClinicalContent, ClinicalDateTime, MessageId, ObservationStatus,
    ObservationValue, OrderControl, Precision, Priority, ResultGroup, Timestamp,
};
use serde_json::json;

use crate::datatypes::{
    Annotation, Attachment, CodeableConcept, Coding, HumanName, Identifier, Quantity, Range, Ratio,
    Reference,
};
use crate::decimal::FhirDecimal;
use crate::error::{FhirError, FhirResult};
use crate::ids::{entry_url, token};
use crate::resources::{
    Bundle, BundleEntry, BundleRequest, Device, DeviceName, DeviceVersion, DiagnosticReport,
    Observation, ObservationReferenceRange, Patient, Resource, ServiceRequest, Specimen,
    SpecimenCollection, SpecimenContainer,
};

/// Observation categories.
pub const OBSERVATION_CATEGORY: &str = "http://terminology.hl7.org/CodeSystem/observation-category";
/// v3 ObservationInterpretation (abnormal flags).
pub const V3_INTERPRETATION: &str =
    "http://terminology.hl7.org/CodeSystem/v3-ObservationInterpretation";
/// HL7 v2 table 0078 (abnormal flags).
pub const V2_ABNORMAL_FLAGS: &str = "http://terminology.hl7.org/CodeSystem/v2-0078";
/// HL7 v2 table 0203 (identifier types).
pub const V2_IDENTIFIER_TYPE: &str = "http://terminology.hl7.org/CodeSystem/v2-0203";
/// HL7 v2 table 0074 (diagnostic service sections).
pub const V2_SERVICE_SECTION: &str = "http://terminology.hl7.org/CodeSystem/v2-0074";
/// UCUM units.
pub const UCUM: &str = "http://unitsofmeasure.org";
/// LOINC.
pub const LOINC: &str = "http://loinc.org";
/// OXIM categories of observations.
pub const OXIM_CATEGORY: &str = "urn:oxim:observation-category";
/// OXIM identifiers of observations.
pub const OXIM_OBSERVATION: &str = "urn:oxim:observation";
/// OXIM identifiers of service requests.
pub const OXIM_SERVICE_REQUEST: &str = "urn:oxim:service-request";
/// OXIM identifiers of diagnostic reports.
pub const OXIM_DIAGNOSTIC_REPORT: &str = "urn:oxim:diagnostic-report";
/// OXIM identifiers of device serial numbers.
pub const OXIM_DEVICE_SERIAL: &str = "urn:oxim:device-serial";
/// The `patient-animal` extension.
pub const PATIENT_ANIMAL: &str = "http://hl7.org/fhir/StructureDefinition/patient-animal";
/// The code of the quality control category in [`OXIM_CATEGORY`].
pub const QUALITY_CONTROL: &str = "quality-control";
/// The note on an observation whose attachment value is in the
/// DiagnosticReport's `presentedForm`.
pub const ATTACHMENT_NOTE: &str =
    "The result is an attachment in the DiagnosticReport's presentedForm.";
/// The prefix of the note carrying QC material details.
pub const QC_NOTE_PREFIX: &str = "Quality control material: ";

/// The bundle type written by [`encode_bundle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BundleType {
    /// A transaction with a request per entry.
    #[default]
    Transaction,
    /// A collection without requests.
    Collection,
}

/// Settings of the FHIR encoders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FhirEncoding {
    /// The bundle type.
    pub bundle_type: BundleType,
    /// System for the first patient identifier when it has none, for
    /// example the OID of the hospital's medical record numbers. Patients
    /// are created conditionally only on identifiers with a system.
    pub patient_identifier_system: Option<String>,
    /// System for the first specimen identifier when it has none.
    pub specimen_identifier_system: Option<String>,
    /// Code in the observation-category system, `laboratory` by default.
    pub observation_category: String,
    /// Offset in minutes for times recorded without one.
    pub utc_offset_minutes: i16,
}

impl Default for FhirEncoding {
    fn default() -> Self {
        Self {
            bundle_type: BundleType::Transaction,
            patient_identifier_system: None,
            specimen_identifier_system: None,
            observation_category: "laboratory".into(),
            utc_offset_minutes: 0,
        }
    }
}

/// A FHIR `date` from a clinical date or date-time.
pub fn fhir_date(value: &ClinicalDateTime) -> String {
    let iso = value.to_iso();
    iso.split('T').next().unwrap_or(&iso).to_owned()
}

fn offset_text(minutes: i16) -> String {
    if minutes == 0 {
        return "Z".into();
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let minutes = minutes.unsigned_abs();
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A FHIR `dateTime`: dates stay dates; times get seconds and an offset
/// (`default_offset` minutes when none was recorded).
pub fn fhir_datetime(value: &ClinicalDateTime, default_offset: i16) -> String {
    if matches!(
        value.precision(),
        Precision::Year | Precision::Month | Precision::Day
    ) {
        return fhir_date(value);
    }
    let iso = value.to_iso();
    let (date, time) = iso.split_once('T').unwrap_or((&iso, ""));
    let split = time.find(['Z', '+', '-']).unwrap_or(time.len());
    let (clock, offset) = time.split_at(split);
    let clock = if clock.len() == 5 {
        format!("{clock}:00")
    } else {
        clock.to_owned()
    };
    let offset = if offset.is_empty() {
        offset_text(default_offset)
    } else {
        offset.to_owned()
    };
    format!("{date}T{clock}{offset}")
}

/// A FHIR `instant` for a system timestamp.
pub fn fhir_instant(timestamp: Timestamp, offset: i16) -> String {
    match ClinicalDateTime::from_timestamp(timestamp, offset) {
        Some(value) => fhir_datetime(&value, offset),
        None => timestamp.to_string(),
    }
}

fn concept(concept: &oxim_model::CodeableConcept) -> CodeableConcept {
    CodeableConcept {
        coding: concept
            .codings
            .iter()
            .map(|c| Coding::new(c.system.as_deref(), &c.code).with_display(c.display.as_deref()))
            .collect(),
        text: concept.text.clone(),
        ..CodeableConcept::default()
    }
}

/// Whether a unit looks like a UCUM code: printable ASCII without spaces
/// and made of UCUM's symbol characters.
pub fn looks_like_ucum(unit: &str) -> bool {
    !unit.is_empty()
        && unit.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'.' | b'/'
                        | b'*'
                        | b'%'
                        | b'['
                        | b']'
                        | b'{'
                        | b'}'
                        | b'('
                        | b')'
                        | b'^'
                        | b'\''
                        | b'-'
                        | b'_'
                )
        })
}

fn quantity(q: &oxim_model::Quantity) -> FhirResult<Quantity> {
    let (system, code) = match (&q.system, &q.code) {
        (None, None) => match q.unit.as_deref().filter(|u| looks_like_ucum(u)) {
            Some(unit) => (Some(UCUM.to_owned()), Some(unit.to_owned())),
            None => (None, None),
        },
        (system, code) => (system.clone(), code.clone()),
    };
    Ok(Quantity {
        value: Some(FhirDecimal::from_decimal(&q.value)?),
        comparator: q.comparator.map(|c| c.as_str().to_owned()),
        unit: q.unit.clone(),
        system,
        code,
        ..Quantity::default()
    })
}

/// A quantity carrying `value` and the unit of `like`.
fn limit(value: &oxim_model::Decimal, like: Option<&Quantity>) -> FhirResult<Quantity> {
    Ok(Quantity {
        value: Some(FhirDecimal::from_decimal(value)?),
        unit: like.and_then(|q| q.unit.clone()),
        system: like.and_then(|q| q.system.clone()),
        code: like.and_then(|q| q.code.clone()),
        ..Quantity::default()
    })
}

fn status_code(status: ObservationStatus) -> &'static str {
    match status {
        ObservationStatus::Registered => "registered",
        ObservationStatus::Preliminary => "preliminary",
        ObservationStatus::Final => "final",
        ObservationStatus::Amended => "amended",
        ObservationStatus::Corrected => "corrected",
        ObservationStatus::Cancelled => "cancelled",
        ObservationStatus::EnteredInError => "entered-in-error",
        ObservationStatus::Unknown => "unknown",
    }
}

fn priority_code(priority: Priority) -> &'static str {
    match priority {
        Priority::Routine => "routine",
        Priority::Urgent => "urgent",
        Priority::Asap => "asap",
        Priority::Stat => "stat",
    }
}

fn gender_code(sex: AdministrativeSex) -> &'static str {
    match sex {
        AdministrativeSex::Male => "male",
        AdministrativeSex::Female => "female",
        AdministrativeSex::Other => "other",
        AdministrativeSex::Unknown => "unknown",
    }
}

fn identifier_type(code: &str) -> CodeableConcept {
    CodeableConcept::coding(Coding::new(Some(V2_IDENTIFIER_TYPE), code))
}

fn oxim_identifier(system: &str, value: String) -> Identifier {
    Identifier {
        system: Some(system.to_owned()),
        value: Some(value),
        ..Identifier::default()
    }
}

/// The `ifNoneExist` search on the first identifier with a system. A
/// value without a system could match another patient's or specimen's
/// identifier, so such identifiers are never searched.
fn conditional_on(identifiers: &[Identifier]) -> Option<String> {
    identifiers.iter().find_map(|id| {
        let system = id.system.as_deref().filter(|s| !s.is_empty())?;
        let value = id.value.as_deref().filter(|v| !v.is_empty())?;
        Some(format!("identifier={}", token(Some(system), value)))
    })
}

/// The requests an observation answers: those whose test shares a coding
/// with the observation's code, or every request of the order when none
/// does.
fn requests_for(
    order: Option<&oxim_model::Order>,
    requests: &[String],
    observation: &oxim_model::Observation,
) -> Vec<String> {
    let matching: Vec<String> = order
        .map(|order| {
            order
                .tests
                .iter()
                .zip(requests)
                .filter(|(test, _)| {
                    test.codings
                        .iter()
                        .any(|coding| observation.code.codings.contains(coding))
                })
                .map(|(_, url)| url.clone())
                .collect()
        })
        .unwrap_or_default();
    if matching.is_empty() {
        requests.to_vec()
    } else {
        matching
    }
}

fn is_final(status: ObservationStatus) -> bool {
    matches!(
        status,
        ObservationStatus::Final | ObservationStatus::Amended | ObservationStatus::Corrected
    )
}

struct Builder<'a> {
    settings: &'a FhirEncoding,
    message: MessageId,
    received_at: Timestamp,
    entries: Vec<BundleEntry>,
    /// Patients already added: identity key and full URL.
    patients: Vec<(String, String)>,
    device: Option<String>,
}

impl Builder<'_> {
    fn push(&mut self, key: &str, resource: Resource, if_none_exist: Option<String>) -> String {
        let full_url = entry_url(self.message, key);
        let request = match self.settings.bundle_type {
            BundleType::Transaction => Some(BundleRequest {
                method: "POST".into(),
                url: resource.resource_type().to_owned(),
                if_none_exist,
                ..BundleRequest::default()
            }),
            BundleType::Collection => None,
        };
        self.entries.push(BundleEntry {
            full_url: Some(full_url.clone()),
            resource: Some(resource),
            request,
            ..BundleEntry::default()
        });
        full_url
    }

    fn time(&self, value: &ClinicalDateTime) -> String {
        fhir_datetime(value, self.settings.utc_offset_minutes)
    }

    /// Adds the device, when it has an identifier or serial number to
    /// create it conditionally; otherwise every message would create
    /// another Device.
    fn device(&mut self, device: &oxim_model::Device) -> Option<String> {
        let mut identifier: Vec<Identifier> = device
            .identifiers
            .iter()
            .map(|id| Identifier {
                system: id.system.clone(),
                value: Some(id.value.clone()),
                kind: id.kind.as_deref().map(identifier_type),
                ..Identifier::default()
            })
            .collect();
        if let Some(serial) = &device.serial_number {
            identifier.push(oxim_identifier(OXIM_DEVICE_SERIAL, serial.clone()));
        }
        let mut names = Vec::new();
        if let Some(name) = &device.name {
            names.push(DeviceName {
                name: name.clone(),
                kind: "user-friendly-name".into(),
                ..DeviceName::default()
            });
        }
        if let Some(model) = &device.model {
            names.push(DeviceName {
                name: model.clone(),
                kind: "model-name".into(),
                ..DeviceName::default()
            });
        }
        let conditional = Some(conditional_on(&identifier)?);
        let resource = Device {
            identifier,
            manufacturer: device.manufacturer.clone(),
            serial_number: device.serial_number.clone(),
            device_name: names,
            version: device
                .software_version
                .iter()
                .map(|v| DeviceVersion {
                    value: v.clone(),
                    ..DeviceVersion::default()
                })
                .collect(),
            ..Device::default()
        };
        Some(self.push("device", resource.into(), conditional))
    }

    fn patient(&mut self, group: usize, patient: &oxim_model::Patient) -> String {
        let identity = format!("{:?}{:?}", patient.identifiers, patient.name);
        if let Some((_, url)) = self.patients.iter().find(|(key, _)| *key == identity) {
            return url.clone();
        }
        let identifier: Vec<Identifier> = patient
            .identifiers
            .iter()
            .enumerate()
            .map(|(index, id)| Identifier {
                system: id.system.clone().or_else(|| {
                    (index == 0)
                        .then(|| self.settings.patient_identifier_system.clone())
                        .flatten()
                }),
                value: Some(id.value.clone()),
                kind: id.kind.as_deref().map(identifier_type),
                assigner: id
                    .assigner
                    .as_ref()
                    .map(|a| Box::new(Reference::display(a.clone()))),
                ..Identifier::default()
            })
            .collect();
        let conditional = conditional_on(&identifier);
        let mut resource = Patient {
            identifier,
            name: patient
                .name
                .iter()
                .map(|n| HumanName {
                    family: n.family.clone(),
                    given: n.given.clone(),
                    prefix: n.prefix.iter().cloned().collect(),
                    suffix: n.suffix.iter().cloned().collect(),
                    ..HumanName::default()
                })
                .collect(),
            gender: patient.sex.map(|s| gender_code(s).to_owned()),
            birth_date: patient.birth_date.as_ref().map(fhir_date),
            ..Patient::default()
        };
        if patient.species.is_some() || patient.breed.is_some() {
            let mut parts = Vec::new();
            for (url, value) in [("species", &patient.species), ("breed", &patient.breed)] {
                if let Some(value) = value {
                    parts.push(json!({"url": url, "valueCodeableConcept": concept(value)}));
                }
            }
            resource.extra.insert(
                "extension".into(),
                json!([{ "url": PATIENT_ANIMAL, "extension": parts }]),
            );
        }
        let url = self.push(&format!("patient-{group}"), resource.into(), conditional);
        self.patients.push((identity, url.clone()));
        url
    }

    fn specimen(
        &mut self,
        group: usize,
        specimen: &oxim_model::Specimen,
        subject: Option<&str>,
    ) -> String {
        let identifier: Vec<Identifier> = specimen
            .identifiers
            .iter()
            .enumerate()
            .map(|(index, id)| Identifier {
                system: id.system.clone().or_else(|| {
                    (index == 0)
                        .then(|| self.settings.specimen_identifier_system.clone())
                        .flatten()
                }),
                value: Some(id.value.clone()),
                kind: id.kind.as_deref().map(identifier_type),
                ..Identifier::default()
            })
            .collect();
        let conditional = conditional_on(&identifier);
        let resource = Specimen {
            identifier,
            kind: specimen.kind.as_ref().map(concept),
            subject: subject.map(Reference::to),
            received_time: specimen.received_at.as_ref().map(|t| self.time(t)),
            collection: specimen.collected_at.as_ref().map(|t| SpecimenCollection {
                collected_date_time: Some(self.time(t)),
                ..SpecimenCollection::default()
            }),
            container: specimen
                .container
                .iter()
                .map(|c| SpecimenContainer {
                    description: Some(c.clone()),
                    ..SpecimenContainer::default()
                })
                .collect(),
            note: specimen.notes.iter().map(Annotation::text).collect(),
            ..Specimen::default()
        };
        self.push(&format!("specimen-{group}"), resource.into(), conditional)
    }

    fn requests(
        &mut self,
        group: usize,
        order: &oxim_model::Order,
        status: &str,
        subject: Option<&str>,
        specimen: Option<(&str, &[oxim_model::Identifier])>,
    ) -> Vec<String> {
        let tests: Vec<Option<&oxim_model::CodeableConcept>> = if order.tests.is_empty() {
            vec![None]
        } else {
            order.tests.iter().map(Some).collect()
        };
        let specimens: Vec<Reference> = order
            .specimen_ids
            .iter()
            .map(|id| {
                let reference = specimen
                    .filter(|(_, ids)| ids.iter().any(|i| i.value == *id))
                    .map(|(url, _)| url.to_owned());
                Reference {
                    reference,
                    display: Some(id.clone()),
                    ..Reference::default()
                }
            })
            .chain(
                specimen
                    .filter(|_| order.specimen_ids.is_empty())
                    .map(|(url, _)| Reference::to(url)),
            )
            .collect();
        let mut urls = Vec::new();
        for (index, test) in tests.into_iter().enumerate() {
            let own = format!("{}-{group}-{index}", self.message);
            let mut identifier = vec![oxim_identifier(OXIM_SERVICE_REQUEST, own.clone())];
            if let Some(placer) = &order.placer_id {
                identifier.push(Identifier {
                    kind: Some(identifier_type("PLAC")),
                    value: Some(placer.clone()),
                    ..Identifier::default()
                });
            }
            if let Some(filler) = &order.filler_id {
                identifier.push(Identifier {
                    kind: Some(identifier_type("FILL")),
                    value: Some(filler.clone()),
                    ..Identifier::default()
                });
            }
            let resource = ServiceRequest {
                identifier,
                status: status.to_owned(),
                intent: "order".into(),
                priority: order.priority.map(|p| priority_code(p).to_owned()),
                code: test.map(concept),
                subject: Some(match subject {
                    Some(url) => Reference::to(url),
                    None => Reference::display("unidentified subject"),
                }),
                authored_on: order.requested_at.as_ref().map(|t| self.time(t)),
                specimen: specimens.clone(),
                note: order.notes.iter().map(Annotation::text).collect(),
                ..ServiceRequest::default()
            };
            let conditional = Some(format!(
                "identifier={}",
                token(Some(OXIM_SERVICE_REQUEST), &own)
            ));
            urls.push(self.push(
                &format!("request-{group}-{index}"),
                resource.into(),
                conditional,
            ));
        }
        urls
    }

    #[allow(clippy::too_many_arguments)]
    fn observation(
        &mut self,
        key: &str,
        observation: &oxim_model::Observation,
        categories: Vec<CodeableConcept>,
        subject: Option<&str>,
        specimen: Option<&str>,
        based_on: &[String],
        mut notes: Vec<Annotation>,
        attachments: &mut Vec<Attachment>,
    ) -> FhirResult<String> {
        let own = format!("{}-{key}", self.message);
        let mut resource = Observation {
            identifier: vec![oxim_identifier(OXIM_OBSERVATION, own.clone())],
            based_on: based_on.iter().map(Reference::to).collect(),
            status: status_code(observation.status).to_owned(),
            category: categories,
            code: concept(&observation.code),
            subject: subject.map(Reference::to),
            effective_date_time: observation.effective_at.as_ref().map(|t| self.time(t)),
            issued: observation.issued_at.as_ref().map(|t| self.time(t)),
            performer: observation
                .operator
                .iter()
                .map(|o| Reference::display(o.clone()))
                .collect(),
            interpretation: observation
                .interpretation
                .iter()
                .map(|flag| {
                    let system = match flag.system.as_deref() {
                        None | Some(V2_ABNORMAL_FLAGS) => Some(V3_INTERPRETATION),
                        Some(other) => Some(other),
                    };
                    CodeableConcept::coding(
                        Coding::new(system, &flag.code).with_display(flag.display.as_deref()),
                    )
                })
                .collect(),
            method: observation.method.as_ref().map(concept),
            specimen: match (specimen, &observation.specimen_id) {
                (None, None) => None,
                (url, id) => Some(Reference {
                    reference: url.map(str::to_owned),
                    display: id.clone(),
                    ..Reference::default()
                }),
            },
            device: match (&self.device, &observation.device_id) {
                (None, None) => None,
                (url, id) => Some(Reference {
                    reference: url.clone(),
                    display: id.clone(),
                    ..Reference::default()
                }),
            },
            ..Observation::default()
        };
        match &observation.value {
            None => {}
            Some(ObservationValue::Quantity(q)) => resource.value_quantity = Some(quantity(q)?),
            Some(ObservationValue::Text(text)) => resource.value_string = Some(text.clone()),
            Some(ObservationValue::Coded(c)) => resource.value_codeable_concept = Some(concept(c)),
            Some(ObservationValue::Range { low, high }) => {
                resource.value_range = Some(Range {
                    low: low.as_ref().map(quantity).transpose()?,
                    high: high.as_ref().map(quantity).transpose()?,
                    ..Range::default()
                });
            }
            Some(ObservationValue::Ratio {
                numerator,
                denominator,
            }) => {
                resource.value_ratio = Some(Ratio {
                    numerator: Some(quantity(numerator)?),
                    denominator: Some(quantity(denominator)?),
                    ..Ratio::default()
                });
            }
            Some(ObservationValue::Boolean(value)) => resource.value_boolean = Some(*value),
            Some(ObservationValue::DateTime(value)) => {
                resource.value_date_time = Some(self.time(value));
            }
            Some(ObservationValue::Attachment {
                content_type,
                data,
                title,
            }) => {
                attachments.push(Attachment {
                    content_type: content_type.clone(),
                    data: Some(data.clone()),
                    title: title.clone(),
                    ..Attachment::default()
                });
                notes.push(Annotation::text(ATTACHMENT_NOTE));
            }
        }
        if let Some(range) = &observation.reference_range {
            let like = resource.value_quantity.as_ref();
            resource.reference_range.push(ObservationReferenceRange {
                low: range.low.as_ref().map(|v| limit(v, like)).transpose()?,
                high: range.high.as_ref().map(|v| limit(v, like)).transpose()?,
                text: range.text.clone(),
                ..ObservationReferenceRange::default()
            });
        }
        notes.extend(observation.notes.iter().map(Annotation::text));
        resource.note = notes;
        let conditional = Some(format!(
            "identifier={}",
            token(Some(OXIM_OBSERVATION), &own)
        ));
        Ok(self.push(&format!("observation-{key}"), resource.into(), conditional))
    }

    fn category(&self, qc: bool) -> Vec<CodeableConcept> {
        let mut category = vec![CodeableConcept::coding(Coding::new(
            Some(OBSERVATION_CATEGORY),
            &self.settings.observation_category,
        ))];
        if qc {
            category.push(CodeableConcept::coding(
                Coding::new(Some(OXIM_CATEGORY), QUALITY_CONTROL)
                    .with_display(Some("Quality control")),
            ));
        }
        category
    }

    fn result_group(&mut self, index: usize, group: &ResultGroup) -> FhirResult<()> {
        let subject = group.patient.as_ref().map(|p| self.patient(index, p));
        let specimen = group
            .specimen
            .as_ref()
            .map(|s| self.specimen(index, s, subject.as_deref()));
        let all_final = group.observations.iter().all(|o| is_final(o.status));
        let requests = match &group.order {
            Some(order) => self.requests(
                index,
                order,
                if all_final { "completed" } else { "active" },
                subject.as_deref(),
                specimen
                    .as_deref()
                    .zip(group.specimen.as_ref())
                    .map(|(url, s)| (url, s.identifiers.as_slice())),
            ),
            None => Vec::new(),
        };
        if group.observations.is_empty() {
            return Ok(());
        }
        let mut attachments = Vec::new();
        let mut results = Vec::new();
        for (position, observation) in group.observations.iter().enumerate() {
            let based_on = requests_for(group.order.as_ref(), &requests, observation);
            results.push(self.observation(
                &format!("{index}-{position}"),
                observation,
                self.category(false),
                subject.as_deref(),
                specimen.as_deref(),
                &based_on,
                Vec::new(),
                &mut attachments,
            )?);
        }
        let statuses: Vec<ObservationStatus> =
            group.observations.iter().map(|o| o.status).collect();
        let status = if !all_final {
            "preliminary"
        } else if statuses
            .iter()
            .any(|s| matches!(s, ObservationStatus::Corrected | ObservationStatus::Amended))
        {
            "corrected"
        } else {
            "final"
        };
        let code = group
            .order
            .as_ref()
            .and_then(|o| o.tests.first())
            .map(concept)
            .unwrap_or_else(|| {
                CodeableConcept::coding(
                    Coding::new(Some(LOINC), "11502-2").with_display(Some("Laboratory report")),
                )
            });
        let own = format!("{}-{index}", self.message);
        let report = DiagnosticReport {
            identifier: vec![oxim_identifier(OXIM_DIAGNOSTIC_REPORT, own.clone())],
            based_on: requests.iter().map(Reference::to).collect(),
            status: status.into(),
            category: vec![CodeableConcept::coding(Coding::new(
                Some(V2_SERVICE_SECTION),
                "LAB",
            ))],
            code,
            subject: subject.as_deref().map(Reference::to),
            effective_date_time: group
                .observations
                .iter()
                .find_map(|o| o.effective_at.as_ref())
                .map(|t| self.time(t)),
            issued: Some(fhir_instant(
                self.received_at,
                self.settings.utc_offset_minutes,
            )),
            specimen: specimen.iter().map(Reference::to).collect(),
            result: results.iter().map(Reference::to).collect(),
            presented_form: attachments,
            ..DiagnosticReport::default()
        };
        let conditional = Some(format!(
            "identifier={}",
            token(Some(OXIM_DIAGNOSTIC_REPORT), &own)
        ));
        self.push(&format!("report-{index}"), report.into(), conditional);
        Ok(())
    }
}

/// Encodes normalized content as a FHIR Bundle. Queries and device events
/// have no FHIR mapping here and are rejected.
pub fn encode_bundle(
    content: &ClinicalContent,
    settings: &FhirEncoding,
    message: MessageId,
    received_at: Timestamp,
) -> FhirResult<Bundle> {
    let mut builder = Builder {
        settings,
        message,
        received_at,
        entries: Vec::new(),
        patients: Vec::new(),
        device: None,
    };
    match content {
        ClinicalContent::Results { device, groups } => {
            if let Some(device) = device {
                builder.device = builder.device(device);
            }
            for (index, group) in groups.iter().enumerate() {
                builder.result_group(index, group)?;
            }
        }
        ClinicalContent::QualityControl { device, results } => {
            if let Some(device) = device {
                builder.device = builder.device(device);
            }
            for (index, result) in results.iter().enumerate() {
                let mut details = Vec::new();
                for (label, value) in [
                    ("material", result.material.clone()),
                    ("lot", result.lot.clone()),
                    ("level", result.level.clone()),
                    ("expires", result.expires_at.as_ref().map(fhir_date)),
                ] {
                    if let Some(value) = value {
                        details.push(format!("{label}={value}"));
                    }
                }
                let notes = if details.is_empty() {
                    Vec::new()
                } else {
                    vec![Annotation::text(format!(
                        "{QC_NOTE_PREFIX}{}",
                        details.join("; ")
                    ))]
                };
                let mut attachments = Vec::new();
                let category = builder.category(true);
                builder.observation(
                    &format!("qc-{index}"),
                    &result.observation,
                    category,
                    None,
                    None,
                    &[],
                    notes,
                    &mut attachments,
                )?;
            }
        }
        ClinicalContent::Orders { groups } => {
            for (index, group) in groups.iter().enumerate() {
                let subject = group.patient.as_ref().map(|p| builder.patient(index, p));
                let specimen = group
                    .specimen
                    .as_ref()
                    .map(|s| builder.specimen(index, s, subject.as_deref()));
                let status = match group.order.control {
                    Some(OrderControl::Cancel) => "revoked",
                    _ => "active",
                };
                builder.requests(
                    index,
                    &group.order,
                    status,
                    subject.as_deref(),
                    specimen
                        .as_deref()
                        .zip(group.specimen.as_ref())
                        .map(|(url, s)| (url, s.identifiers.as_slice())),
                );
            }
        }
        ClinicalContent::Query { .. } => {
            return Err(FhirError::Unsupported(
                "host queries have no FHIR bundle mapping".into(),
            ));
        }
        ClinicalContent::DeviceEvent { .. } => {
            return Err(FhirError::Unsupported(
                "device events have no FHIR bundle mapping".into(),
            ));
        }
    }
    Ok(Bundle {
        kind: match settings.bundle_type {
            BundleType::Transaction => "transaction".into(),
            BundleType::Collection => "collection".into(),
        },
        timestamp: Some(fhir_instant(received_at, settings.utc_offset_minutes)),
        entry: builder.entries,
        ..Bundle::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times_as_fhir_requires() {
        let minute = ClinicalDateTime::parse_hl7("202609291430").unwrap();
        assert_eq!(fhir_datetime(&minute, 180), "2026-09-29T14:30:00+03:00");
        let full = ClinicalDateTime::parse_hl7("20260929143005.25-0500").unwrap();
        assert_eq!(fhir_datetime(&full, 180), "2026-09-29T14:30:05.25-05:00");
        let date = ClinicalDateTime::parse_hl7("19800101").unwrap();
        assert_eq!(fhir_datetime(&date, 180), "1980-01-01");
        let utc = ClinicalDateTime::parse_hl7("20260929143005").unwrap();
        assert_eq!(fhir_datetime(&utc, 0), "2026-09-29T14:30:05Z");
        let instant: Timestamp = "2026-09-29T11:30:00Z".parse().unwrap();
        assert_eq!(fhir_instant(instant, 180), "2026-09-29T14:30:00+03:00");
    }

    #[test]
    fn recognizes_ucum_like_units() {
        for unit in [
            "mmol/L",
            "10*9/L",
            "g/dL",
            "%",
            "[IU]/L",
            "mL/min/{1.73_m2}",
        ] {
            assert!(looks_like_ucum(unit), "{unit}");
        }
        for unit in ["", "per high power field", "µg/L"] {
            assert!(!looks_like_ucum(unit), "{unit}");
        }
    }
}
