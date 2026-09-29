//! The instance index: the patient, study, series and instance attributes
//! of DICOM objects OXIM has stored, for C-FIND queries (`dicom-qr-scp`)
//! and storage commitment.
//!
//! Objects enter the index when a `dicom-scp` source with `index: true`
//! (or `storage_commitment: true`) stores them, or through the
//! `dicom-index` step. The index is derived data: it names the OXIM
//! message that holds each object.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dicom_core::VR;
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use oxim_core::{MessageContext, StepConfig, StepError, Transformer};
use oxim_model::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;

use crate::environment::{DicomEnvironment, open_database};
use crate::error::DicomError;
use crate::net::settings;
use crate::object::DicomObject;
use crate::query::{MatchOptions, match_dataset, text, text_element};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS instances (
    sop_instance_uid TEXT PRIMARY KEY NOT NULL,
    sop_class_uid TEXT NOT NULL,
    study_uid TEXT NOT NULL,
    series_uid TEXT NOT NULL,
    patient_id TEXT NOT NULL,
    attributes TEXT NOT NULL,
    message_id TEXT,
    indexed_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS instances_by_study ON instances (study_uid);
CREATE INDEX IF NOT EXISTS instances_by_series ON instances (series_uid);
CREATE INDEX IF NOT EXISTS instances_by_patient ON instances (patient_id);
CREATE INDEX IF NOT EXISTS instances_by_time ON instances (indexed_at);
";

/// The attributes kept per instance, by level.
const PATIENT_KEYS: &[(dicom_core::Tag, VR)] = &[
    (tags::PATIENT_NAME, VR::PN),
    (tags::PATIENT_ID, VR::LO),
    (tags::ISSUER_OF_PATIENT_ID, VR::LO),
    (tags::PATIENT_BIRTH_DATE, VR::DA),
    (tags::PATIENT_SEX, VR::CS),
];
const STUDY_KEYS: &[(dicom_core::Tag, VR)] = &[
    (tags::STUDY_DATE, VR::DA),
    (tags::STUDY_TIME, VR::TM),
    (tags::ACCESSION_NUMBER, VR::SH),
    (tags::STUDY_ID, VR::SH),
    (tags::STUDY_INSTANCE_UID, VR::UI),
    (tags::STUDY_DESCRIPTION, VR::LO),
    (tags::REFERRING_PHYSICIAN_NAME, VR::PN),
];
const SERIES_KEYS: &[(dicom_core::Tag, VR)] = &[
    (tags::MODALITY, VR::CS),
    (tags::SERIES_INSTANCE_UID, VR::UI),
    (tags::SERIES_NUMBER, VR::IS),
    (tags::SERIES_DESCRIPTION, VR::LO),
    (tags::SERIES_DATE, VR::DA),
];
const INSTANCE_KEYS: &[(dicom_core::Tag, VR)] = &[
    (tags::SOP_CLASS_UID, VR::UI),
    (tags::SOP_INSTANCE_UID, VR::UI),
    (tags::INSTANCE_NUMBER, VR::IS),
];

fn all_keys() -> impl Iterator<Item = &'static (dicom_core::Tag, VR)> {
    PATIENT_KEYS
        .iter()
        .chain(STUDY_KEYS)
        .chain(SERIES_KEYS)
        .chain(INSTANCE_KEYS)
}

/// The attributes of one indexed instance, as text by tag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexedInstance {
    /// Text values by tag (`GGGGEEEE`).
    pub attributes: BTreeMap<String, String>,
    /// The OXIM message holding the object.
    pub message_id: Option<String>,
}

fn key(tag: dicom_core::Tag) -> String {
    format!("{:04X}{:04X}", tag.group(), tag.element())
}

impl IndexedInstance {
    /// The indexed attributes of an object.
    pub fn from_object(object: &DicomObject) -> Self {
        let mut attributes = BTreeMap::new();
        for (tag, _) in all_keys() {
            if let Some(value) = text(&object.dataset, *tag) {
                attributes.insert(key(*tag), value);
            }
        }
        if !object.meta.sop_class_uid.is_empty() {
            attributes
                .entry(key(tags::SOP_CLASS_UID))
                .or_insert_with(|| object.meta.sop_class_uid.clone());
        }
        if !object.meta.sop_instance_uid.is_empty() {
            attributes
                .entry(key(tags::SOP_INSTANCE_UID))
                .or_insert_with(|| object.meta.sop_instance_uid.clone());
        }
        Self {
            attributes,
            message_id: None,
        }
    }

    fn get(&self, tag: dicom_core::Tag) -> &str {
        self.attributes.get(&key(tag)).map_or("", String::as_str)
    }

    fn put(&self, dataset: &mut InMemDicomObject, keys: &[(dicom_core::Tag, VR)]) {
        for (tag, vr) in keys {
            dataset.put(text_element(*tag, *vr, self.get(*tag)));
        }
    }
}

/// The instance index (SQLite).
pub struct IndexStore {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for IndexStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexStore").finish_non_exhaustive()
    }
}

/// The candidates of one query level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    Patient,
    Study,
    Series,
    Image,
}

impl Level {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_uppercase().as_str() {
            "PATIENT" => Some(Self::Patient),
            "STUDY" => Some(Self::Study),
            "SERIES" => Some(Self::Series),
            "IMAGE" | "INSTANCE" => Some(Self::Image),
            _ => None,
        }
    }
}

impl IndexStore {
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
            .map_err(|_| DicomError::invalid("the instance index is unavailable"))?;
        work(&mut connection).map_err(|e| DicomError::Invalid(format!("instance index: {e}")))
    }

    /// Records an instance; a later record of the same instance replaces it.
    pub fn record(&self, instance: &IndexedInstance, now: Timestamp) -> Result<(), DicomError> {
        let uid = instance.get(tags::SOP_INSTANCE_UID).to_owned();
        if uid.is_empty() {
            return Err(DicomError::invalid(
                "an instance without SOP Instance UID cannot be indexed",
            ));
        }
        let attributes = serde_json::to_string(&instance.attributes)
            .map_err(|e| DicomError::Invalid(e.to_string()))?;
        self.with(|connection| {
            connection.execute(
                "INSERT INTO instances (sop_instance_uid, sop_class_uid, study_uid, series_uid, patient_id, attributes, message_id, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (sop_instance_uid) DO UPDATE SET
                    sop_class_uid = excluded.sop_class_uid, study_uid = excluded.study_uid,
                    series_uid = excluded.series_uid, patient_id = excluded.patient_id,
                    attributes = excluded.attributes, message_id = excluded.message_id,
                    indexed_at = excluded.indexed_at",
                params![
                    uid,
                    instance.get(tags::SOP_CLASS_UID),
                    instance.get(tags::STUDY_INSTANCE_UID),
                    instance.get(tags::SERIES_INSTANCE_UID),
                    instance.get(tags::PATIENT_ID),
                    attributes,
                    instance.message_id,
                    now.unix_nanos(),
                ],
            )?;
            Ok(())
        })
    }

    /// The SOP class of an indexed instance.
    pub fn sop_class(&self, sop_instance_uid: &str) -> Result<Option<String>, DicomError> {
        self.with(|connection| {
            connection
                .query_row(
                    "SELECT sop_class_uid FROM instances WHERE sop_instance_uid = ?1",
                    [sop_instance_uid],
                    |row| row.get(0),
                )
                .optional()
        })
    }

    /// Deletes instances indexed before `before`; returns how many.
    pub fn prune(&self, before: Timestamp) -> Result<u64, DicomError> {
        self.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM instances WHERE indexed_at < ?1",
                [before.unix_nanos()],
            )? as u64)
        })
    }

    /// Instances whose unique keys equal the single values of the
    /// identifier, at most `limit` of them.
    fn candidates(
        &self,
        identifier: &InMemDicomObject,
        limit: usize,
    ) -> Result<Vec<IndexedInstance>, DicomError> {
        // Single values of unique keys narrow the candidates in SQL; the
        // matching rules then apply to every key.
        let exact = |tag| text(identifier, tag).filter(|value| !value.contains(['*', '?', '\\']));
        let mut sql = "SELECT attributes, message_id FROM instances WHERE 1 = 1".to_owned();
        let mut values: Vec<String> = Vec::new();
        for (tag, column) in [
            (tags::PATIENT_ID, "patient_id"),
            (tags::STUDY_INSTANCE_UID, "study_uid"),
            (tags::SERIES_INSTANCE_UID, "series_uid"),
            (tags::SOP_INSTANCE_UID, "sop_instance_uid"),
        ] {
            if let Some(value) = exact(tag) {
                values.push(value);
                sql.push_str(&format!(" AND {column} = ?{}", values.len()));
            }
        }
        sql.push_str(&format!(" ORDER BY indexed_at LIMIT {}", limit.max(1)));
        self.with(|connection| {
            let mut statement = connection.prepare(&sql)?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?
        .into_iter()
        .map(|(attributes, message_id)| {
            Ok(IndexedInstance {
                attributes: serde_json::from_str(&attributes)
                    .map_err(|e| DicomError::Invalid(format!("corrupt index entry: {e}")))?,
                message_id,
            })
        })
        .collect()
    }

    /// Answers a C-FIND of the Patient Root or Study Root information model
    /// at `level`: the matches, at most `max_results`.
    pub(crate) fn find(
        &self,
        level: Level,
        identifier: &InMemDicomObject,
        max_results: usize,
    ) -> Result<Vec<InMemDicomObject>, DicomError> {
        let rows = self.candidates(identifier, 100_000)?;
        // Group the instances by the unique key of the level.
        let mut groups: BTreeMap<String, Vec<&IndexedInstance>> = BTreeMap::new();
        for row in &rows {
            let unique = match level {
                Level::Patient => row.get(tags::PATIENT_ID),
                Level::Study => row.get(tags::STUDY_INSTANCE_UID),
                Level::Series => row.get(tags::SERIES_INSTANCE_UID),
                Level::Image => row.get(tags::SOP_INSTANCE_UID),
            };
            groups.entry(unique.to_owned()).or_default().push(row);
        }
        let mut matches = Vec::new();
        for members in groups.values() {
            let Some(first) = members.first() else {
                continue;
            };
            let mut candidate = InMemDicomObject::new_empty();
            first.put(&mut candidate, PATIENT_KEYS);
            let count = |tag| {
                let mut seen: Vec<&str> = members.iter().map(|row| row.get(tag)).collect();
                seen.sort_unstable();
                seen.dedup();
                seen.len().to_string()
            };
            match level {
                Level::Patient => {
                    candidate.put(text_element(
                        tags::NUMBER_OF_PATIENT_RELATED_STUDIES,
                        VR::IS,
                        &count(tags::STUDY_INSTANCE_UID),
                    ));
                    candidate.put(text_element(
                        tags::NUMBER_OF_PATIENT_RELATED_INSTANCES,
                        VR::IS,
                        &members.len().to_string(),
                    ));
                }
                Level::Study => {
                    first.put(&mut candidate, STUDY_KEYS);
                    let mut modalities: Vec<&str> = members
                        .iter()
                        .map(|row| row.get(tags::MODALITY))
                        .filter(|modality| !modality.is_empty())
                        .collect();
                    modalities.sort_unstable();
                    modalities.dedup();
                    candidate.put(text_element(
                        tags::MODALITIES_IN_STUDY,
                        VR::CS,
                        &modalities.join("\\"),
                    ));
                    candidate.put(text_element(
                        tags::NUMBER_OF_STUDY_RELATED_SERIES,
                        VR::IS,
                        &count(tags::SERIES_INSTANCE_UID),
                    ));
                    candidate.put(text_element(
                        tags::NUMBER_OF_STUDY_RELATED_INSTANCES,
                        VR::IS,
                        &members.len().to_string(),
                    ));
                }
                Level::Series => {
                    first.put(&mut candidate, STUDY_KEYS);
                    first.put(&mut candidate, SERIES_KEYS);
                    candidate.put(text_element(
                        tags::NUMBER_OF_SERIES_RELATED_INSTANCES,
                        VR::IS,
                        &members.len().to_string(),
                    ));
                }
                Level::Image => {
                    first.put(&mut candidate, STUDY_KEYS);
                    first.put(&mut candidate, SERIES_KEYS);
                    first.put(&mut candidate, INSTANCE_KEYS);
                }
            }
            if let Some(response) = match_dataset(identifier, &candidate, MatchOptions::default()) {
                matches.push(response);
                if matches.len() >= max_results {
                    break;
                }
            }
        }
        Ok(matches)
    }
}

/// Settings of the `dicom-index` step.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DicomIndexSettings {}

/// Records each DICOM message in the instance index, so `dicom-qr-scp`
/// can find it and storage commitment requests can confirm it.
#[derive(Debug, Clone)]
pub struct DicomIndex {
    environment: DicomEnvironment,
}

impl Transformer for DicomIndex {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        const STEP: &str = "dicom-index";
        let object = crate::steps::decode(context, STEP)?;
        let mut instance = IndexedInstance::from_object(&object);
        instance.message_id = Some(context.envelope.id.to_string());
        self.environment
            .index()
            .and_then(|index| index.record(&instance, context.envelope.received_at))
            .map_err(|e| StepError::new(STEP, e.to_string()))
    }
}

/// Registers the `dicom-index` step.
pub(crate) fn register(registry: &mut oxim_core::Registry, environment: &DicomEnvironment) {
    let environment = environment.clone();
    registry.add_transformer("dicom-index", move |step: &StepConfig| {
        let _settings: DicomIndexSettings = settings(&step.settings, "dicom-index step")?;
        Ok(Arc::new(DicomIndex {
            environment: environment.clone(),
        }) as Arc<dyn Transformer>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::part10::FileMeta;

    fn instance(study: &str, series: &str, instance: &str, modality: &str) -> IndexedInstance {
        let dataset = InMemDicomObject::from_element_iter([
            text_element(tags::PATIENT_ID, VR::LO, "SYN-0001"),
            text_element(tags::PATIENT_NAME, VR::PN, "SYNTHETIC^PATIENT"),
            text_element(tags::STUDY_INSTANCE_UID, VR::UI, study),
            text_element(tags::STUDY_DATE, VR::DA, "20240229"),
            text_element(tags::SERIES_INSTANCE_UID, VR::UI, series),
            text_element(tags::MODALITY, VR::CS, modality),
            text_element(tags::SOP_INSTANCE_UID, VR::UI, instance),
            text_element(tags::SOP_CLASS_UID, VR::UI, "1.2.840.10008.5.1.4.1.1.2"),
        ]);
        IndexedInstance::from_object(&DicomObject {
            meta: FileMeta::default(),
            dataset,
        })
    }

    #[test]
    fn finds_by_level() {
        let index = IndexStore::open(None).unwrap();
        let now = Timestamp::from_unix_nanos(1);
        index
            .record(&instance("1.1", "1.1.1", "1.1.1.1", "CT"), now)
            .unwrap();
        index
            .record(&instance("1.1", "1.1.1", "1.1.1.2", "CT"), now)
            .unwrap();
        index
            .record(&instance("1.1", "1.1.2", "1.1.2.1", "SR"), now)
            .unwrap();
        index
            .record(&instance("1.2", "1.2.1", "1.2.1.1", "MR"), now)
            .unwrap();

        let query = InMemDicomObject::from_element_iter([
            text_element(tags::QUERY_RETRIEVE_LEVEL, VR::CS, "STUDY"),
            text_element(tags::PATIENT_ID, VR::LO, "SYN-0001"),
            text_element(tags::STUDY_INSTANCE_UID, VR::UI, ""),
            text_element(tags::MODALITIES_IN_STUDY, VR::CS, ""),
            text_element(tags::NUMBER_OF_STUDY_RELATED_INSTANCES, VR::IS, ""),
        ]);
        let studies = index.find(Level::Study, &query, 10).unwrap();
        assert_eq!(studies.len(), 2);
        assert_eq!(
            text(&studies[0], tags::MODALITIES_IN_STUDY).as_deref(),
            Some("CT\\SR")
        );
        assert_eq!(
            text(&studies[0], tags::NUMBER_OF_STUDY_RELATED_INSTANCES).as_deref(),
            Some("3")
        );

        let query = InMemDicomObject::from_element_iter([
            text_element(tags::QUERY_RETRIEVE_LEVEL, VR::CS, "SERIES"),
            text_element(tags::STUDY_INSTANCE_UID, VR::UI, "1.1"),
            text_element(tags::MODALITY, VR::CS, "CT"),
            text_element(tags::SERIES_INSTANCE_UID, VR::UI, ""),
        ]);
        let series = index.find(Level::Series, &query, 10).unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(
            text(&series[0], tags::SERIES_INSTANCE_UID).as_deref(),
            Some("1.1.1")
        );

        assert_eq!(
            index.sop_class("1.1.1.2").unwrap().as_deref(),
            Some("1.2.840.10008.5.1.4.1.1.2")
        );
        assert!(index.sop_class("9.9").unwrap().is_none());
        assert_eq!(index.find(Level::Image, &query, 1).unwrap().len(), 1);
        assert_eq!(index.prune(Timestamp::from_unix_nanos(2)).unwrap(), 4);
    }
}
