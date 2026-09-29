//! Storage commitment: OXIM as SCP committing to the objects it stored, and
//! as SCU waiting for reports on the same or on a new association.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::pacs::{Commitment, Pacs};
use common::{
    Recorder, Synthetic, delivery, deploy, engine_with, free_port, scu, store_when_ready,
    wait_until,
};
use oxim_core::DestinationConnector;
use oxim_dicom::{DicomEnvironment, DicomScu, DicomScuSettings};

fn committing_scu(
    port: u16,
    called: &str,
    environment: DicomEnvironment,
    timeout: &str,
    wait: &str,
) -> DicomScu {
    let settings: DicomScuSettings = serde_json::from_value(serde_json::json!({
        "target": format!("127.0.0.1:{port}"),
        "called_ae_title": called,
        "calling_ae_title": "MODALITY",
        "timeout": "10s",
        "storage_commitment": {"timeout": timeout, "association_wait": wait},
    }))
    .unwrap();
    DicomScu::with_environment(settings, environment).unwrap()
}

async fn scp(recorder: &Arc<Recorder>) -> (oxim_core::Engine, u16, DicomEnvironment) {
    let environment = DicomEnvironment::in_memory();
    let shared = environment.clone();
    let engine = engine_with(recorder.clone(), move |registry| {
        oxim_dicom::register_with(registry, &shared);
    })
    .await;
    let port = free_port();
    deploy(
        &engine,
        &format!(
            "id: archive\nsource:\n  type: dicom-scp\n  data_type: dicom\n  settings: {{listen: '127.0.0.1:{port}', ae_title: OXIM, storage_commitment: true}}\ndestinations:\n  - {{id: out, type: recorder}}\n"
        ),
    )
    .await;
    store_when_ready(&scu(port, "PROBE", "OXIM"), &Synthetic::ct(99).bytes()).await;
    (engine, port, environment)
}

#[tokio::test(flavor = "multi_thread")]
async fn oxim_commits_to_what_it_stored() {
    let recorder = Arc::new(Recorder::default());
    let (engine, port, environment) = scp(&recorder).await;
    let modality = committing_scu(port, "OXIM", environment, "10s", "10s");
    let object = Synthetic::ct(1);
    let body = modality
        .send(&delivery(1, &object.bytes()))
        .await
        .unwrap()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["committed"], true, "{json}");
    wait_until("both objects are delivered", || {
        recorder.payloads().len() == 2
    })
    .await;
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_and_missing_reports_fail_the_delivery() {
    let object = Synthetic::ct(1).bytes();
    let mut pacs = Pacs::new(Vec::new());
    pacs.commitment = Commitment::Fail(0x0112);
    let (port, _) = pacs.start().await;
    let error = committing_scu(port, "PACS", DicomEnvironment::in_memory(), "5s", "5s")
        .send(&delivery(1, &object))
        .await
        .unwrap_err();
    assert!(error.permanent, "{error}");
    assert!(error.message.contains("0112"), "{error}");

    let mut pacs = Pacs::new(Vec::new());
    pacs.commitment = Commitment::Fail(0x0110);
    let (port, _) = pacs.start().await;
    let error = committing_scu(port, "PACS", DicomEnvironment::in_memory(), "5s", "5s")
        .send(&delivery(2, &object))
        .await
        .unwrap_err();
    assert!(!error.permanent, "{error}");

    let mut pacs = Pacs::new(Vec::new());
    pacs.commitment = Commitment::Silent;
    let (port, _) = pacs.start().await;
    let error = committing_scu(
        port,
        "PACS",
        DicomEnvironment::in_memory(),
        "400ms",
        "100ms",
    )
    .send(&delivery(3, &object))
    .await
    .unwrap_err();
    assert!(!error.permanent);
    assert!(
        error.message.contains("no storage commitment report"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_on_a_new_association_reach_the_waiting_request() {
    let recorder = Arc::new(Recorder::default());
    // OXIM's Storage SCP receives the archive's late report.
    let (engine, oxim_port, environment) = scp(&recorder).await;
    let mut pacs = Pacs::new(Vec::new());
    pacs.commitment = Commitment::Later {
        port: oxim_port,
        ae_title: "OXIM".into(),
    };
    let (pacs_port, pacs) = pacs.start().await;
    let archive = committing_scu(pacs_port, "PACS", environment, "10s", "100ms");
    let body = archive
        .send(&delivery(1, &Synthetic::ct(1).bytes()))
        .await
        .unwrap()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["committed"], true, "{json}");
    assert_eq!(pacs.log.lock().unwrap().len(), 2);
    engine.shutdown().await;
}
