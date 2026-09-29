//! FHIR → the normalized model.
//!
//! [`normalize`] reads a Bundle (any type) or a single Observation,
//! DiagnosticReport or ServiceRequest:
//!
//! - Each DiagnosticReport becomes a result group: its `subject` (a
//!   Patient), first `specimen`, `basedOn` requests (merged into one order,
//!   one test per request) and `result` observations, in order.
//! - Observations no report refers to are grouped by subject and specimen.
//! - When every observation carries the OXIM quality-control category and
//!   there is no report, the content is quality control; otherwise results.
//! - Without observations, ServiceRequests become orders, grouped by
//!   subject, specimen and placer/filler numbers (`revoked` → cancel,
//!   anything else → new).
//! - The first Device becomes the content's device.
//!
//! References are resolved against entry `fullUrl`s and `Type/id`
//! (absolute URLs by their last two segments); unresolved references leave
//! the patient, specimen or order empty. Displays are read for the
//! observation's device, operator and specimen id, as written by
//! [`encode_bundle`](crate::encode_bundle).

use oxim_model::{
    AdministrativeSex, ClinicalContent, ClinicalDateTime, Comparator, ObservationStatus,
    ObservationValue, Order, OrderControl, OrderGroup, Priority, QcResult, ReferenceRange,
    ResultGroup,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::datatypes::{Annotation, CodeableConcept, Quantity, Reference};
use crate::error::{FhirError, FhirResult};
use crate::resources::{
    Device, DiagnosticReport, Observation, Patient, Resource, ServiceRequest, Specimen,
};
use crate::to_fhir::{
    ATTACHMENT_NOTE, OXIM_CATEGORY, OXIM_DEVICE_SERIAL, PATIENT_ANIMAL, QC_NOTE_PREFIX,
    QUALITY_CONTROL, V2_IDENTIFIER_TYPE, V3_INTERPRETATION,
};

/// Maps FHIR resources to normalized content.
pub fn normalize(resource: &Resource) -> FhirResult<ClinicalContent> {
    let index = Index::new(resource);
    let observations: Vec<&Observation> = index.all(|r| match r {
        Resource::Observation(o) => Some(o.as_ref()),
        _ => None,
    });
    let reports: Vec<&DiagnosticReport> = index.all(|r| match r {
        Resource::DiagnosticReport(r) => Some(r.as_ref()),
        _ => None,
    });
    let requests: Vec<&ServiceRequest> = index.all(|r| match r {
        Resource::ServiceRequest(r) => Some(r.as_ref()),
        _ => None,
    });
    let device = index
        .all(|r| match r {
            Resource::Device(d) => Some(d.as_ref()),
            _ => None,
        })
        .first()
        .map(|d| device(d));

    if observations.is_empty() && reports.is_empty() {
        if requests.is_empty() {
            return Err(FhirError::Unsupported(
                "no Observation, DiagnosticReport or ServiceRequest to normalize".into(),
            ));
        }
        return index.orders(&requests);
    }

    if reports.is_empty() && observations.iter().all(|o| is_qc(o)) {
        let results = observations
            .iter()
            .map(|o| qc_result(o))
            .collect::<FhirResult<_>>()?;
        return Ok(ClinicalContent::QualityControl { device, results });
    }

    let mut groups = Vec::new();
    let mut grouped: Vec<&Observation> = Vec::new();
    for report in &reports {
        let mut members = Vec::new();
        for reference in &report.result {
            if let Some(Resource::Observation(o)) = index.resolve(reference) {
                members.push(o.as_ref());
                grouped.push(o.as_ref());
            }
        }
        let mut attachments = report.presented_form.iter();
        let mut normalized = Vec::new();
        for member in &members {
            let mut observation = observation(member)?;
            if observation.value.is_none()
                && member.note.iter().any(|n| n.text == ATTACHMENT_NOTE)
                && let Some(attachment) = attachments.next()
            {
                observation.value = Some(ObservationValue::Attachment {
                    content_type: attachment.content_type.clone(),
                    data: attachment.data.clone().unwrap_or_default(),
                    title: attachment.title.clone(),
                });
            }
            normalized.push(observation);
        }
        groups.push(ResultGroup {
            patient: index.patient(report.subject.as_ref()),
            specimen: index.specimen(report.specimen.first()),
            order: index.order(&report.based_on, None),
            observations: normalized,
        });
    }

    // Observations outside reports, grouped by subject and specimen.
    let mut loose: Vec<(String, Vec<&Observation>)> = Vec::new();
    for o in observations
        .iter()
        .filter(|o| !grouped.iter().any(|g| std::ptr::eq(*g, **o)))
    {
        let key = format!(
            "{}\u{0}{}",
            reference_key(o.subject.as_ref()),
            reference_key(o.specimen.as_ref())
        );
        match loose.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(o),
            None => loose.push((key, vec![o])),
        }
    }
    for (_, members) in loose {
        let first = members[0];
        groups.push(ResultGroup {
            patient: index.patient(first.subject.as_ref()),
            specimen: index.specimen(first.specimen.as_ref()),
            order: index.order(&first.based_on, None),
            observations: members
                .iter()
                .map(|o| observation(o))
                .collect::<FhirResult<_>>()?,
        });
    }
    Ok(ClinicalContent::Results { device, groups })
}

/// Entries of the input with their full URLs.
struct Index<'a> {
    entries: Vec<(Option<&'a str>, &'a Resource)>,
}

impl<'a> Index<'a> {
    fn new(resource: &'a Resource) -> Self {
        let entries = match resource {
            Resource::Bundle(bundle) => bundle
                .entry
                .iter()
                .filter_map(|e| Some((e.full_url.as_deref(), e.resource.as_ref()?)))
                .collect(),
            other => vec![(None, other)],
        };
        Self { entries }
    }

    fn all<T: ?Sized>(&self, pick: impl Fn(&'a Resource) -> Option<&'a T>) -> Vec<&'a T> {
        self.entries.iter().filter_map(|(_, r)| pick(r)).collect()
    }

    fn resolve(&self, reference: &Reference) -> Option<&'a Resource> {
        let target = reference.reference.as_deref()?;
        if let Some((_, found)) = self.entries.iter().find(|(url, _)| *url == Some(target)) {
            return Some(found);
        }
        let relative = relative_reference(target)?;
        self.entries
            .iter()
            .find(|(url, resource)| {
                resource
                    .id()
                    .is_some_and(|id| relative == format!("{}/{id}", resource.resource_type()))
                    || url.and_then(relative_reference).as_deref() == Some(relative.as_str())
            })
            .map(|(_, resource)| *resource)
    }

    fn patient(&self, reference: Option<&Reference>) -> Option<oxim_model::Patient> {
        match self.resolve(reference?)? {
            Resource::Patient(p) => Some(patient(p)),
            _ => None,
        }
    }

    fn specimen(&self, reference: Option<&Reference>) -> Option<oxim_model::Specimen> {
        match self.resolve(reference?)? {
            Resource::Specimen(s) => Some(specimen(s)),
            _ => None,
        }
    }

    /// The requests `references` resolve to, merged into one order.
    fn order(&self, references: &[Reference], control: Option<OrderControl>) -> Option<Order> {
        let requests: Vec<&ServiceRequest> = references
            .iter()
            .filter_map(|r| match self.resolve(r)? {
                Resource::ServiceRequest(request) => Some(request.as_ref()),
                _ => None,
            })
            .collect();
        merge_requests(&requests, control)
    }

    fn orders(&self, requests: &[&ServiceRequest]) -> FhirResult<ClinicalContent> {
        let mut groups: Vec<(String, Vec<&ServiceRequest>)> = Vec::new();
        for request in requests {
            let key = format!(
                "{}\u{0}{}\u{0}{:?}\u{0}{:?}\u{0}{}",
                reference_key(request.subject.as_ref()),
                reference_key(request.specimen.first()),
                typed_identifier(request, "PLAC"),
                typed_identifier(request, "FILL"),
                request.status
            );
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push(request),
                None => groups.push((key, vec![request])),
            }
        }
        let groups = groups
            .into_iter()
            .filter_map(|(_, members)| {
                let first = members[0];
                let control = if first.status == "revoked" {
                    OrderControl::Cancel
                } else {
                    OrderControl::New
                };
                Some(OrderGroup {
                    patient: self.patient(first.subject.as_ref()),
                    specimen: first.specimen.iter().find_map(|r| self.specimen(Some(r))),
                    order: merge_requests(&members, Some(control))?,
                })
            })
            .collect();
        Ok(ClinicalContent::Orders { groups })
    }
}

/// `Type/id` of a relative or absolute reference.
fn relative_reference(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let path = path.find("/_history/").map_or(path, |at| &path[..at]);
    let mut parts = path.rsplit('/');
    let id = parts.next()?;
    let kind = parts.next()?;
    let is_type = kind.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    (!id.is_empty() && is_type).then(|| format!("{kind}/{id}"))
}

fn reference_key(reference: Option<&Reference>) -> String {
    reference
        .and_then(|r| r.reference.clone().or_else(|| r.display.clone()))
        .unwrap_or_default()
}

fn parse_code<T: DeserializeOwned>(code: &str) -> Option<T> {
    serde_json::from_value(Value::String(code.to_owned())).ok()
}

fn datetime(text: Option<&String>) -> Option<ClinicalDateTime> {
    text.and_then(|t| ClinicalDateTime::parse_iso(t).ok())
}

fn concept(concept: &CodeableConcept) -> oxim_model::CodeableConcept {
    oxim_model::CodeableConcept {
        codings: concept
            .coding
            .iter()
            .filter_map(|c| {
                Some(oxim_model::Coding {
                    system: c.system.clone(),
                    code: c.code.clone()?,
                    display: c.display.clone(),
                })
            })
            .collect(),
        text: concept.text.clone(),
    }
}

fn notes(notes: &[Annotation]) -> Vec<String> {
    notes.iter().map(|n| n.text.clone()).collect()
}

fn identifier(id: &crate::datatypes::Identifier) -> Option<oxim_model::Identifier> {
    Some(oxim_model::Identifier {
        system: id.system.clone(),
        value: id.value.clone()?,
        kind: id.kind.as_ref().and_then(|kind| {
            kind.coding
                .iter()
                .find(|c| c.system.as_deref() == Some(V2_IDENTIFIER_TYPE))
                .or_else(|| kind.coding.first())
                .and_then(|c| c.code.clone())
                .or_else(|| kind.text.clone())
        }),
        assigner: id.assigner.as_ref().and_then(|a| a.display.clone()),
    })
}

fn has_type(id: &crate::datatypes::Identifier, code: &str) -> bool {
    id.kind
        .as_ref()
        .is_some_and(|k| k.coding.iter().any(|c| c.code.as_deref() == Some(code)))
}

fn typed_identifier(request: &ServiceRequest, code: &str) -> Option<String> {
    request
        .identifier
        .iter()
        .find(|id| has_type(id, code))
        .and_then(|id| id.value.clone())
}

fn animal(patient: &Patient, part: &str) -> Option<oxim_model::CodeableConcept> {
    let extensions = patient.extra.get("extension")?.as_array()?;
    let animal = extensions
        .iter()
        .find(|e| e.get("url").and_then(Value::as_str) == Some(PATIENT_ANIMAL))?;
    let value = animal
        .get("extension")?
        .as_array()?
        .iter()
        .find(|e| e.get("url").and_then(Value::as_str) == Some(part))?
        .get("valueCodeableConcept")?;
    let value: CodeableConcept = serde_json::from_value(value.clone()).ok()?;
    Some(concept(&value))
}

fn patient(patient: &Patient) -> oxim_model::Patient {
    oxim_model::Patient {
        identifiers: patient.identifier.iter().filter_map(identifier).collect(),
        name: patient.name.first().map(|n| oxim_model::HumanName {
            family: n.family.clone(),
            given: n.given.clone(),
            prefix: n.prefix.first().cloned(),
            suffix: n.suffix.first().cloned(),
        }),
        birth_date: datetime(patient.birth_date.as_ref()),
        sex: patient
            .gender
            .as_deref()
            .and_then(parse_code::<AdministrativeSex>),
        species: animal(patient, "species"),
        breed: animal(patient, "breed"),
    }
}

fn specimen(specimen: &Specimen) -> oxim_model::Specimen {
    oxim_model::Specimen {
        identifiers: specimen
            .identifier
            .iter()
            .chain(&specimen.accession_identifier)
            .filter_map(identifier)
            .collect(),
        kind: specimen.kind.as_ref().map(concept),
        collected_at: specimen
            .collection
            .as_ref()
            .and_then(|c| datetime(c.collected_date_time.as_ref())),
        received_at: datetime(specimen.received_time.as_ref()),
        container: specimen
            .container
            .iter()
            .find_map(|c| c.description.clone()),
        notes: notes(&specimen.note),
    }
}

fn device(device: &Device) -> oxim_model::Device {
    let name = |kind: &str| {
        device
            .device_name
            .iter()
            .find(|n| n.kind == kind)
            .map(|n| n.name.clone())
    };
    oxim_model::Device {
        identifiers: device
            .identifier
            .iter()
            .filter(|id| id.system.as_deref() != Some(OXIM_DEVICE_SERIAL))
            .filter_map(identifier)
            .collect(),
        manufacturer: device.manufacturer.clone(),
        model: name("model-name").or_else(|| device.model_number.clone()),
        serial_number: device.serial_number.clone(),
        software_version: device.version.first().map(|v| v.value.clone()),
        name: name("user-friendly-name"),
    }
}

fn merge_requests(requests: &[&ServiceRequest], control: Option<OrderControl>) -> Option<Order> {
    let first = requests.first()?;
    let specimen_ids = first
        .specimen
        .iter()
        .filter_map(|r| r.display.clone())
        .collect();
    Some(Order {
        placer_id: typed_identifier(first, "PLAC"),
        filler_id: typed_identifier(first, "FILL"),
        tests: requests
            .iter()
            .filter_map(|r| r.code.as_ref().map(concept))
            .collect(),
        priority: first.priority.as_deref().and_then(parse_code::<Priority>),
        requested_at: datetime(first.authored_on.as_ref()),
        specimen_ids,
        control,
        notes: notes(&first.note),
    })
}

fn quantity(quantity: &Quantity) -> FhirResult<Option<oxim_model::Quantity>> {
    let Some(value) = &quantity.value else {
        return Ok(None);
    };
    Ok(Some(oxim_model::Quantity {
        value: value.to_decimal()?,
        comparator: quantity.comparator.as_deref().and_then(comparator),
        unit: quantity.unit.clone(),
        system: quantity.system.clone(),
        code: quantity.code.clone(),
    }))
}

fn comparator(code: &str) -> Option<Comparator> {
    match code {
        "<" => Some(Comparator::LessThan),
        "<=" => Some(Comparator::LessOrEqual),
        ">=" => Some(Comparator::GreaterOrEqual),
        ">" => Some(Comparator::GreaterThan),
        _ => None,
    }
}

fn value(o: &Observation) -> FhirResult<Option<ObservationValue>> {
    if let Some(q) = &o.value_quantity {
        return Ok(quantity(q)?.map(ObservationValue::Quantity));
    }
    if let Some(text) = &o.value_string {
        return Ok(Some(ObservationValue::Text(text.clone())));
    }
    if let Some(c) = &o.value_codeable_concept {
        return Ok(Some(ObservationValue::Coded(concept(c))));
    }
    if let Some(range) = &o.value_range {
        let low = range.low.as_ref().map(quantity).transpose()?.flatten();
        let high = range.high.as_ref().map(quantity).transpose()?.flatten();
        return Ok(Some(ObservationValue::Range { low, high }));
    }
    if let Some(ratio) = &o.value_ratio {
        let numerator = ratio
            .numerator
            .as_ref()
            .map(quantity)
            .transpose()?
            .flatten();
        let denominator = ratio
            .denominator
            .as_ref()
            .map(quantity)
            .transpose()?
            .flatten();
        return Ok(numerator.zip(denominator).map(|(numerator, denominator)| {
            ObservationValue::Ratio {
                numerator,
                denominator,
            }
        }));
    }
    if let Some(value) = o.value_boolean {
        return Ok(Some(ObservationValue::Boolean(value)));
    }
    if let Some(value) = &o.value_date_time {
        let parsed = ClinicalDateTime::parse_iso(value)
            .map_err(|_| FhirError::Invalid(format!("invalid valueDateTime {value:?}")))?;
        return Ok(Some(ObservationValue::DateTime(parsed)));
    }
    Ok(None)
}

fn observation(o: &Observation) -> FhirResult<oxim_model::Observation> {
    Ok(oxim_model::Observation {
        sequence: None,
        code: concept(&o.code),
        value: value(o)?,
        reference_range: o
            .reference_range
            .first()
            .map(|range| -> FhirResult<ReferenceRange> {
                let limit = |q: Option<&Quantity>| {
                    q.and_then(|q| q.value.as_ref())
                        .map(|v| v.to_decimal())
                        .transpose()
                };
                Ok(ReferenceRange {
                    low: limit(range.low.as_ref())?,
                    high: limit(range.high.as_ref())?,
                    text: range.text.clone(),
                })
            })
            .transpose()?,
        interpretation: o
            .interpretation
            .iter()
            .flat_map(|c| &c.coding)
            .filter_map(|c| {
                Some(oxim_model::Coding {
                    system: c.system.clone().filter(|s| s != V3_INTERPRETATION),
                    code: c.code.clone()?,
                    display: c.display.clone(),
                })
            })
            .collect(),
        status: parse_code::<ObservationStatus>(&o.status).unwrap_or_default(),
        effective_at: datetime(o.effective_date_time.as_ref())
            .or_else(|| datetime(o.effective_period.as_ref().and_then(|p| p.start.as_ref()))),
        issued_at: datetime(o.issued.as_ref()),
        method: o.method.as_ref().map(concept),
        device_id: o.device.as_ref().and_then(|d| d.display.clone()),
        operator: o.performer.iter().find_map(|p| p.display.clone()),
        specimen_id: o.specimen.as_ref().and_then(|s| s.display.clone()),
        notes: o
            .note
            .iter()
            .filter(|n| n.text != ATTACHMENT_NOTE && !n.text.starts_with(QC_NOTE_PREFIX))
            .map(|n| n.text.clone())
            .collect(),
    })
}

fn is_qc(o: &Observation) -> bool {
    o.category.iter().any(|c| {
        c.coding.iter().any(|c| {
            c.system.as_deref() == Some(OXIM_CATEGORY) && c.code.as_deref() == Some(QUALITY_CONTROL)
        })
    })
}

fn qc_result(o: &Observation) -> FhirResult<QcResult> {
    let mut result = QcResult {
        observation: observation(o)?,
        ..QcResult::default()
    };
    let details = o
        .note
        .iter()
        .find_map(|n| n.text.strip_prefix(QC_NOTE_PREFIX));
    for part in details.into_iter().flat_map(|d| d.split("; ")) {
        let Some((label, value)) = part.split_once('=') else {
            continue;
        };
        match label {
            "material" => result.material = Some(value.to_owned()),
            "lot" => result.lot = Some(value.to_owned()),
            "level" => result.level = Some(value.to_owned()),
            "expires" => result.expires_at = ClinicalDateTime::parse_iso(value).ok(),
            _ => {}
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute_references() {
        assert_eq!(
            relative_reference("https://fhir.example.test/r4/Patient/p1/_history/2").as_deref(),
            Some("Patient/p1")
        );
        assert_eq!(
            relative_reference("Patient/p1").as_deref(),
            Some("Patient/p1")
        );
        assert_eq!(relative_reference("urn:uuid:0000"), None);
    }

    #[test]
    fn normalizes_observations_with_server_ids() {
        let bundle = Resource::from_json(
            br#"{"resourceType":"Bundle","type":"searchset","entry":[
              {"fullUrl":"https://fhir.example.test/Patient/p1","resource":{"resourceType":"Patient","id":"p1","identifier":[{"value":"MRN-1"}]}},
              {"resource":{"resourceType":"Observation","status":"final","code":{"text":"Glucose"},
                "subject":{"reference":"Patient/p1"},"valueQuantity":{"value":5.40,"unit":"mmol/L"}}}
            ]}"#,
        )
        .unwrap();
        let ClinicalContent::Results { groups, .. } = normalize(&bundle).unwrap() else {
            panic!("expected results");
        };
        assert_eq!(groups.len(), 1);
        let patient = groups[0].patient.as_ref().unwrap();
        assert_eq!(patient.identifiers[0].value, "MRN-1");
        let Some(ObservationValue::Quantity(q)) = &groups[0].observations[0].value else {
            panic!("expected a quantity");
        };
        assert_eq!(q.value.as_str(), "5.40");
    }

    #[test]
    fn rejects_content_without_clinical_resources() {
        let patient = Resource::from_json(br#"{"resourceType":"Patient"}"#).unwrap();
        assert!(matches!(
            normalize(&patient),
            Err(FhirError::Unsupported(_))
        ));
    }
}
