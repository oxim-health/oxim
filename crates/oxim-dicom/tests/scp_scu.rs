//! Storage SCP and SCU over loopback, through an engine with an in-memory
//! SQLite store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    CT_IMAGE_STORAGE, Recorder, Synthetic, delivery, deploy, engine, free_port, messages,
    scripted_scp, scu, store_when_ready, text, wait_until,
};
use oxim_core::DestinationConnector;
use oxim_dicom::dimse::element;
use oxim_dicom::part10::{self, FileMeta};
use oxim_dicom::{DicomError, DicomObject, DicomScu, uids};

fn scp_channel(id: &str, port: u16, settings: &str, destination: &str) -> String {
    format!(
        "id: {id}\nsource:\n  type: dicom-scp\n  data_type: dicom\n  settings:\n    listen: 127.0.0.1:{port}\n{settings}destinations:\n{destination}"
    )
}

const RECORDER: &str = "  - id: out\n    type: recorder\n";

async fn echo_when_ready(scu: &DicomScu) {
    for _ in 0..500 {
        if scu.echo().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the SCP never answered C-ECHO");
}

#[tokio::test(flavor = "multi_thread")]
async fn stores_objects_durably_and_answers_echo() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    deploy(
        &engine,
        &scp_channel(
            "ct",
            port,
            "    ae_title: OXIM\n    calling_ae_titles: [MODALITY]\n",
            RECORDER,
        ),
    )
    .await;
    let modality = scu(port, "MODALITY", "OXIM");
    let object = Synthetic::ct(1);
    let bytes = object.bytes();
    let response = store_when_ready(&modality, &bytes).await;
    assert_eq!(response.status, 0x0000);
    assert_eq!(response.transfer_syntax, uids::EXPLICIT_VR_LITTLE_ENDIAN);
    modality.echo().await.unwrap();

    wait_until("the object is delivered", || recorder.payloads().len() == 1).await;
    let payload = &recorder.payloads()[0];
    let received = part10::parse(payload).unwrap();
    let sent = part10::parse(&bytes).unwrap();
    // The data set is stored exactly as received.
    assert_eq!(received.dataset, sent.dataset);
    assert_eq!(received.meta.sop_class_uid, CT_IMAGE_STORAGE);
    assert_eq!(received.meta.sop_instance_uid, object.instance_uid);
    assert_eq!(
        received.meta.transfer_syntax,
        uids::EXPLICIT_VR_LITTLE_ENDIAN
    );
    assert_eq!(received.meta.source_ae_title.as_deref(), Some("OXIM"));
    assert_eq!(received.meta.sending_ae_title.as_deref(), Some("MODALITY"));
    assert_eq!(received.meta.receiving_ae_title.as_deref(), Some("OXIM"));

    let records = messages(&engine).await;
    assert_eq!(records.len(), 1);
    let metadata = &records[0].metadata;
    assert_eq!(metadata["dicom.calling_ae"], "MODALITY");
    assert_eq!(metadata["dicom.called_ae"], "OXIM");
    assert_eq!(metadata["dicom.sop_instance_uid"], object.instance_uid);
    assert_eq!(metadata["dicom.study_instance_uid"], object.study_uid);
    assert_eq!(metadata["dicom.modality"], "CT");
    assert!(
        records[0]
            .peer
            .as_deref()
            .unwrap()
            .starts_with("127.0.0.1:")
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_unknown_calling_and_called_ae_titles() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    deploy(
        &engine,
        &scp_channel(
            "ct",
            port,
            "    ae_title: OXIM\n    calling_ae_titles: [MODALITY]\n",
            RECORDER,
        ),
    )
    .await;
    echo_when_ready(&scu(port, "MODALITY", "OXIM")).await;

    let intruder = scu(port, "INTRUDER", "OXIM");
    let error = intruder.echo().await.unwrap_err();
    assert!(matches!(error, DicomError::Association(_)), "{error}");
    assert!(error.to_string().contains("rejected"), "{error}");
    // A rejected association is retried: the configuration may be fixed.
    let error = intruder
        .send(&delivery(1, &Synthetic::ct(1).bytes()))
        .await
        .unwrap_err();
    assert!(!error.permanent, "{error}");

    let misaddressed = scu(port, "MODALITY", "SOMEONE-ELSE");
    assert!(misaddressed.echo().await.is_err());

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(recorder.payloads().is_empty());
    assert!(messages(&engine).await.is_empty());
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn forwards_and_converts_between_transfer_syntaxes() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let inbound = free_port();
    let archive = free_port();
    // The archive accepts Explicit VR Little Endian only.
    deploy(
        &engine,
        &scp_channel(
            "archive",
            archive,
            &format!(
                "    ae_title: ARCHIVE\n    calling_ae_titles: [OXIM]\n    transfer_syntaxes: ['{}']\n",
                uids::EXPLICIT_VR_LITTLE_ENDIAN
            ),
            RECORDER,
        ),
    )
    .await;
    deploy(
        &engine,
        &scp_channel(
            "inbound",
            inbound,
            "    ae_title: OXIM\n",
            &format!(
                "  - id: archive\n    type: dicom-scu\n    settings:\n      target: 127.0.0.1:{archive}\n      called_ae_title: ARCHIVE\n      calling_ae_title: OXIM\n"
            ),
        ),
    )
    .await;

    let object = Synthetic::ct(2).with_transfer_syntax(uids::IMPLICIT_VR_LITTLE_ENDIAN);
    let bytes = object.bytes();
    let modality = scu(inbound, "MODALITY", "OXIM");
    assert_eq!(store_when_ready(&modality, &bytes).await.status, 0);

    wait_until("the object reaches the archive", || {
        recorder.payloads().len() == 1
    })
    .await;
    let forwarded = DicomObject::parse(&recorder.payloads()[0]).unwrap();
    assert_eq!(
        forwarded.meta.transfer_syntax,
        uids::EXPLICIT_VR_LITTLE_ENDIAN
    );
    let original = DicomObject::parse(&bytes).unwrap();
    for name in [
        "SOPInstanceUID",
        "StudyInstanceUID",
        "PatientID",
        "PatientName",
        "Modality",
        "Rows",
        "ReferencedImageSequence[0].ReferencedSOPInstanceUID",
    ] {
        assert_eq!(text(&forwarded, name), text(&original, name), "{name}");
        assert!(text(&forwarded, name).is_some(), "{name}");
    }
    let pixels = |object: &DicomObject| {
        object
            .dataset
            .get(dicom_dictionary_std::tags::PIXEL_DATA)
            .unwrap()
            .to_bytes()
            .unwrap()
            .into_owned()
    };
    assert_eq!(pixels(&forwarded), pixels(&original));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn maps_c_store_statuses() {
    let statuses = vec![
        0x0000, 0xB000, 0xA700, 0xA701, 0x0110, 0xA900, 0xC000, 0x0122,
    ];
    let (port, received) = scripted_scp(CT_IMAGE_STORAGE, &[], statuses).await;
    let destination = scu(port, "OXIM", "SCRIPTED");
    let bytes = Synthetic::ct(3).bytes();
    let payload = delivery(1, &bytes);

    let body = destination.send(&payload).await.unwrap().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "0000");
    let body = destination.send(&payload).await.unwrap().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["result"], "warning");
    for expected_permanent in [false, false, false, true, true, true] {
        let error = destination.send(&payload).await.unwrap_err();
        assert_eq!(error.permanent, expected_permanent, "{error}");
        assert!(error.message.contains("scripted"), "{error}");
    }

    let received = received.lock().unwrap();
    assert_eq!(received.commands.len(), 8);
    assert_eq!(
        received.commands[0]
            .text(element::AFFECTED_SOP_INSTANCE_UID)
            .as_deref(),
        Some(Synthetic::ct(3).instance_uid.as_str())
    );
    // Sent unchanged in the object's own transfer syntax.
    assert_eq!(
        received.data_sets[0],
        part10::parse(&bytes).unwrap().dataset
    );
    assert_eq!(
        received.transfer_syntaxes[0],
        uids::EXPLICIT_VR_LITTLE_ENDIAN
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_permanently_without_a_usable_presentation_context() {
    // The receiver accepts MR images only.
    let (port, _) = scripted_scp(
        dicom_dictionary_std::uids::MR_IMAGE_STORAGE,
        &[],
        vec![0x0000],
    )
    .await;
    let error = scu(port, "OXIM", "SCRIPTED")
        .send(&delivery(1, &Synthetic::ct(4).bytes()))
        .await
        .unwrap_err();
    assert!(error.permanent, "{error}");

    // Compressed objects are not decompressed for a receiver that only
    // accepts native transfer syntaxes.
    let (port, _) = scripted_scp(
        CT_IMAGE_STORAGE,
        &[uids::EXPLICIT_VR_LITTLE_ENDIAN],
        vec![0x0000],
    )
    .await;
    let native = Synthetic::ct(5).bytes();
    let dataset = part10::parse(&native).unwrap().dataset.to_vec();
    let jpeg = part10::encode(
        &FileMeta {
            sop_class_uid: CT_IMAGE_STORAGE.into(),
            sop_instance_uid: Synthetic::ct(5).instance_uid,
            transfer_syntax: "1.2.840.10008.1.2.4.50".into(),
            ..FileMeta::default()
        },
        &dataset,
    );
    let error = scu(port, "OXIM", "SCRIPTED")
        .send(&delivery(2, &jpeg))
        .await
        .unwrap_err();
    assert!(error.permanent, "{error}");
    assert!(error.message.contains("decompress"), "{error}");

    // Nobody listening: retried later.
    let error = scu(free_port(), "OXIM", "ANY-SCP")
        .send(&delivery(3, &native))
        .await
        .unwrap_err();
    assert!(!error.permanent, "{error}");

    // Not a DICOM object at all.
    let error = scu(port, "OXIM", "SCRIPTED")
        .send(&delivery(4, b"MSH|^~\\&|"))
        .await
        .unwrap_err();
    assert!(error.permanent, "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_objects_over_the_size_limit() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    deploy(
        &engine,
        &scp_channel("ct", port, "    max_object_size: 256\n", RECORDER),
    )
    .await;
    let response =
        store_when_ready(&scu(port, "MODALITY", "OXIM"), &Synthetic::ct(6).bytes()).await;
    assert_eq!(response.status, 0xA700);
    assert!(response.comment.unwrap().contains("256"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(recorder.payloads().is_empty());
    engine.shutdown().await;
}
