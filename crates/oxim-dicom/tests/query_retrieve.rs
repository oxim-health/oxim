//! C-FIND against the instance index, C-MOVE into an OXIM Storage SCP and
//! C-GET into a `dicom-retrieved` channel, over loopback.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::pacs::Pacs;
use common::{
    Recorder, Synthetic, delivery, deploy, engine, engine_with, free_port, scu, store_when_ready,
    wait_until,
};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use oxim_core::{DestinationConnector, Registry};
use oxim_dicom::query::parse_identifier;
use oxim_dicom::{
    DicomEnvironment, DicomFind, DicomFindSettings, DicomRetrieve, DicomRetrieveSettings, Model,
    part10,
};

fn text(dataset: &InMemDicomObject, tag: dicom_core::Tag) -> String {
    dataset
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_end_matches(['\0', ' ']).to_owned())
        .unwrap_or_default()
}

fn find_settings(port: u16, keys: serde_json::Value, max_results: usize) -> DicomFindSettings {
    serde_json::from_value(serde_json::json!({
        "target": format!("127.0.0.1:{port}"),
        "called_ae_title": "QR",
        "keys": keys,
        "max_results": max_results,
    }))
    .unwrap()
}

async fn find_when_ready(
    find: &DicomFind,
    identifier: &InMemDicomObject,
) -> oxim_dicom::FindResult {
    for _ in 0..500 {
        if let Ok(result) = find.find(identifier).await {
            return result;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the C-FIND SCP never answered");
}

fn second_study(instance: u32) -> Synthetic {
    let mut object = Synthetic::ct(instance).with_modality("MR");
    object.study_uid = "1.2.826.0.1.3680043.10.1.2".into();
    object.series_uid = "1.2.826.0.1.3680043.10.1.2.1".into();
    object.instance_uid = format!("1.2.826.0.1.3680043.10.1.2.1.{instance}");
    object
}

#[tokio::test(flavor = "multi_thread")]
async fn finds_stored_instances() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let (storage, query) = (free_port(), free_port());
    deploy(
        &engine,
        &format!(
            "id: ct\nsource:\n  type: dicom-scp\n  data_type: dicom\n  settings: {{listen: '127.0.0.1:{storage}', index: true}}\ndestinations:\n  - {{id: out, type: recorder}}\n"
        ),
    )
    .await;
    deploy(
        &engine,
        &format!(
            "id: qr\nsource:\n  type: dicom-qr-scp\n  data_type: dicom\n  settings: {{listen: '127.0.0.1:{query}', ae_title: QR, record_queries: true}}\ndestinations:\n  - {{id: out, type: recorder}}\n"
        ),
    )
    .await;
    let modality = scu(storage, "MODALITY", "OXIM");
    for object in [Synthetic::ct(1), Synthetic::ct(2), second_study(1)] {
        assert_eq!(store_when_ready(&modality, &object.bytes()).await.status, 0);
    }

    let find = DicomFind::new(find_settings(query, serde_json::json!({}), 100)).unwrap();
    let studies = find_when_ready(
        &find,
        &parse_identifier(
            br#"{"QueryRetrieveLevel": "STUDY", "PatientID": "SYN-0001", "StudyInstanceUID": null, "ModalitiesInStudy": null, "NumberOfStudyRelatedInstances": null, "StudyDate": "20240101-20241231"}"#,
        )
        .unwrap(),
    )
    .await;
    assert_eq!(studies.status, 0);
    assert_eq!(studies.matches.len(), 2);
    let ct = studies
        .matches
        .iter()
        .find(|study| text(study, tags::MODALITIES_IN_STUDY) == "CT")
        .unwrap();
    assert_eq!(text(ct, tags::NUMBER_OF_STUDY_RELATED_INSTANCES), "2");
    assert_eq!(text(ct, tags::QUERY_RETRIEVE_LEVEL), "STUDY");

    let images = find
        .find(
            &parse_identifier(
                br#"{"QueryRetrieveLevel": "IMAGE", "StudyInstanceUID": "1.2.826.0.1.3680043.10.1.1", "SeriesInstanceUID": "1.2.826.0.1.3680043.10.1.1.1", "SOPInstanceUID": null}"#,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(images.matches.len(), 2);

    // After max_results matches the query is cancelled.
    let limited = DicomFind::new(find_settings(
        query,
        serde_json::json!({"QueryRetrieveLevel": "IMAGE", "SOPInstanceUID": null}),
        1,
    ))
    .unwrap();
    let result = limited.find(&InMemDicomObject::new_empty()).await.unwrap();
    assert!(result.truncated);
    assert_eq!(result.matches.len(), 1);

    // The destination returns the matches as DICOM JSON.
    let mut registry = Registry::new();
    oxim_dicom::register(&mut registry);
    let config = oxim_core::ChannelConfig::from_yaml(&format!(
        "id: q\nsource: {{type: dicom-scp, data_type: dicom, settings: {{listen: '127.0.0.1:0'}}}}\ndestinations:\n  - id: pacs\n    type: dicom-find\n    settings: {{target: '127.0.0.1:{query}', called_ae_title: QR, keys: {{StudyDescription: null}}}}\n"
    ))
    .unwrap();
    let destination = registry.destination(&config.destinations[0]).unwrap();
    let body = destination
        .send(&delivery(
            1,
            br#"{"QueryRetrieveLevel": "STUDY", "PatientID": "SYN-*", "StudyInstanceUID": null}"#,
        ))
        .await
        .unwrap()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["count"], 2);
    assert_eq!(
        json["matches"][0]["00081030"]["Value"][0],
        "Synthetic study"
    );

    // A Study Root query at PATIENT level does not match the model.
    let error = find
        .find(
            &parse_identifier(br#"{"QueryRetrieveLevel": "PATIENT", "PatientID": null}"#).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(error.status, 0xA900);
    // The queries were recorded.
    assert!(recorder.payloads().iter().any(|payload| {
        part10::parse(payload)
            .is_ok_and(|object| object.meta.sop_class_uid == oxim_dicom::uids::STUDY_ROOT_FIND)
    }));
    engine.shutdown().await;
}

fn retrieve_settings(port: u16, extra: serde_json::Value) -> DicomRetrieveSettings {
    let mut settings = serde_json::json!({
        "target": format!("127.0.0.1:{port}"),
        "called_ae_title": "PACS",
        "calling_ae_title": "OXIM",
        "timeout": "10s",
    });
    for (key, value) in extra.as_object().unwrap() {
        settings[key] = value.clone();
    }
    serde_json::from_value(settings).unwrap()
}

const STUDY: &[u8] =
    br#"{"QueryRetrieveLevel": "STUDY", "StudyInstanceUID": "1.2.826.0.1.3680043.10.1.1"}"#;

#[tokio::test(flavor = "multi_thread")]
async fn moves_studies_into_a_storage_scp() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let storage = free_port();
    deploy(
        &engine,
        &format!(
            "id: ct\nsource:\n  type: dicom-scp\n  data_type: dicom\n  settings: {{listen: '127.0.0.1:{storage}', ae_title: OXIM}}\ndestinations:\n  - {{id: out, type: recorder}}\n"
        ),
    )
    .await;
    // Wait until the SCP listens.
    store_when_ready(&scu(storage, "PROBE", "OXIM"), &Synthetic::ct(9).bytes()).await;
    wait_until("the probe is stored", || recorder.payloads().len() == 1).await;

    let mut pacs = Pacs::new(vec![Synthetic::ct(1).bytes(), Synthetic::ct(2).bytes()]);
    pacs.destinations.insert("OXIM".into(), storage);
    let (port, pacs) = pacs.start().await;
    let environment = DicomEnvironment::in_memory();
    let mover = DicomRetrieve::new(
        false,
        retrieve_settings(port, serde_json::json!({})),
        environment.clone(),
    )
    .unwrap();
    let body = mover.send(&delivery(1, STUDY)).await.unwrap().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "0000");
    assert_eq!(json["completed"], 2);
    wait_until("the moved instances are stored", || {
        recorder.payloads().len() == 3
    })
    .await;
    let moved = pacs
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|command| command.command_field() == Some(0x0021))
        .count();
    assert_eq!(moved, 1);

    // An unknown move destination fails the delivery for good.
    let nowhere = DicomRetrieve::new(
        false,
        retrieve_settings(port, serde_json::json!({"move_destination": "NOWHERE"})),
        environment,
    )
    .unwrap();
    let error = nowhere.send(&delivery(2, STUDY)).await.unwrap_err();
    assert!(error.permanent, "{error}");
    // Without a level the identifier is rejected.
    let error = mover
        .send(&delivery(3, br#"{"PatientID": "X"}"#))
        .await
        .unwrap_err();
    assert!(error.permanent);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gets_instances_into_a_retrieved_channel() {
    let recorder = Arc::new(Recorder::default());
    let environment = DicomEnvironment::in_memory();
    let shared = environment.clone();
    let engine = engine_with(recorder.clone(), move |registry| {
        oxim_dicom::register_with(registry, &shared);
    })
    .await;
    deploy(
        &engine,
        "id: prefetch\nsource:\n  type: dicom-retrieved\n  data_type: dicom\n  settings: {inbox: prefetch}\ndestinations:\n  - {id: out, type: recorder}\n",
    )
    .await;
    let objects = vec![Synthetic::ct(1).bytes(), Synthetic::ct(2).bytes()];
    let (port, _pacs) = Pacs::new(objects.clone()).start().await;
    let getter = DicomRetrieve::new(
        true,
        retrieve_settings(
            port,
            serde_json::json!({"into": "prefetch", "sop_classes": ["CTImageStorage"]}),
        ),
        environment.clone(),
    )
    .unwrap();
    // The source may still be starting.
    let mut body = None;
    for _ in 0..200 {
        match getter.send(&delivery(1, STUDY)).await {
            Ok(answer) => {
                body = answer;
                break;
            }
            Err(e) if !e.permanent => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(e) => panic!("{e}"),
        }
    }
    let json: serde_json::Value = serde_json::from_slice(&body.unwrap()).unwrap();
    assert_eq!(json["completed"], 2, "{json}");
    wait_until("the retrieved instances are stored", || {
        recorder.payloads().len() == 2
    })
    .await;
    for (stored, original) in recorder.payloads().iter().zip(&objects) {
        assert_eq!(
            part10::parse(stored).unwrap().dataset,
            part10::parse(original).unwrap().dataset
        );
    }

    // Without a running inbox the sub-operations fail and the delivery is
    // retried later.
    let lost = DicomRetrieve::new(
        true,
        retrieve_settings(port, serde_json::json!({"into": "nobody", "sop_classes": ["CTImageStorage"], "retry_partial": true})),
        environment,
    )
    .unwrap();
    let error = lost.send(&delivery(2, STUDY)).await.unwrap_err();
    assert!(!error.permanent, "{error}");
    engine.shutdown().await;
}

#[test]
fn validates_retrieve_settings() {
    let environment = DicomEnvironment::in_memory();
    let settings = |extra| retrieve_settings(104, extra);
    assert!(
        DicomRetrieve::new(true, settings(serde_json::json!({})), environment.clone()).is_err()
    );
    assert!(
        DicomRetrieve::new(
            false,
            settings(serde_json::json!({"into": "x"})),
            environment.clone()
        )
        .is_err()
    );
    assert!(
        DicomRetrieve::new(
            true,
            settings(serde_json::json!({"into": "x", "move_destination": "Y"})),
            environment.clone()
        )
        .is_err()
    );
    assert!(
        DicomRetrieve::new(
            false,
            settings(serde_json::json!({"model": "worklist"})),
            environment.clone()
        )
        .is_err()
    );
    assert!(
        DicomRetrieve::new(
            true,
            settings(serde_json::json!({"into": "x", "sop_classes": ["NoSuchStorage"]})),
            environment
        )
        .is_err()
    );
    let _ = Model::PatientRoot;
}
