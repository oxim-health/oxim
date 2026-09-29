//! POCT1-A and the normalized model.
//!
//! | POCT1-A | Model |
//! |---|---|
//! | `OBS` message, patient observations (`SVC.role_cd` `OBS` or under `PT`) | `Results`, grouped by consecutive `PT.patient_id` |
//! | `OBS` message, quality control (`LQC`, `EQC`, `QC`, or under `CTC`), calibration (`CAL`), proficiency (`PRF`) | `QualityControl`; `QcResult.level` is `calibration` or `proficiency` for those roles |
//! | `OBS.observation_id` (coding system from the `SN` attribute) | `Observation.code` |
//! | `OBS.value` with unit attribute `U` | `Observation.value`, a `Quantity` when numeric |
//! | `OBS.method_cd` | `Observation.method` |
//! | `OBS.interpretation_cd` / `normal_flag` / `qualifier_cd` | `Observation.interpretation`, unchanged |
//! | `OBS.observation_dttm`, else `SVC.observation_dttm` | `Observation.effective_at` |
//! | `OPR.operator_id` | `Observation.operator` |
//! | `*.specimen_cd` | `Specimen.kind` |
//! | `CTC.name`, `CTC.lot_number` | `QcResult.material`, `lot` |
//! | `DST` message (`DST.condition_cd`, `DST.status_dttm`) | `DeviceEvent` with code `DST` |
//! | `EVS` message | `DeviceEvent` with code `EVS` at `HDR.creation_dttm` |
//!
//! A message with both patient and quality control observations maps to
//! `Results`; its control observations become groups without a patient
//! whose specimen kind is the text `quality control`, with the control
//! material and lot as notes.
//!
//! POCT1-A observations carry no result status, so the status stays
//! `unknown`; encoders decide how to present it.

use oxim_model::{
    ClinicalContent, CodeableConcept, Coding, DeviceEvent, Identifier, Observation, Patient,
    QcResult, ResultGroup, Specimen,
};
use oxim_poct1a::{MessageKind, ObservationKind};

use crate::codes::{coding, system_from_hl7};
use crate::error::{MappingError, MappingResult};
use crate::values::{datetime, nonempty, value_from_text};

fn observation(obs: &oxim_poct1a::Observation<'_>) -> Observation {
    let code = obs
        .observation_id()
        .and_then(|id| coding(id, None, obs.coding_system().and_then(system_from_hl7)))
        .map(CodeableConcept::from_coding)
        .unwrap_or_default();
    Observation {
        code,
        value: obs.value().and_then(|v| value_from_text(v, obs.unit())),
        interpretation: obs
            .interpretation()
            .and_then(nonempty)
            .map(Coding::new)
            .into_iter()
            .collect(),
        method: obs
            .method()
            .and_then(nonempty)
            .map(|m| CodeableConcept::from_coding(Coding::new(m))),
        effective_at: obs.observed_at().and_then(datetime),
        operator: obs.operator_id().and_then(nonempty),
        ..Observation::default()
    }
}

fn specimen(obs: &oxim_poct1a::Observation<'_>) -> Option<Specimen> {
    obs.specimen().and_then(nonempty).map(|kind| Specimen {
        kind: Some(CodeableConcept::from_coding(Coding::new(kind))),
        ..Specimen::default()
    })
}

fn observations(message: &oxim_poct1a::Message) -> MappingResult<ClinicalContent> {
    let mut groups: Vec<ResultGroup> = Vec::new();
    let mut last_patient: Option<Option<String>> = None;
    let mut quality_control = Vec::new();
    for obs in message.observations() {
        let observation = observation(&obs);
        match obs.kind() {
            ObservationKind::Patient | ObservationKind::Other => {
                let patient_id = obs.patient_id().and_then(nonempty);
                if last_patient.as_ref() != Some(&patient_id) || groups.is_empty() {
                    groups.push(ResultGroup {
                        patient: patient_id.clone().map(|id| Patient {
                            identifiers: vec![Identifier::new(id)],
                            ..Patient::default()
                        }),
                        specimen: specimen(&obs),
                        ..ResultGroup::default()
                    });
                    last_patient = Some(patient_id);
                }
                if let Some(group) = groups.last_mut() {
                    group.observations.push(observation);
                }
            }
            kind => {
                let level = match kind {
                    ObservationKind::Calibration => Some("calibration".to_owned()),
                    ObservationKind::Proficiency => Some("proficiency".to_owned()),
                    _ => None,
                };
                quality_control.push(QcResult {
                    material: obs.control_name().and_then(nonempty),
                    lot: obs.control_lot().and_then(nonempty),
                    level,
                    expires_at: None,
                    observation,
                });
            }
        }
    }
    if groups.is_empty() && quality_control.is_empty() {
        return Err(MappingError::Unsupported(
            "the POCT1-A observation message has no OBS element".into(),
        ));
    }
    if groups.is_empty() {
        return Ok(ClinicalContent::QualityControl {
            device: None,
            results: quality_control,
        });
    }
    for qc in quality_control {
        let mut observation = qc.observation;
        observation
            .notes
            .extend(qc.material.map(|m| format!("Control material: {m}")));
        observation
            .notes
            .extend(qc.lot.map(|l| format!("Lot: {l}")));
        groups.push(ResultGroup {
            specimen: Some(Specimen {
                kind: Some(CodeableConcept::from_text("quality control")),
                ..Specimen::default()
            }),
            observations: vec![observation],
            ..ResultGroup::default()
        });
    }
    Ok(ClinicalContent::Results {
        device: None,
        groups,
    })
}

/// Maps a POCT1-A message to normalized content (see the module
/// documentation for the rules).
pub fn normalize(message: &oxim_poct1a::Message) -> MappingResult<ClinicalContent> {
    match message.kind() {
        MessageKind::Observation => observations(message),
        MessageKind::DeviceStatus => {
            let status = message.device_status().unwrap_or_default();
            Ok(ClinicalContent::DeviceEvent {
                device: None,
                event: DeviceEvent {
                    code: CodeableConcept::from_coding(Coding::new("DST")),
                    text: status.condition.and_then(nonempty),
                    occurred_at: status.status_dttm.as_deref().and_then(datetime),
                    severity: None,
                },
            })
        }
        MessageKind::Event => Ok(ClinicalContent::DeviceEvent {
            device: None,
            event: DeviceEvent {
                code: CodeableConcept::from_coding(Coding::new("EVS")),
                text: None,
                occurred_at: message.creation_dttm().and_then(datetime),
                severity: None,
            },
        }),
        _ => Err(MappingError::Unsupported(format!(
            "POCT1-A {} messages carry no clinical content",
            message.message_type()
        ))),
    }
}
