//! The modality worklist: scheduled procedure steps built from orders,
//! answered to modalities with C-FIND (`dicom-mwl-scp`), and the state of
//! the performed procedure steps modalities report with MPPS.
//!
//! The `worklist-from-orders` step turns normalized `Orders` (from HL7
//! `ORM`/`OML`, FHIR `ServiceRequest` or any other normalizer) into
//! worklist items, one per ordered procedure:
//!
//! | Order | Worklist item |
//! |---|---|
//! | Filler order number, else placer order number | Accession Number (first 16 characters) |
//! | Test position | Requested Procedure ID and Scheduled Procedure Step ID: the accession number, with `.n` for the n-th test of a multi-test order |
//! | Test code, display and system | Requested Procedure Code Sequence; text as Requested Procedure Description |
//! | Requested date/time, else the time the order was received | Scheduled Procedure Step Start Date and Time |
//! | Priority | Requested Procedure Priority (`STAT`, `HIGH`, `ROUTINE`) |
//! | Patient identifier, name, birth date, sex | Patient ID (with issuer), Patient's Name, Birth Date, Sex |
//! | Placer and filler order numbers | Placer/Filler Order Number / Imaging Service Request |
//! | Accession number and procedure ID | Study Instance UID, derived so every system computes the same UID |
//!
//! New and added orders create or update items, a replacement replaces the
//! items of its accession number and a cancellation discontinues them.
//! Items are offered while they are `SCHEDULED` or `IN PROGRESS` and until
//! they expire (`expire_after` past their start, 7 days by default). MPPS
//! `IN PROGRESS`, `COMPLETED` and `DISCONTINUED` update the status of the
//! items they reference.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dicom_core::VR;
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use oxim_core::config::DurationText;
use oxim_core::{EngineError, MessageContext, StepConfig, StepError, Transformer};
use oxim_model::{
    AdministrativeSex, ClinicalContent, ClinicalDateTime, CodeableConcept, OrderControl,
    OrderGroup, Priority, Timestamp,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::environment::{DicomEnvironment, open_database};
use crate::error::DicomError;
use crate::net::{settings, validate_ae_title};
use crate::object;
use crate::query::{MatchOptions, items, match_dataset, sequence, text, text_element};
use crate::uids;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY NOT NULL,
    accession TEXT NOT NULL,
    item TEXT NOT NULL,
    status TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS items_by_accession ON items (accession);
CREATE INDEX IF NOT EXISTS items_by_expiry ON items (expires_at);
CREATE TABLE IF NOT EXISTS performed_steps (
    sop_instance_uid TEXT PRIMARY KEY NOT NULL,
    dataset BLOB NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;
";

/// A coded procedure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcedureCode {
    /// Code Value.
    pub value: String,
    /// Coding Scheme Designator.
    pub scheme: String,
    /// Code Meaning.
    pub meaning: String,
}

/// One scheduled procedure step of the worklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklistItem {
    /// Accession Number.
    pub accession_number: String,
    /// Requested Procedure ID.
    pub requested_procedure_id: String,
    /// Scheduled Procedure Step ID.
    pub scheduled_step_id: String,
    /// Patient ID.
    pub patient_id: String,
    /// Issuer of Patient ID.
    pub patient_id_issuer: Option<String>,
    /// Patient's Name as a DICOM PN (`Family^Given`).
    pub patient_name: String,
    /// Patient's Birth Date (`YYYYMMDD`).
    pub birth_date: String,
    /// Patient's Sex (`M`, `F`, `O`).
    pub sex: String,
    /// Study Instance UID of the study to acquire.
    pub study_instance_uid: String,
    /// Requested Procedure Code.
    pub procedure_code: Option<ProcedureCode>,
    /// Requested Procedure Description.
    pub procedure_description: String,
    /// Requested Procedure Priority.
    pub priority: String,
    /// Modality.
    pub modality: String,
    /// Scheduled Station AE Title.
    pub station_ae_title: String,
    /// Scheduled Station Name.
    pub station_name: String,
    /// Scheduled Procedure Step Start Date (`YYYYMMDD`).
    pub start_date: String,
    /// Scheduled Procedure Step Start Time (`HHMMSS`).
    pub start_time: String,
    /// Scheduled Performing Physician's Name.
    pub performing_physician: String,
    /// Referring Physician's Name.
    pub referring_physician: String,
    /// Placer Order Number / Imaging Service Request.
    pub placer_order_number: String,
    /// Filler Order Number / Imaging Service Request.
    pub filler_order_number: String,
    /// Scheduled Procedure Step Status.
    pub status: String,
}

/// Scheduled Procedure Step Status values.
pub(crate) mod status {
    /// Scheduled, not started.
    pub(crate) const SCHEDULED: &str = "SCHEDULED";
    /// Started (MPPS `IN PROGRESS`).
    pub(crate) const IN_PROGRESS: &str = "IN PROGRESS";
    /// Completed (MPPS `COMPLETED`).
    pub(crate) const COMPLETED: &str = "COMPLETED";
    /// Discontinued by MPPS or cancelled by the order placer.
    pub(crate) const DISCONTINUED: &str = "DISCONTINUED";
}

impl WorklistItem {
    fn id(&self) -> String {
        format!("{}/{}", self.accession_number, self.scheduled_step_id)
    }

    /// The Modality Worklist attributes of the item (PS3.4 K.6).
    pub fn to_dataset(&self) -> InMemDicomObject {
        let mut step = InMemDicomObject::from_element_iter([
            text_element(tags::MODALITY, VR::CS, &self.modality),
            text_element(
                tags::SCHEDULED_STATION_AE_TITLE,
                VR::AE,
                &self.station_ae_title,
            ),
            text_element(
                tags::SCHEDULED_PROCEDURE_STEP_START_DATE,
                VR::DA,
                &self.start_date,
            ),
            text_element(
                tags::SCHEDULED_PROCEDURE_STEP_START_TIME,
                VR::TM,
                &self.start_time,
            ),
            text_element(
                tags::SCHEDULED_PERFORMING_PHYSICIAN_NAME,
                VR::PN,
                &self.performing_physician,
            ),
            text_element(
                tags::SCHEDULED_PROCEDURE_STEP_DESCRIPTION,
                VR::LO,
                &self.procedure_description,
            ),
            text_element(tags::SCHEDULED_STATION_NAME, VR::SH, &self.station_name),
            text_element(
                tags::SCHEDULED_PROCEDURE_STEP_ID,
                VR::SH,
                &self.scheduled_step_id,
            ),
            text_element(tags::SCHEDULED_PROCEDURE_STEP_STATUS, VR::CS, &self.status),
        ]);
        let code = |code: &ProcedureCode| {
            InMemDicomObject::from_element_iter([
                text_element(tags::CODE_VALUE, VR::SH, &code.value),
                text_element(tags::CODING_SCHEME_DESIGNATOR, VR::SH, &code.scheme),
                text_element(tags::CODE_MEANING, VR::LO, &code.meaning),
            ])
        };
        if let Some(procedure) = &self.procedure_code {
            step.put(sequence(
                tags::SCHEDULED_PROTOCOL_CODE_SEQUENCE,
                vec![code(procedure)],
            ));
        }
        let mut dataset = InMemDicomObject::from_element_iter([
            text_element(tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 192"),
            text_element(tags::ACCESSION_NUMBER, VR::SH, &self.accession_number),
            text_element(
                tags::REFERRING_PHYSICIAN_NAME,
                VR::PN,
                &self.referring_physician,
            ),
            text_element(tags::PATIENT_NAME, VR::PN, &self.patient_name),
            text_element(tags::PATIENT_ID, VR::LO, &self.patient_id),
            text_element(
                tags::ISSUER_OF_PATIENT_ID,
                VR::LO,
                self.patient_id_issuer.as_deref().unwrap_or_default(),
            ),
            text_element(tags::PATIENT_BIRTH_DATE, VR::DA, &self.birth_date),
            text_element(tags::PATIENT_SEX, VR::CS, &self.sex),
            text_element(tags::STUDY_INSTANCE_UID, VR::UI, &self.study_instance_uid),
            text_element(
                tags::REQUESTED_PROCEDURE_DESCRIPTION,
                VR::LO,
                &self.procedure_description,
            ),
            text_element(
                tags::REQUESTED_PROCEDURE_ID,
                VR::SH,
                &self.requested_procedure_id,
            ),
            text_element(tags::REQUESTED_PROCEDURE_PRIORITY, VR::SH, &self.priority),
            text_element(
                tags::PLACER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST,
                VR::LO,
                &self.placer_order_number,
            ),
            text_element(
                tags::FILLER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST,
                VR::LO,
                &self.filler_order_number,
            ),
        ]);
        if let Some(procedure) = &self.procedure_code {
            dataset.put(sequence(
                tags::REQUESTED_PROCEDURE_CODE_SEQUENCE,
                vec![code(procedure)],
            ));
        }
        dataset.put(sequence(
            tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE,
            vec![step],
        ));
        dataset
    }
}

/// The worklist and the performed procedure steps (SQLite).
pub struct WorklistStore {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for WorklistStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorklistStore").finish_non_exhaustive()
    }
}

/// The outcome of an MPPS request, with the DIMSE status to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) status: u16,
    pub(crate) comment: String,
}

impl Refusal {
    fn new(status: u16, comment: impl Into<String>) -> Self {
        Self {
            status,
            comment: comment.into(),
        }
    }
}

/// Performed Procedure Step Status (0040,0252) of an MPPS data set.
pub(crate) fn performed_status(dataset: &InMemDicomObject) -> Option<String> {
    text(dataset, tags::PERFORMED_PROCEDURE_STEP_STATUS).map(|status| status.to_ascii_uppercase())
}

impl WorklistStore {
    pub(crate) fn open(path: Option<&Path>) -> Result<Self, DicomError> {
        Ok(Self {
            connection: Mutex::new(open_database(path, SCHEMA)?),
        })
    }

    fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DicomError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| DicomError::invalid("the worklist is unavailable"))?;
        work(&mut connection).map_err(|e| DicomError::Invalid(format!("worklist: {e}")))
    }

    /// Adds or replaces an item.
    pub fn put(
        &self,
        item: &WorklistItem,
        expires_at: Timestamp,
        now: Timestamp,
    ) -> Result<(), DicomError> {
        let json = serde_json::to_string(item).map_err(|e| DicomError::Invalid(e.to_string()))?;
        self.with(|connection| {
            connection.execute(
                "INSERT INTO items (id, accession, item, status, expires_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (id) DO UPDATE SET item = excluded.item, status = excluded.status,
                    expires_at = excluded.expires_at, updated_at = excluded.updated_at",
                params![
                    item.id(),
                    item.accession_number,
                    json,
                    item.status,
                    expires_at.unix_nanos(),
                    now.unix_nanos()
                ],
            )?;
            Ok(())
        })
    }

    /// The items of an accession number.
    pub fn items(&self, accession_number: &str) -> Result<Vec<WorklistItem>, DicomError> {
        let rows: Vec<String> = self.with(|connection| {
            let mut statement =
                connection.prepare("SELECT item FROM items WHERE accession = ?1 ORDER BY id")?;
            statement
                .query_map([accession_number], |row| row.get(0))?
                .collect()
        })?;
        rows.iter()
            .map(|json| {
                serde_json::from_str(json)
                    .map_err(|e| DicomError::Invalid(format!("corrupt worklist item: {e}")))
            })
            .collect()
    }

    /// Sets the status of the items of an accession number, all of them or
    /// the one with `step_id`; returns how many changed.
    pub fn set_status(
        &self,
        accession_number: &str,
        step_id: Option<&str>,
        new_status: &str,
        now: Timestamp,
    ) -> Result<usize, DicomError> {
        let mut changed = 0;
        for mut item in self.items(accession_number)? {
            if step_id.is_some_and(|id| id != item.scheduled_step_id) || item.status == new_status {
                continue;
            }
            item.status = new_status.to_owned();
            let expires_at: i64 = self.with(|connection| {
                connection.query_row(
                    "SELECT expires_at FROM items WHERE id = ?1",
                    [item.id()],
                    |row| row.get(0),
                )
            })?;
            self.put(&item, Timestamp::from_unix_nanos(expires_at), now)?;
            changed += 1;
        }
        Ok(changed)
    }

    /// Removes the items of an accession number that `keep` rejects.
    fn remove_except(&self, accession_number: &str, keep: &[String]) -> Result<(), DicomError> {
        for item in self.items(accession_number)? {
            if !keep.contains(&item.id()) {
                let id = item.id();
                self.with(|connection| {
                    connection.execute("DELETE FROM items WHERE id = ?1", [id])
                })?;
            }
        }
        Ok(())
    }

    /// Answers a Modality Worklist C-FIND: the matching items that are
    /// scheduled or in progress and not expired, at most `max_results`.
    pub(crate) fn find(
        &self,
        identifier: &InMemDicomObject,
        now: Timestamp,
        max_results: usize,
    ) -> Result<Vec<InMemDicomObject>, DicomError> {
        let rows: Vec<String> = self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT item FROM items WHERE status IN ('SCHEDULED', 'IN PROGRESS')
                 AND expires_at >= ?1 ORDER BY id",
            )?;
            statement
                .query_map([now.unix_nanos()], |row| row.get(0))?
                .collect()
        })?;
        let mut matches = Vec::new();
        for json in rows {
            let item: WorklistItem = serde_json::from_str(&json)
                .map_err(|e| DicomError::Invalid(format!("corrupt worklist item: {e}")))?;
            if let Some(response) =
                match_dataset(identifier, &item.to_dataset(), MatchOptions::default())
            {
                matches.push(response);
                if matches.len() >= max_results {
                    break;
                }
            }
        }
        Ok(matches)
    }

    /// Deletes expired items and performed steps last changed before
    /// `before`; returns how many rows were deleted.
    pub fn prune(&self, now: Timestamp, before: Timestamp) -> Result<u64, DicomError> {
        self.with(|connection| {
            let items = connection.execute(
                "DELETE FROM items WHERE expires_at < ?1",
                [now.unix_nanos()],
            )?;
            let steps = connection.execute(
                "DELETE FROM performed_steps WHERE updated_at < ?1",
                [before.unix_nanos()],
            )?;
            Ok((items + steps) as u64)
        })
    }

    fn performed_step(&self, uid: &str) -> Result<Option<(InMemDicomObject, String)>, DicomError> {
        let row: Option<(Vec<u8>, String)> = self.with(|connection| {
            connection
                .query_row(
                    "SELECT dataset, status FROM performed_steps WHERE sop_instance_uid = ?1",
                    [uid],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
        })?;
        row.map(|(bytes, status)| {
            Ok((
                object::read_dataset(&bytes, uids::EXPLICIT_VR_LITTLE_ENDIAN)?,
                status,
            ))
        })
        .transpose()
    }

    /// Checks an N-CREATE of a performed procedure step and returns the
    /// data set to store. Nothing is saved until [`WorklistStore::save_performed_step`].
    pub(crate) fn check_create(
        &self,
        uid: &str,
        attributes: &InMemDicomObject,
    ) -> Result<InMemDicomObject, Refusal> {
        use crate::dimse::status;
        match self.performed_step(uid) {
            Ok(Some(_)) => {
                return Err(Refusal::new(
                    status::DUPLICATE_SOP_INSTANCE,
                    "the performed procedure step exists already",
                ));
            }
            Ok(None) => {}
            Err(e) => return Err(Refusal::new(status::PROCESSING_FAILURE, e.to_string())),
        }
        match performed_status(attributes).as_deref() {
            Some("IN PROGRESS") => {}
            Some(other) => {
                return Err(Refusal::new(
                    status::INVALID_ATTRIBUTE_VALUE,
                    format!("a new performed procedure step must be IN PROGRESS, not {other}"),
                ));
            }
            None => {
                return Err(Refusal::new(
                    status::MISSING_ATTRIBUTE_VALUE,
                    "Performed Procedure Step Status is missing",
                ));
            }
        }
        let mut dataset = attributes.clone();
        dataset.put(text_element(tags::SOP_CLASS_UID, VR::UI, uids::MPPS));
        dataset.put(text_element(tags::SOP_INSTANCE_UID, VR::UI, uid));
        Ok(dataset)
    }

    /// Checks an N-SET of a performed procedure step and returns the
    /// merged data set to store.
    pub(crate) fn check_set(
        &self,
        uid: &str,
        modifications: &InMemDicomObject,
    ) -> Result<InMemDicomObject, Refusal> {
        use crate::dimse::status;
        let (mut dataset, current) = match self.performed_step(uid) {
            Ok(Some(found)) => found,
            Ok(None) => {
                return Err(Refusal::new(
                    status::NO_SUCH_SOP_INSTANCE,
                    "no such performed procedure step",
                ));
            }
            Err(e) => return Err(Refusal::new(status::PROCESSING_FAILURE, e.to_string())),
        };
        if current != "IN PROGRESS" {
            return Err(Refusal::new(
                status::PROCESSING_FAILURE,
                format!("the performed procedure step is {current} and may no longer be updated"),
            ));
        }
        if let Some(new_status) = performed_status(modifications)
            && !matches!(
                new_status.as_str(),
                "IN PROGRESS" | "COMPLETED" | "DISCONTINUED"
            )
        {
            return Err(Refusal::new(
                status::INVALID_ATTRIBUTE_VALUE,
                format!("invalid Performed Procedure Step Status {new_status}"),
            ));
        }
        for element in modifications.iter() {
            dataset.put(element.clone());
        }
        Ok(dataset)
    }

    /// Saves a performed procedure step checked by `check_create` or
    /// `check_set`, and updates the status of the worklist items it
    /// references.
    pub(crate) fn save_performed_step(
        &self,
        uid: &str,
        dataset: &InMemDicomObject,
        now: Timestamp,
    ) -> Result<(), DicomError> {
        let status = performed_status(dataset).unwrap_or_default();
        let bytes = object::write_dataset(dataset, uids::EXPLICIT_VR_LITTLE_ENDIAN)?;
        self.with(|connection| {
            connection.execute(
                "INSERT INTO performed_steps (sop_instance_uid, dataset, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT (sop_instance_uid) DO UPDATE SET dataset = excluded.dataset,
                    status = excluded.status, updated_at = excluded.updated_at",
                params![uid, bytes, status, now.unix_nanos()],
            )?;
            Ok(())
        })?;
        let item_status = match status.as_str() {
            "IN PROGRESS" => self::status::IN_PROGRESS,
            "COMPLETED" => self::status::COMPLETED,
            "DISCONTINUED" => self::status::DISCONTINUED,
            _ => return Ok(()),
        };
        for reference in items(dataset, tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE) {
            let Some(accession) = text(reference, tags::ACCESSION_NUMBER) else {
                continue;
            };
            let step = text(reference, tags::SCHEDULED_PROCEDURE_STEP_ID);
            let changed = self.set_status(&accession, step.as_deref(), item_status, now)?;
            debug!(%accession, changed, status = item_status, "worklist items updated by MPPS");
        }
        Ok(())
    }
}

/// Settings of the `worklist-from-orders` step.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorklistFromOrdersSettings {
    /// Modality of the scheduled steps, for example `CT`.
    pub modality: String,
    /// Modality by test code, overriding `modality`.
    #[serde(default)]
    pub modalities: std::collections::BTreeMap<String, String>,
    /// Scheduled Station AE Title.
    #[serde(default)]
    pub station_ae_title: Option<String>,
    /// Scheduled Station AE Title by modality, overriding
    /// `station_ae_title`.
    #[serde(default)]
    pub stations: std::collections::BTreeMap<String, String>,
    /// Scheduled Station Name.
    #[serde(default)]
    pub station_name: Option<String>,
    /// How long after its scheduled start an item is offered.
    #[serde(default = "default_expire_after")]
    pub expire_after: DurationText,
    /// UTC offset in minutes of the scheduled times written to the
    /// worklist.
    #[serde(default)]
    pub utc_offset: i16,
}

fn default_expire_after() -> DurationText {
    DurationText(Duration::from_secs(7 * 86_400))
}

/// Turns normalized orders into worklist items.
#[derive(Debug, Clone)]
pub struct WorklistFromOrders {
    settings: WorklistFromOrdersSettings,
    environment: DicomEnvironment,
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn dicom_date(value: &ClinicalDateTime) -> String {
    format!(
        "{:04}{:02}{:02}",
        value.year(),
        value.month().unwrap_or(1),
        value.day().unwrap_or(1)
    )
}

fn dicom_time(value: &ClinicalDateTime) -> String {
    format!(
        "{:02}{:02}{:02}",
        value.hour().unwrap_or(0),
        value.minute().unwrap_or(0),
        value.second().unwrap_or(0)
    )
}

fn code_of(test: &CodeableConcept) -> Option<ProcedureCode> {
    let coding = test.codings.first()?;
    let scheme = match coding.system.as_deref() {
        Some("http://loinc.org") => "LN".to_owned(),
        Some("http://snomed.info/sct") => "SCT".to_owned(),
        Some(system) if !system.contains(['/', ':']) => truncate(system, 16),
        _ => "L".to_owned(),
    };
    Some(ProcedureCode {
        value: truncate(&coding.code, 16),
        scheme,
        meaning: truncate(
            coding
                .display
                .as_deref()
                .or(test.text.as_deref())
                .unwrap_or(&coding.code),
            64,
        ),
    })
}

impl WorklistFromOrders {
    fn items(&self, group: &OrderGroup, received: Timestamp) -> Result<Vec<WorklistItem>, String> {
        let order = &group.order;
        let accession = order
            .filler_id
            .as_deref()
            .or(order.placer_id.as_deref())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or("the order has neither a filler nor a placer order number")?;
        let accession = truncate(accession, 16);
        let patient = group.patient.as_ref();
        let identifier = patient.and_then(|patient| patient.identifiers.first());
        let name = patient
            .and_then(|patient| patient.name.as_ref())
            .map(|name| {
                let mut parts = vec![
                    name.family.clone().unwrap_or_default(),
                    name.given.first().cloned().unwrap_or_default(),
                    name.given.get(1).cloned().unwrap_or_default(),
                    name.prefix.clone().unwrap_or_default(),
                    name.suffix.clone().unwrap_or_default(),
                ];
                while parts.last().is_some_and(String::is_empty) {
                    parts.pop();
                }
                parts.join("^")
            })
            .unwrap_or_default();
        let start = order
            .requested_at
            .or_else(|| ClinicalDateTime::from_timestamp(received, self.settings.utc_offset))
            .ok_or("the order has no usable requested time")?;
        let priority = match order.priority {
            Some(Priority::Stat) => "STAT",
            Some(Priority::Urgent | Priority::Asap) => "HIGH",
            Some(Priority::Routine) => "ROUTINE",
            None => "",
        };
        let tests: Vec<Option<&CodeableConcept>> = if order.tests.is_empty() {
            vec![None]
        } else {
            order.tests.iter().map(Some).collect()
        };
        let several = tests.len() > 1;
        let mut items = Vec::with_capacity(tests.len());
        for (index, test) in tests.into_iter().enumerate() {
            let procedure_id = if several {
                let suffix = format!(".{}", index + 1);
                format!("{}{suffix}", truncate(&accession, 16 - suffix.len()))
            } else {
                accession.clone()
            };
            let code = test.and_then(code_of);
            let modality = code
                .as_ref()
                .and_then(|code| self.settings.modalities.get(&code.value))
                .unwrap_or(&self.settings.modality)
                .clone();
            let station = self
                .settings
                .stations
                .get(&modality)
                .or(self.settings.station_ae_title.as_ref())
                .cloned()
                .unwrap_or_default();
            items.push(WorklistItem {
                study_instance_uid: uids::derived(&format!(
                    "oxim-worklist:{accession}:{procedure_id}"
                )),
                accession_number: accession.clone(),
                requested_procedure_id: procedure_id.clone(),
                scheduled_step_id: procedure_id,
                patient_id: truncate(
                    identifier.map(|id| id.value.as_str()).unwrap_or_default(),
                    64,
                ),
                patient_id_issuer: identifier.and_then(|id| id.assigner.clone()),
                patient_name: name.clone(),
                birth_date: patient
                    .and_then(|patient| patient.birth_date.as_ref())
                    .map(dicom_date)
                    .unwrap_or_default(),
                sex: match patient.and_then(|patient| patient.sex) {
                    Some(AdministrativeSex::Male) => "M",
                    Some(AdministrativeSex::Female) => "F",
                    Some(AdministrativeSex::Other) => "O",
                    Some(AdministrativeSex::Unknown) | None => "",
                }
                .to_owned(),
                procedure_description: truncate(
                    test.and_then(|test| test.text.clone())
                        .or_else(|| code.as_ref().map(|code| code.meaning.clone()))
                        .unwrap_or_default()
                        .as_str(),
                    64,
                ),
                procedure_code: code,
                priority: priority.to_owned(),
                modality,
                station_ae_title: station,
                station_name: self.settings.station_name.clone().unwrap_or_default(),
                start_date: dicom_date(&start),
                start_time: dicom_time(&start),
                performing_physician: String::new(),
                referring_physician: String::new(),
                placer_order_number: order.placer_id.clone().unwrap_or_default(),
                filler_order_number: order.filler_id.clone().unwrap_or_default(),
                status: status::SCHEDULED.to_owned(),
            });
        }
        Ok(items)
    }

    fn expiry(&self, item: &WorklistItem, received: Timestamp) -> Timestamp {
        let start = ClinicalDateTime::parse_hl7(&format!("{}{}", item.start_date, item.start_time))
            .ok()
            .and_then(|start| start.to_timestamp(self.settings.utc_offset))
            .unwrap_or(received);
        let after = i64::try_from(self.settings.expire_after.0.as_nanos()).unwrap_or(i64::MAX);
        Timestamp::from_unix_nanos(start.unix_nanos().saturating_add(after))
    }
}

impl Transformer for WorklistFromOrders {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        const STEP: &str = "worklist-from-orders";
        let Some(ClinicalContent::Orders { groups }) = &context.clinical else {
            return Ok(());
        };
        let failure = |message: String| StepError::new(STEP, message);
        let store = self
            .environment
            .worklist()
            .map_err(|e| failure(e.to_string()))?;
        let received = context.envelope.received_at;
        for group in groups {
            let items = self
                .items(group, received)
                .map_err(|e| failure(e.to_owned()))?;
            let accession = items
                .first()
                .map(|item| item.accession_number.clone())
                .unwrap_or_default();
            match group.order.control.unwrap_or(OrderControl::New) {
                OrderControl::Cancel => {
                    store
                        .set_status(&accession, None, status::DISCONTINUED, received)
                        .map_err(|e| failure(e.to_string()))?;
                }
                control => {
                    if control == OrderControl::Replace {
                        let keep: Vec<String> = items.iter().map(WorklistItem::id).collect();
                        store
                            .remove_except(&accession, &keep)
                            .map_err(|e| failure(e.to_string()))?;
                    }
                    for item in &items {
                        store
                            .put(item, self.expiry(item, received), received)
                            .map_err(|e| failure(e.to_string()))?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Registers the `worklist-from-orders` step.
pub(crate) fn register(registry: &mut oxim_core::Registry, environment: &DicomEnvironment) {
    let environment = environment.clone();
    registry.add_transformer("worklist-from-orders", move |step: &StepConfig| {
        let settings: WorklistFromOrdersSettings =
            settings(&step.settings, "worklist-from-orders step")?;
        if settings.modality.trim().is_empty() {
            return Err(EngineError::Config(
                "worklist-from-orders step: modality must not be empty".into(),
            ));
        }
        for title in settings
            .station_ae_title
            .iter()
            .chain(settings.stations.values())
        {
            validate_ae_title("station AE title", title)?;
        }
        if !(-720..=840).contains(&settings.utc_offset) {
            return Err(EngineError::Config(
                "worklist-from-orders step: utc_offset must be minutes between -720 and 840".into(),
            ));
        }
        Ok(Arc::new(WorklistFromOrders {
            settings,
            environment: environment.clone(),
        }) as Arc<dyn Transformer>)
    });
}
