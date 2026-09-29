//! An HL7 order becomes a worklist item, a modality finds it with C-FIND,
//! reports the procedure with MPPS and the order placer receives HL7
//! status updates.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::pacs::{encode, receive, send};
use common::{Recorder, deploy, engine_with, free_port, wait_until};
use dicom_core::value::DataSetSequence;
use dicom_core::{DataElement, PrimitiveValue, Tag, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use dicom_ul::association::client::ClientAssociationOptions;
use oxim_dicom::dimse::{Command, element, status};
use oxim_dicom::query::parse_identifier;
use oxim_dicom::{DicomFind, DicomFindSettings, uids};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn text_value(dataset: &InMemDicomObject, tag: Tag) -> String {
    dataset
        .get(tag)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_end_matches(['\0', ' ']).to_owned())
        .unwrap_or_default()
}

fn element(tag: Tag, vr: VR, value: &str) -> DataElement<InMemDicomObject> {
    DataElement::new(tag, vr, PrimitiveValue::from(value))
}

/// Sends an HL7 message over MLLP and waits for the acknowledgment.
async fn send_order(port: u16, message: &str) -> String {
    let mut stream = None;
    for _ in 0..500 {
        if let Ok(connected) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            stream = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut stream = stream.expect("the MLLP source is not listening");
    stream
        .write_all(&oxim_mllp::encode(message.as_bytes()).unwrap())
        .await
        .unwrap();
    let mut ack = Vec::new();
    let mut buffer = [0u8; 1024];
    while !ack.contains(&0x1c) {
        let n = stream.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        ack.extend_from_slice(&buffer[..n]);
    }
    String::from_utf8_lossy(&ack).into_owned()
}

fn order(control: &str, accession: &str) -> String {
    format!(
        "MSH|^~\\&|RIS|HOSP|OXIM|HOSP|20260929100000||OML^O21^OML_O21|{control}{accession}|P|2.5.1\rPID|1||SYN-0001^^^HOSP^MR||SYNTHETIC^PATIENT||19800115|O\rORC|{control}|ORD-{accession}|{accession}\rOBR|1|ORD-{accession}|{accession}|CTHEAD^CT head^L\r"
    )
}

fn worklist_query() -> InMemDicomObject {
    parse_identifier(
        br#"{"PatientID": null, "PatientName": null, "AccessionNumber": null, "StudyInstanceUID": null, "RequestedProcedureDescription": null,
            "ScheduledProcedureStepSequence": {"Modality": "CT", "ScheduledStationAETitle": "CT1", "ScheduledProcedureStepStartDate": null, "ScheduledProcedureStepID": null, "ScheduledProcedureStepStatus": null}}"#,
    )
    .unwrap()
}

/// Sends one MPPS request and returns the response.
async fn mpps(port: u16, command: Command, dataset: &InMemDicomObject) -> Command {
    let mut association = ClientAssociationOptions::new()
        .calling_ae_title("CT1")
        .called_ae_title("RIS")
        .with_presentation_context(uids::MPPS, vec![uids::EXPLICIT_VR_LITTLE_ENDIAN])
        .establish_async(("127.0.0.1", port))
        .await
        .unwrap();
    let context_id = association.presentation_contexts()[0].id;
    assert!(
        send(
            &mut association,
            context_id,
            &command,
            Some(&encode(dataset))
        )
        .await
    );
    let response = receive(&mut association).await.unwrap();
    let _ = association.release().await;
    response.command
}

fn performed_step(state: &str, study: &str) -> InMemDicomObject {
    let reference = InMemDicomObject::from_element_iter([
        element(tags::STUDY_INSTANCE_UID, VR::UI, study),
        element(tags::ACCESSION_NUMBER, VR::SH, "ACC-100"),
        element(tags::REQUESTED_PROCEDURE_ID, VR::SH, "ACC-100"),
        element(tags::SCHEDULED_PROCEDURE_STEP_ID, VR::SH, "ACC-100"),
        element(
            tags::PLACER_ORDER_NUMBER_IMAGING_SERVICE_REQUEST,
            VR::LO,
            "ORD-ACC-100",
        ),
    ]);
    let code = InMemDicomObject::from_element_iter([
        element(tags::CODE_VALUE, VR::SH, "CTHEAD"),
        element(tags::CODING_SCHEME_DESIGNATOR, VR::SH, "L"),
        element(tags::CODE_MEANING, VR::LO, "CT head"),
    ]);
    InMemDicomObject::from_element_iter([
        DataElement::new(
            tags::SCHEDULED_STEP_ATTRIBUTES_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![reference]),
        ),
        element(tags::PATIENT_NAME, VR::PN, "SYNTHETIC^PATIENT"),
        element(tags::PATIENT_ID, VR::LO, "SYN-0001"),
        element(tags::PATIENT_BIRTH_DATE, VR::DA, "19800115"),
        element(tags::PATIENT_SEX, VR::CS, "O"),
        element(tags::MODALITY, VR::CS, "CT"),
        element(tags::PERFORMED_STATION_AE_TITLE, VR::AE, "CT1"),
        element(tags::PERFORMED_PROCEDURE_STEP_ID, VR::SH, "PPS1"),
        element(
            tags::PERFORMED_PROCEDURE_STEP_START_DATE,
            VR::DA,
            "20260929",
        ),
        element(tags::PERFORMED_PROCEDURE_STEP_START_TIME, VR::TM, "101500"),
        element(tags::PERFORMED_PROCEDURE_STEP_STATUS, VR::CS, state),
        DataElement::new(
            tags::PROCEDURE_CODE_SEQUENCE,
            VR::SQ,
            DataSetSequence::from(vec![code]),
        ),
    ])
}

fn status_updates(recorder: &Recorder) -> Vec<String> {
    recorder
        .payloads()
        .iter()
        .map(|payload| String::from_utf8_lossy(payload).into_owned())
        .filter(|text| text.contains("ORM^O01"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn orders_become_worklist_items_and_mpps_updates_the_placer() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine_with(recorder.clone(), |registry| {
        oxim_connectors::register(registry);
        oxim_mapping::register(registry);
    })
    .await;
    let (orders, ris) = (free_port(), free_port());
    deploy(
        &engine,
        &format!(
            "id: orders\nsource:\n  type: mllp\n  data_type: hl7v2\n  normalize: true\n  settings: {{listen: '127.0.0.1:{orders}'}}\ntransformers:\n  - {{type: worklist-from-orders, modality: CT, station_ae_title: CT1, station_name: CT-ROOM-1}}\ndestinations:\n  - {{id: out, type: recorder}}\n"
        ),
    )
    .await;
    deploy(
        &engine,
        &format!(
            "id: ris\nsource:\n  type: dicom-mwl-scp\n  data_type: dicom\n  settings: {{listen: '127.0.0.1:{ris}', ae_title: RIS, mpps: true}}\ndestinations:\n  - id: placer\n    type: recorder\n    encoder: {{type: hl7v2-orm-status, receiving_application: HIS}}\n"
        ),
    )
    .await;
    let ack = send_order(orders, &order("NW", "ACC-100")).await;
    assert!(ack.contains("MSA|AA|"), "{ack}");

    let settings: DicomFindSettings = serde_json::from_value(serde_json::json!({
        "target": format!("127.0.0.1:{ris}"),
        "called_ae_title": "RIS",
        "calling_ae_title": "CT1",
        "model": "worklist",
    }))
    .unwrap();
    let find = DicomFind::new(settings).unwrap();
    let mut found = None;
    for _ in 0..300 {
        if let Ok(result) = find.find(&worklist_query()).await
            && !result.matches.is_empty()
        {
            found = Some(result);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let found = found.expect("the worklist item never appeared");
    assert_eq!(found.matches.len(), 1);
    let item = &found.matches[0];
    assert_eq!(text_value(item, tags::PATIENT_ID), "SYN-0001");
    assert_eq!(text_value(item, tags::PATIENT_NAME), "SYNTHETIC^PATIENT");
    assert_eq!(text_value(item, tags::ACCESSION_NUMBER), "ACC-100");
    assert_eq!(
        text_value(item, tags::REQUESTED_PROCEDURE_DESCRIPTION),
        "CT head"
    );
    let study = text_value(item, tags::STUDY_INSTANCE_UID);
    assert!(study.starts_with("2.25."), "{study}");
    let step = &item
        .get(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE)
        .unwrap()
        .value()
        .items()
        .unwrap()[0];
    assert_eq!(text_value(step, tags::MODALITY), "CT");
    assert_eq!(text_value(step, tags::SCHEDULED_STATION_AE_TITLE), "CT1");
    assert_eq!(
        text_value(step, tags::SCHEDULED_PROCEDURE_STEP_STATUS),
        "SCHEDULED"
    );
    // Another station's query finds nothing.
    let mut other = worklist_query();
    let mut sequence_item = step.clone();
    sequence_item.put(element(tags::SCHEDULED_STATION_AE_TITLE, VR::AE, "MR1"));
    other.put(DataElement::new(
        tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE,
        VR::SQ,
        DataSetSequence::from(vec![sequence_item]),
    ));
    assert!(find.find(&other).await.unwrap().matches.is_empty());

    // The modality starts the procedure.
    let pps_uid = "1.2.826.0.1.3680043.10.9.1";
    let response = mpps(
        ris,
        Command::n_create_rq(1, uids::MPPS, pps_uid),
        &performed_step("IN PROGRESS", &study),
    )
    .await;
    assert_eq!(response.status(), Some(status::SUCCESS));
    assert_eq!(
        response.text(element::AFFECTED_SOP_INSTANCE_UID).as_deref(),
        Some(pps_uid)
    );
    wait_until("the in-progress update", || {
        status_updates(&recorder).len() == 1
    })
    .await;
    let update = &status_updates(&recorder)[0];
    assert!(
        update.contains("\rORC|SC|ORD-ACC-100|ACC-100||IP|"),
        "{update}"
    );
    assert!(update.contains("|CTHEAD^CT head^L|"), "{update}");
    assert!(update.contains("MSH|^~\\&|OXIM||HIS|"), "{update}");
    let step = &find.find(&worklist_query()).await.unwrap().matches[0]
        .get(tags::SCHEDULED_PROCEDURE_STEP_SEQUENCE)
        .unwrap()
        .value()
        .items()
        .unwrap()[0]
        .clone();
    assert_eq!(
        text_value(step, tags::SCHEDULED_PROCEDURE_STEP_STATUS),
        "IN PROGRESS"
    );

    // A second N-CREATE of the same step is refused.
    let duplicate = mpps(
        ris,
        Command::n_create_rq(2, uids::MPPS, pps_uid),
        &performed_step("IN PROGRESS", &study),
    )
    .await;
    assert_eq!(duplicate.status(), Some(status::DUPLICATE_SOP_INSTANCE));

    // The modality completes it.
    let completion = InMemDicomObject::from_element_iter([
        element(tags::PERFORMED_PROCEDURE_STEP_STATUS, VR::CS, "COMPLETED"),
        element(tags::PERFORMED_PROCEDURE_STEP_END_DATE, VR::DA, "20260929"),
        element(tags::PERFORMED_PROCEDURE_STEP_END_TIME, VR::TM, "103000"),
    ]);
    let response = mpps(ris, Command::n_set_rq(3, uids::MPPS, pps_uid), &completion).await;
    assert_eq!(response.status(), Some(status::SUCCESS));
    wait_until("the completed update", || {
        status_updates(&recorder).len() == 2
    })
    .await;
    let update = &status_updates(&recorder)[1];
    assert!(update.contains("|CM||||20260929103000\r"), "{update}");
    assert!(
        update.contains("|20260929101500|20260929103000|"),
        "{update}"
    );
    // Completed steps leave the worklist and cannot change any more.
    assert!(
        find.find(&worklist_query())
            .await
            .unwrap()
            .matches
            .is_empty()
    );
    let again = mpps(ris, Command::n_set_rq(4, uids::MPPS, pps_uid), &completion).await;
    assert_eq!(again.status(), Some(status::PROCESSING_FAILURE));
    let unknown = mpps(
        ris,
        Command::n_set_rq(5, uids::MPPS, "1.2.826.0.1.3680043.10.9.99"),
        &completion,
    )
    .await;
    assert_eq!(unknown.status(), Some(status::NO_SUCH_SOP_INSTANCE));

    // A cancelled order leaves the worklist.
    send_order(orders, &order("NW", "ACC-200")).await;
    let mut listed = false;
    for _ in 0..300 {
        let matches = find.find(&worklist_query()).await.unwrap().matches;
        if matches
            .iter()
            .any(|item| text_value(item, tags::ACCESSION_NUMBER) == "ACC-200")
        {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(listed);
    send_order(orders, &order("CA", "ACC-200")).await;
    let mut removed = false;
    for _ in 0..300 {
        if find
            .find(&worklist_query())
            .await
            .unwrap()
            .matches
            .is_empty()
        {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(removed);
    engine.shutdown().await;
}
