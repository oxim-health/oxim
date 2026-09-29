//! The `hl7v2-orm-status` encoder: a modality performed procedure step
//! stored by `dicom-mpps-scp` (or `dicom-mwl-scp` with `mpps: true`) as an
//! HL7 v2 procedure status update for the order placer.
//!
//! Following the IHE Radiology Scheduled Workflow (the Order Filler informs
//! the Order Placer of procedure status changes), each message is an
//! `ORM^O01` with order control `SC` (status changed) and one ORC/OBR pair
//! per requested procedure the step performed:
//!
//! | MPPS | HL7 v2 |
//! |---|---|
//! | Performed Procedure Step Status `IN PROGRESS`, `COMPLETED`, `DISCONTINUED` | ORC-5 order status `IP`, `CM`, `DC` |
//! | Patient ID (issuer), Patient's Name, Birth Date, Sex | PID-3 (CX.4), PID-5, PID-7, PID-8 |
//! | Placer / Filler Order Number / Imaging Service Request | ORC-2 and OBR-2 / ORC-3 and OBR-3 (Accession Number when no filler number) |
//! | Procedure Code Sequence (code, meaning, scheme) | OBR-4, else the Requested Procedure Description as text |
//! | Performed Procedure Step Start / End Date and Time | OBR-7 / OBR-8; ORC-9 is the end for completed and discontinued steps, else the start |
//! | Accession Number, Requested Procedure ID, Scheduled Procedure Step ID | OBR-18, OBR-19, OBR-20 |
//! | Modality | OBR-24 |
//!
//! MSH-10 is the OXIM message identifier and MSH-7 the time the step was
//! received.

use std::sync::Arc;

use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use oxim_core::{Document, Encoded, Encoder, EngineError, MessageContext, StepConfig, StepError};
use oxim_hl7::{Delimiters, Message};
use oxim_model::{ClinicalDateTime, DataType};
use serde::Deserialize;

use crate::net::settings;
use crate::object::DicomObject;
use crate::part10;
use crate::query::{items, text};
use crate::uids;

const NAME: &str = "hl7v2-orm-status";

fn default_sending_application() -> String {
    "OXIM".to_owned()
}

fn default_version() -> String {
    "2.5.1".to_owned()
}

fn default_processing_id() -> String {
    "P".to_owned()
}

/// Settings of the `hl7v2-orm-status` encoder.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrmStatusSettings {
    /// MSH-3.
    #[serde(default = "default_sending_application")]
    pub sending_application: String,
    /// MSH-4.
    #[serde(default)]
    pub sending_facility: Option<String>,
    /// MSH-5.
    #[serde(default)]
    pub receiving_application: Option<String>,
    /// MSH-6.
    #[serde(default)]
    pub receiving_facility: Option<String>,
    /// MSH-12.
    #[serde(default = "default_version")]
    pub version: String,
    /// MSH-11.
    #[serde(default = "default_processing_id")]
    pub processing_id: String,
    /// UTC offset in minutes for MSH-7.
    #[serde(default)]
    pub utc_offset: i16,
}

/// Encodes a performed procedure step as an HL7 v2 status update.
#[derive(Debug, Clone)]
pub struct OrmStatusEncoder {
    settings: OrmStatusSettings,
}

/// ORC-5 for a Performed Procedure Step Status.
fn order_status(status: &str) -> Option<&'static str> {
    match status {
        "IN PROGRESS" => Some("IP"),
        "COMPLETED" => Some("CM"),
        "DISCONTINUED" => Some("DC"),
        _ => None,
    }
}

/// A DICOM date and time as an HL7 DTM.
fn date_time(date: Option<String>, time: Option<String>) -> Option<String> {
    let date = date?;
    let time: String = time
        .unwrap_or_default()
        .chars()
        .take_while(|c| *c != '.')
        .filter(char::is_ascii_digit)
        .collect();
    Some(format!("{date}{time}"))
}

struct Writer {
    message: Message,
}

impl Writer {
    fn set(&mut self, path: &str, value: Option<&str>) -> Result<(), String> {
        match value.filter(|value| !value.is_empty()) {
            Some(value) => self
                .message
                .set(path, value)
                .map_err(|e| format!("{path}: {e}")),
            None => Ok(()),
        }
    }

    fn segment(&mut self, id: &str) -> Result<(), String> {
        self.message
            .push_segment(id)
            .map(|_| ())
            .map_err(|e| format!("{id}: {e}"))
    }
}

impl OrmStatusEncoder {
    /// Builds the status update for an MPPS data set.
    pub fn encode_step(
        &self,
        step: &InMemDicomObject,
        control_id: &str,
        timestamp: Option<ClinicalDateTime>,
    ) -> Result<Message, String> {
        let state = text(step, tags::PERFORMED_PROCEDURE_STEP_STATUS)
            .map(|state| state.to_ascii_uppercase())
            .ok_or("the performed procedure step has no status")?;
        let status = order_status(&state)
            .ok_or_else(|| format!("unknown Performed Procedure Step Status {state:?}"))?;
        let message = Message::new(Delimiters::default()).map_err(|e| e.to_string())?;
        let mut writer = Writer { message };
        let settings = &self.settings;
        writer.set("MSH-3", Some(&settings.sending_application))?;
        writer.set("MSH-4", settings.sending_facility.as_deref())?;
        writer.set("MSH-5", settings.receiving_application.as_deref())?;
        writer.set("MSH-6", settings.receiving_facility.as_deref())?;
        let stamp = timestamp.map(|time| {
            let text = time.to_hl7();
            text.split('.').next().unwrap_or(&text).to_owned()
        });
        writer.set("MSH-7", stamp.as_deref())?;
        writer
            .message
            .set_raw("MSH-9", b"ORM^O01^ORM_O01")
            .map_err(|e| e.to_string())?;
        writer.set("MSH-10", Some(control_id))?;
        writer.set("MSH-11", Some(&settings.processing_id))?;
        writer.set("MSH-12", Some(&settings.version))?;

        writer.segment("PID")?;
        writer.set("PID-1", Some("1"))?;
        writer.set("PID-3.1", text(step, tags::PATIENT_ID).as_deref())?;
        writer.set("PID-3.4", text(step, tags::ISSUER_OF_PATIENT_ID).as_deref())?;
        if let Some(name) = text(step, tags::PATIENT_NAME) {
            // A DICOM PN and an HL7 XPN share the component order.
            let raw = name
                .split('=')
                .next()
                .unwrap_or_default()
                .split('^')
                .map(|part| {
                    oxim_hl7::escape(part.as_bytes(), writer.message.delimiters())
                        .map(|escaped| escaped.into_owned())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(&[writer.message.delimiters().component][..]);
            writer
                .message
                .set_raw("PID-5", &raw)
                .map_err(|e| e.to_string())?;
        }
        writer.set("PID-7", text(step, tags::PATIENT_BIRTH_DATE).as_deref())?;
        writer.set("PID-8", text(step, tags::PATIENT_SEX).as_deref())?;

        let start = date_time(
            text(step, tags::PERFORMED_PROCEDURE_STEP_START_DATE),
            text(step, tags::PERFORMED_PROCEDURE_STEP_START_TIME),
        );
        let end = date_time(
            text(step, tags::PERFORMED_PROCEDURE_STEP_END_DATE),
            text(step, tags::PERFORMED_PROCEDURE_STEP_END_TIME),
        );
        let transaction = if status == "IP" {
            start.clone()
        } else {
            end.clone().or_else(|| start.clone())
        };
        let procedure = items(step, tags::PROCEDURE_CODE_SEQUENCE).first().cloned();
        let modality = text(step, tags::MODALITY);
        let references = items(step, tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE);
        let empty = InMemDicomObject::new_empty();
        let references: Vec<&InMemDicomObject> = if references.is_empty() {
            // An unscheduled step still reports its status.
            vec![&empty]
        } else {
            references.iter().collect()
        };
        for (index, reference) in references.into_iter().enumerate() {
            let n = index + 1;
            let accession = text(reference, tags::ACCESSION_NUMBER);
            let placer = text(reference, tags::PLACER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST);
            let filler = text(reference, tags::FILLER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST)
                .or_else(|| accession.clone());
            writer.segment("ORC")?;
            writer.set(&format!("ORC[{n}]-1"), Some("SC"))?;
            writer.set(&format!("ORC[{n}]-2"), placer.as_deref())?;
            writer.set(&format!("ORC[{n}]-3"), filler.as_deref())?;
            writer.set(&format!("ORC[{n}]-5"), Some(status))?;
            writer.set(&format!("ORC[{n}]-9"), transaction.as_deref())?;
            writer.segment("OBR")?;
            writer.set(&format!("OBR[{n}]-1"), Some(&n.to_string()))?;
            writer.set(&format!("OBR[{n}]-2"), placer.as_deref())?;
            writer.set(&format!("OBR[{n}]-3"), filler.as_deref())?;
            match &procedure {
                Some(code) => {
                    writer.set(
                        &format!("OBR[{n}]-4.1"),
                        text(code, tags::CODE_VALUE).as_deref(),
                    )?;
                    writer.set(
                        &format!("OBR[{n}]-4.2"),
                        text(code, tags::CODE_MEANING).as_deref(),
                    )?;
                    writer.set(
                        &format!("OBR[{n}]-4.3"),
                        text(code, tags::CODING_SCHEME_DESIGNATOR).as_deref(),
                    )?;
                }
                None => {
                    let description = text(reference, tags::REQUESTED_PROCEDURE_DESCRIPTION)
                        .or_else(|| text(step, tags::PERFORMED_PROCEDURE_STEP_DESCRIPTION));
                    writer.set(&format!("OBR[{n}]-4.2"), description.as_deref())?;
                }
            }
            writer.set(&format!("OBR[{n}]-7"), start.as_deref())?;
            writer.set(&format!("OBR[{n}]-8"), end.as_deref())?;
            writer.set(&format!("OBR[{n}]-18"), accession.as_deref())?;
            writer.set(
                &format!("OBR[{n}]-19"),
                text(reference, tags::REQUESTED_PROCEDURE_ID).as_deref(),
            )?;
            writer.set(
                &format!("OBR[{n}]-20"),
                text(reference, tags::SCHEDULED_PROCEDURE_STEP_ID).as_deref(),
            )?;
            writer.set(&format!("OBR[{n}]-24"), modality.as_deref())?;
        }
        Ok(writer.message)
    }
}

/// Whether the message is a stored performed procedure step.
fn is_performed_step(context: &MessageContext) -> bool {
    match &context.document {
        Document::Raw(bytes) => {
            part10::parse(bytes).is_ok_and(|part10| part10.meta.sop_class_uid == uids::MPPS)
        }
        _ => false,
    }
}

impl Encoder for OrmStatusEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        if !is_performed_step(context) {
            return Err(StepError::new(
                NAME,
                "the message is not a modality performed procedure step",
            ));
        }
        let raw = crate::steps::raw(context, NAME)?;
        let object = DicomObject::parse(raw).map_err(|e| StepError::new(NAME, e.to_string()))?;
        let timestamp = ClinicalDateTime::from_timestamp(
            context.envelope.received_at,
            self.settings.utc_offset,
        );
        let message = self
            .encode_step(&object.dataset, &context.envelope.id.to_string(), timestamp)
            .map_err(|e| StepError::new(NAME, e))?;
        Ok(Encoded {
            data_type: DataType::Hl7V2,
            data: message.to_bytes(),
        })
    }

    fn handles(&self, context: &MessageContext) -> bool {
        is_performed_step(context)
    }
}

/// Registers the `hl7v2-orm-status` encoder.
pub(crate) fn register(registry: &mut oxim_core::Registry) {
    registry.add_encoder(NAME, |step: &StepConfig| {
        let settings: OrmStatusSettings = settings(&step.settings, "hl7v2-orm-status encoder")?;
        if !(-720..=840).contains(&settings.utc_offset) {
            return Err(EngineError::Config(
                "hl7v2-orm-status encoder: utc_offset must be minutes between -720 and 840".into(),
            ));
        }
        Ok(Arc::new(OrmStatusEncoder { settings }) as Arc<dyn Encoder>)
    });
}

#[cfg(test)]
mod tests {
    use dicom_core::VR;

    use super::*;
    use crate::query::{sequence, text_element};

    fn encoder() -> OrmStatusEncoder {
        OrmStatusEncoder {
            settings: OrmStatusSettings {
                sending_application: "OXIM".into(),
                sending_facility: None,
                receiving_application: Some("RIS".into()),
                receiving_facility: None,
                version: "2.5.1".into(),
                processing_id: "P".into(),
                utc_offset: 0,
            },
        }
    }

    #[test]
    fn writes_status_updates() {
        let step = InMemDicomObject::from_element_iter([
            text_element(tags::PATIENT_NAME, VR::PN, "SYNTHETIC^PATIENT"),
            text_element(tags::PATIENT_ID, VR::LO, "SYN-0001"),
            text_element(tags::PATIENT_BIRTH_DATE, VR::DA, "19800115"),
            text_element(tags::PATIENT_SEX, VR::CS, "O"),
            text_element(tags::MODALITY, VR::CS, "CT"),
            text_element(tags::PERFORMED_PROCEDURE_STEP_STATUS, VR::CS, "COMPLETED"),
            text_element(
                tags::PERFORMED_PROCEDURE_STEP_START_DATE,
                VR::DA,
                "20240229",
            ),
            text_element(tags::PERFORMED_PROCEDURE_STEP_START_TIME, VR::TM, "101500"),
            text_element(tags::PERFORMED_PROCEDURE_STEP_END_DATE, VR::DA, "20240229"),
            text_element(tags::PERFORMED_PROCEDURE_STEP_END_TIME, VR::TM, "103000.25"),
            sequence(
                tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE,
                vec![InMemDicomObject::from_element_iter([
                    text_element(tags::ACCESSION_NUMBER, VR::SH, "ACC-7"),
                    text_element(
                        tags::PLACER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST,
                        VR::LO,
                        "ORD-7",
                    ),
                    text_element(tags::REQUESTED_PROCEDURE_ID, VR::SH, "ACC-7"),
                    text_element(tags::SCHEDULED_PROCEDURE_STEP_ID, VR::SH, "ACC-7"),
                    text_element(tags::REQUESTED_PROCEDURE_DESCRIPTION, VR::LO, "CT head"),
                ])],
            ),
        ]);
        let message = encoder().encode_step(&step, "MSG1", None).unwrap();
        let text = String::from_utf8(message.to_bytes())
            .unwrap()
            .replace('\r', "\n");
        assert_eq!(
            text,
            "MSH|^~\\&|OXIM||RIS||||ORM^O01^ORM_O01|MSG1|P|2.5.1
PID|1||SYN-0001||SYNTHETIC^PATIENT||19800115|O
ORC|SC|ORD-7|ACC-7||CM||||20240229103000
OBR|1|ORD-7|ACC-7|^CT head|||20240229101500|20240229103000||||||||||ACC-7|ACC-7|ACC-7||||CT
"
        );
        let mut bad = step.clone();
        bad.put(text_element(
            tags::PERFORMED_PROCEDURE_STEP_STATUS,
            VR::CS,
            "PAUSED",
        ));
        assert!(encoder().encode_step(&bad, "MSG2", None).is_err());
    }
}
