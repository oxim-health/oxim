//! Laboratory results in a CDA document mapped to the normalized model.
//!
//! | CDA | Model |
//! |---|---|
//! | `recordTarget/patientRole` (first) | `ResultGroup.patient` of every group |
//! | `author/assignedAuthor/assignedAuthoringDevice` (first) | `Results.device` |
//! | `organizer` | one `ResultGroup`; its `code` becomes `Order.tests`, its `specimen` the group's `Specimen` |
//! | `observation` (outside an organizer) | a group of the section's loose observations |
//! | `act/entryRelationship/*` | searched for organizers and observations |
//! | `observation/code` (with `translation`) | `Observation.code` |
//! | `value` `PQ` | `Quantity` (value text kept exactly, UCUM unit) |
//! | `value` `IVL_PQ` with both limits | `Range`; with one limit a `Quantity` with a comparator (`<`, `<=`, `>`, `>=` from `inclusive`) |
//! | `value` `RTO_PQ_PQ`, `RTO` | `Ratio` |
//! | `value` `CD`, `CE`, `CV`, `CO`, `CS` | `Coded` |
//! | `value` `ST`, `ED` (`ED` with `representation="B64"` is an `Attachment`) | `Text` |
//! | `value` `BL`, `TS`, `INT`, `REAL` | `Boolean`, `DateTime`, unitless `Quantity` |
//! | `interpretationCode` | `interpretation`, codes unchanged |
//! | `referenceRange/observationRange` | `reference_range` (`low`/`high` values, `text`) |
//! | `statusCode` `completed`, `active`, `new`, `aborted`/`cancelled`, `nullified` | `Final`, `Preliminary`, `Registered`, `Cancelled`, `EnteredInError` |
//! | `effectiveTime` (`@value`, else `low/@value`) | `effective_at` |
//! | `methodCode` | `method` |
//! | `specimen/specimenRole/id` | `specimen_id` |
//! | `entryRelationship/act/text` | `notes` |
//!
//! Only observations in event mood (`moodCode="EVN"` or none) are results.
//! A document whose `code` is LOINC `11502-2` (Laboratory report) is read
//! in full; other documents only in their laboratory sections (LOINC
//! `30954-2`, `26436-6`, `11502-2` and the IHE XD-LAB specialty section
//! codes). Values are carried, never interpreted (ADR 0011).

use oxim_formats::{XmlDocument, XmlElement};
use oxim_model::{
    ClinicalContent, Comparator, Decimal, Observation, ObservationStatus, ObservationValue, Order,
    Quantity, ReferenceRange, ResultGroup, Specimen,
};

use crate::codes::LOINC_OID;
use crate::error::{CdaError, CdaResult};
use crate::header::{body_sections, collapse, concept, header, identifier, root, statement, time};

/// LOINC section codes that hold laboratory results.
const LAB_SECTIONS: [&str; 21] = [
    "11502-2", "26436-6", "30954-2", "18717-9", "18718-7", "18719-5", "18720-3", "18721-1",
    "18722-9", "18723-7", "18724-5", "18725-2", "18727-8", "18728-6", "18729-4", "18767-4",
    "18768-2", "18769-0", "26435-8", "26437-4", "26438-2",
];

fn code_of(element: XmlElement<'_>) -> Option<(String, Option<String>)> {
    let code = element.child("code")?;
    Some((code.attribute("code")?, code.attribute("codeSystem")))
}

fn is_lab_section(section: XmlElement<'_>) -> bool {
    code_of(section).is_some_and(|(code, system)| {
        system.as_deref().is_none_or(|s| s == LOINC_OID) && LAB_SECTIONS.contains(&code.as_str())
    })
}

fn decimal(text: Option<String>) -> Option<Decimal> {
    text.and_then(|t| Decimal::new(t.trim()).ok())
}

fn quantity(element: XmlElement<'_>) -> Option<Quantity> {
    let value = decimal(element.attribute("value"))?;
    let unit = element
        .attribute("unit")
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty());
    let mut quantity = Quantity::new(value, unit.clone());
    if let Some(unit) = unit {
        quantity.system = Some("http://unitsofmeasure.org".to_owned());
        quantity.code = Some(unit);
    }
    Some(quantity)
}

fn value_type(value: XmlElement<'_>) -> String {
    value
        .attribute("xsi:type")
        .or_else(|| value.attribute("type"))
        .map(|t| t.rsplit(':').next().unwrap_or_default().to_owned())
        .unwrap_or_default()
}

fn inclusive(element: XmlElement<'_>) -> bool {
    element.attribute("inclusive").as_deref() != Some("false")
}

fn interval(value: XmlElement<'_>) -> Option<ObservationValue> {
    let low = value
        .child("low")
        .filter(|l| l.attribute("value").is_some());
    let high = value
        .child("high")
        .filter(|h| h.attribute("value").is_some());
    match (low, high) {
        (Some(low), Some(high)) => Some(ObservationValue::Range {
            low: quantity(low),
            high: quantity(high),
        }),
        (None, Some(high)) => {
            let mut q = quantity(high)?;
            q.comparator = Some(if inclusive(high) {
                Comparator::LessOrEqual
            } else {
                Comparator::LessThan
            });
            Some(ObservationValue::Quantity(q))
        }
        (Some(low), None) => {
            let mut q = quantity(low)?;
            q.comparator = Some(if inclusive(low) {
                Comparator::GreaterOrEqual
            } else {
                Comparator::GreaterThan
            });
            Some(ObservationValue::Quantity(q))
        }
        (None, None) => None,
    }
}

fn text_value(value: XmlElement<'_>) -> Option<ObservationValue> {
    let text = value
        .attribute("value")
        .unwrap_or_else(|| collapse(&value.text_content()));
    (!text.is_empty()).then_some(ObservationValue::Text(text))
}

/// Maps a `value` element by its `xsi:type`.
fn observation_value(value: XmlElement<'_>) -> Option<ObservationValue> {
    match value_type(value).as_str() {
        "PQ" => match quantity(value) {
            Some(q) => Some(ObservationValue::Quantity(q)),
            None => text_value(value),
        },
        "INT" | "REAL" => match decimal(value.attribute("value")) {
            Some(d) => Some(ObservationValue::Quantity(Quantity::new(d, None))),
            None => text_value(value),
        },
        "IVL_PQ" | "IVL_INT" | "IVL_REAL" => interval(value),
        "RTO_PQ_PQ" | "RTO" | "RTO_INT_INT" => {
            let numerator = value.child("numerator").and_then(quantity);
            let denominator = value.child("denominator").and_then(quantity);
            match (numerator, denominator) {
                (Some(numerator), Some(denominator)) => Some(ObservationValue::Ratio {
                    numerator,
                    denominator,
                }),
                _ => text_value(value),
            }
        }
        "CD" | "CE" | "CV" | "CO" | "CS" => concept(value).map(ObservationValue::Coded),
        "BL" => match value.attribute("value").as_deref() {
            Some("true") => Some(ObservationValue::Boolean(true)),
            Some("false") => Some(ObservationValue::Boolean(false)),
            _ => None,
        },
        "TS" => time(value).map(ObservationValue::DateTime),
        "ED" if value.attribute("representation").as_deref() == Some("B64") => {
            Some(ObservationValue::Attachment {
                content_type: value.attribute("mediaType"),
                data: value.text().split_whitespace().collect(),
                title: None,
            })
        }
        _ => text_value(value),
    }
}

fn reference_range(element: XmlElement<'_>) -> Option<ReferenceRange> {
    let range = element.child("observationRange")?;
    let value = range.child("value");
    let limit = |name: &str| {
        value
            .and_then(|v| v.child(name))
            .and_then(|l| decimal(l.attribute("value")))
    };
    let text = range
        .child("text")
        .map(|t| collapse(&t.text_content()))
        .filter(|t| !t.is_empty());
    let result = ReferenceRange {
        low: limit("low"),
        high: limit("high"),
        text,
    };
    (result != ReferenceRange::default()).then_some(result)
}

fn status(element: XmlElement<'_>) -> ObservationStatus {
    match element
        .child("statusCode")
        .and_then(|s| s.attribute("code"))
        .as_deref()
    {
        Some("completed") => ObservationStatus::Final,
        Some("new") => ObservationStatus::Registered,
        Some("active") => ObservationStatus::Preliminary,
        Some("aborted" | "cancelled") => ObservationStatus::Cancelled,
        Some("nullified") => ObservationStatus::EnteredInError,
        _ => ObservationStatus::Unknown,
    }
}

fn specimen(holder: XmlElement<'_>) -> Option<Specimen> {
    let role = holder.child("specimen")?.child("specimenRole")?;
    let entity = role.child("specimenPlayingEntity");
    Some(Specimen {
        identifiers: role.children_named("id").filter_map(identifier).collect(),
        kind: entity.and_then(|e| e.child("code")).and_then(concept),
        ..Specimen::default()
    })
}

fn is_event(statement: XmlElement<'_>) -> bool {
    statement
        .attribute("moodCode")
        .is_none_or(|mood| mood == "EVN")
}

fn observation(element: XmlElement<'_>, sequence: u32) -> Option<Observation> {
    let code = element.child("code").and_then(concept)?;
    let notes = element
        .children_named("entryRelationship")
        .filter_map(|relationship| relationship.child("act"))
        .filter_map(|act| act.child("text"))
        .map(|t| collapse(&t.text_content()))
        .filter(|t| !t.is_empty())
        .collect();
    Some(Observation {
        sequence: Some(sequence),
        code,
        value: element.children_named("value").find_map(observation_value),
        reference_range: element
            .children_named("referenceRange")
            .find_map(reference_range),
        interpretation: element
            .children_named("interpretationCode")
            .filter_map(concept)
            .flat_map(|c| c.codings)
            .collect(),
        status: status(element),
        effective_at: element.child("effectiveTime").and_then(time),
        method: element.child("methodCode").and_then(concept),
        specimen_id: specimen(element)
            .and_then(|s| s.identifiers.into_iter().next())
            .map(|id| id.value),
        notes,
        ..Observation::default()
    })
}

struct Collector {
    groups: Vec<ResultGroup>,
}

impl Collector {
    /// Collects the results of a clinical statement into `group`, opening
    /// a new group for every organizer.
    fn statement(&mut self, element: XmlElement<'_>, group: &mut ResultGroup, depth: usize) {
        if depth > 32 || !is_event(element) {
            return;
        }
        match element.local_name().as_str() {
            "observation" => {
                let sequence = u32::try_from(group.observations.len() + 1).unwrap_or(u32::MAX);
                if let Some(observation) = observation(element, sequence) {
                    group.observations.push(observation);
                }
            }
            "organizer" => {
                let mut inner = ResultGroup {
                    specimen: specimen(element),
                    order: element.child("code").and_then(concept).map(|test| Order {
                        tests: vec![test],
                        ..Order::default()
                    }),
                    ..ResultGroup::default()
                };
                if let (Some(order), Some(specimen)) = (&mut inner.order, &inner.specimen)
                    && let Some(id) = specimen.identifiers.first()
                {
                    order.specimen_ids.push(id.value.clone());
                }
                for component in element.children_named("component") {
                    if let Some(child) = statement(component) {
                        self.statement(child, &mut inner, depth + 1);
                    }
                }
                self.push(inner);
            }
            _ => {
                for relationship in element.children_named("entryRelationship") {
                    if let Some(child) = statement(relationship) {
                        self.statement(child, group, depth + 1);
                    }
                }
            }
        }
    }

    fn push(&mut self, group: ResultGroup) {
        if !group.observations.is_empty() {
            self.groups.push(group);
        }
    }

    fn section(&mut self, section: XmlElement<'_>, all: bool, depth: usize) {
        if depth > 32 {
            return;
        }
        let included = all || is_lab_section(section);
        if included {
            let mut loose = ResultGroup::default();
            for entry in section.children_named("entry") {
                if let Some(child) = statement(entry) {
                    self.statement(child, &mut loose, 0);
                }
            }
            self.push(loose);
        }
        for nested in section
            .children_named("component")
            .filter_map(|c| c.child("section"))
        {
            self.section(nested, included, depth + 1);
        }
    }
}

/// Maps the laboratory results of a CDA document to normalized `Results`.
pub fn lab_results(document: &XmlDocument) -> CdaResult<ClinicalContent> {
    let root = root(document)?;
    let header = header(document)?;
    let whole = code_of(root).is_some_and(|(code, system)| {
        code == "11502-2" && system.as_deref().is_none_or(|s| s == LOINC_OID)
    });
    let mut collector = Collector { groups: Vec::new() };
    for section in body_sections(root) {
        collector.section(section, whole, 0);
    }
    if collector.groups.is_empty() {
        return Err(CdaError::NoResults);
    }
    let patient = header.patients.into_iter().next();
    let device = header.authors.into_iter().find_map(|author| author.device);
    let groups = collector
        .groups
        .into_iter()
        .map(|mut group| {
            group.patient.clone_from(&patient);
            group
        })
        .collect();
    Ok(ClinicalContent::Results { device, groups })
}
