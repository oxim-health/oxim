//! QIDO-RS search and WADO-RS retrieve against a minimal local HTTP server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Recorder, Synthetic, delivery, deploy, engine_with, wait_until};
use oxim_core::DestinationConnector;
use oxim_dicom::{
    DicomEnvironment, QidoDestination, QidoSettings, WadoDestination, WadoSettings, part10,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Answers each connection with the next `(status line, content type,
/// body)` and returns the request heads.
async fn server(
    listener: TcpListener,
    replies: Vec<(&'static str, String, Vec<u8>)>,
) -> Vec<String> {
    let mut heads = Vec::new();
    for (status, content_type, body) in replies {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        let mut buffer = [0u8; 8192];
        while !data.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buffer).await.unwrap();
            data.extend_from_slice(&buffer[..n]);
        }
        heads.push(String::from_utf8_lossy(&data).into_owned());
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        stream.write_all(&response).await.unwrap();
    }
    heads
}

#[tokio::test(flavor = "multi_thread")]
async fn searches_with_qido_rs() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let study = r#"[{"0020000D":{"vr":"UI","Value":["1.2.3"]}}]"#;
    let server = tokio::spawn(server(
        listener,
        vec![
            (
                "200 OK",
                "application/dicom+json".into(),
                study.as_bytes().to_vec(),
            ),
            (
                "204 No Content",
                "application/dicom+json".into(),
                Vec::new(),
            ),
            (
                "400 Bad Request",
                "text/plain".into(),
                b"bad query".to_vec(),
            ),
        ],
    ));
    let settings: QidoSettings = serde_json::from_value(serde_json::json!({
        "url": format!("http://127.0.0.1:{port}/dicom-web"),
        "params": {"limit": "5"},
        "bearer_token": "synthetic-token",
    }))
    .unwrap();
    let qido = QidoDestination::new(settings).unwrap();
    let query = br#"{"PatientID": "SYN-0001", "StudyInstanceUID": null}"#;
    let body = qido.send(&delivery(1, query)).await.unwrap().unwrap();
    assert_eq!(body, study.as_bytes());
    assert_eq!(
        qido.send(&delivery(2, query)).await.unwrap().unwrap(),
        b"[]"
    );
    assert!(qido.send(&delivery(3, query)).await.unwrap_err().permanent);
    let heads = server.await.unwrap();
    assert!(
        heads[0].starts_with(
            "GET /dicom-web/studies?limit=5&PatientID=SYN-0001&includefield=StudyInstanceUID HTTP/1.1"
        ),
        "{}",
        heads[0]
    );
    let lower = heads[0].to_ascii_lowercase();
    assert!(
        lower.contains("accept: application/dicom+json"),
        "{}",
        heads[0]
    );
    assert!(
        lower.contains("authorization: bearer synthetic-token"),
        "{}",
        heads[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retrieves_with_wado_rs_into_a_channel() {
    let recorder = Arc::new(Recorder::default());
    let environment = DicomEnvironment::in_memory();
    let shared = environment.clone();
    let engine = engine_with(recorder.clone(), move |registry| {
        oxim_dicom::register_with(registry, &shared);
    })
    .await;
    deploy(
        &engine,
        "id: prefetch\nsource:\n  type: dicom-retrieved\n  data_type: dicom\n  settings: {inbox: web}\ndestinations:\n  - {id: out, type: recorder}\n",
    )
    .await;
    let objects = [Synthetic::ct(1).bytes(), Synthetic::ct(2).bytes()];
    let mut body = Vec::new();
    for object in &objects {
        body.extend_from_slice(b"--boundary-7\r\nContent-Type: application/dicom\r\n\r\n");
        body.extend_from_slice(object);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--boundary-7--\r\n");
    // The same answer for every attempt: the source may still be starting.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let multipart = "multipart/related; type=\"application/dicom\"; boundary=boundary-7";
    let serving = tokio::spawn(server(
        listener,
        (0..100)
            .map(|_| ("200 OK", multipart.to_owned(), body.clone()))
            .collect(),
    ));
    let wado = |port: u16| {
        let settings: WadoSettings = serde_json::from_value(serde_json::json!({
            "url": format!("http://127.0.0.1:{port}/wado"),
            "into": "web",
        }))
        .unwrap();
        WadoDestination::new(settings, environment.clone()).unwrap()
    };
    let retriever = wado(port);
    let study = br#"{"StudyInstanceUID": "1.2.826.0.1.3680043.10.1.1"}"#;
    let mut answer = None;
    for _ in 0..100 {
        match retriever.send(&delivery(1, study)).await {
            Ok(body) => {
                answer = body;
                break;
            }
            // The source may still be starting; the server answers once.
            Err(e) if e.message.contains("inbox") => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => panic!("{e}"),
        }
    }
    let json: serde_json::Value = serde_json::from_slice(&answer.unwrap()).unwrap();
    assert_eq!(json["retrieved"], 2);
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
    serving.abort();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let missing = tokio::spawn(server(
        listener,
        vec![(
            "404 Not Found",
            "text/plain".into(),
            b"no such study".to_vec(),
        )],
    ));
    assert!(
        wado(port)
            .send(&delivery(2, study))
            .await
            .unwrap_err()
            .permanent
    );
    let heads = missing.await.unwrap();
    assert!(
        heads[0].starts_with("GET /wado/studies/1.2.826.0.1.3680043.10.1.1 HTTP/1.1"),
        "{}",
        heads[0]
    );
    assert!(
        heads[0]
            .to_ascii_lowercase()
            .contains("accept: multipart/related; type=\"application/dicom\""),
        "{}",
        heads[0]
    );
    engine.shutdown().await;
}
