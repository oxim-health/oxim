//! DICOM associations over TLS with client certificates.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Pki, Recorder, Synthetic, deploy, engine, free_port, wait_until, yaml_path};
use oxim_dicom::{DicomScu, DicomScuSettings};

fn scu(port: u16, pki: &Pki, tls: bool, certificate: bool) -> DicomScu {
    let mut settings = serde_json::json!({
        "target": format!("127.0.0.1:{port}"),
        "called_ae_title": "OXIM",
        "calling_ae_title": "MODALITY",
        "connect_timeout": "5s",
        "timeout": "5s",
    });
    if tls {
        let mut tls = serde_json::json!({
            "ca_file": pki.ca,
            "system_roots": false,
            "server_name": "localhost",
        });
        if certificate {
            tls["cert_file"] = serde_json::json!(pki.client_cert);
            tls["key_file"] = serde_json::json!(pki.client_key);
        }
        settings["tls"] = tls;
    }
    let settings: DicomScuSettings = serde_json::from_value(settings).unwrap();
    DicomScu::new(settings).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn stores_over_mutual_tls() {
    let pki = Pki::new();
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    deploy(
        &engine,
        &format!(
            "id: secure\nsource:\n  type: dicom-scp\n  data_type: dicom\n  settings:\n    listen: '127.0.0.1:{port}'\n    tls: {{cert_file: {}, key_file: {}, client_ca_file: {}}}\ndestinations:\n  - {{id: out, type: recorder}}\n",
            yaml_path(&pki.server_cert),
            yaml_path(&pki.server_key),
            yaml_path(&pki.ca),
        ),
    )
    .await;
    let object = Synthetic::ct(1).bytes();
    let secure = scu(port, &pki, true, true);
    let mut stored = None;
    for _ in 0..200 {
        match secure.store(&object).await {
            Ok(response) => {
                stored = Some(response);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    assert_eq!(stored.expect("no TLS association").status, 0);
    wait_until("the object is stored", || recorder.payloads().len() == 1).await;

    // Without a client certificate, or without TLS, nothing is stored.
    assert!(scu(port, &pki, true, false).store(&object).await.is_err());
    assert!(scu(port, &pki, false, false).store(&object).await.is_err());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(recorder.payloads().len(), 1);
    engine.shutdown().await;
}
