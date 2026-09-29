//! The HTTP listener source over loopback.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Recorder, channel, delivery, destination, engine, free_port, wait_until};
use oxim_core::ChannelConfig;
use oxim_model::DataType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Sends one HTTP/1.1 request and returns the status and body.
async fn request(port: u16, head: &str, body: &[u8]) -> (u16, String) {
    let mut stream = None;
    for _ in 0..500 {
        if let Ok(connected) = TcpStream::connect(("127.0.0.1", port)).await {
            stream = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut stream = stream.expect("nothing is listening");
    let text = format!(
        "{head}\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(text.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .expect("no response")
        .unwrap();
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response[9..12].parse().unwrap();
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn stores_posted_messages() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: http
  data_type: json
  settings:
    listen: '127.0.0.1:{port}'
    path: /inbound
    max_body: 64
    status: 202
    metadata_headers: [X-Device]
    auth: {{type: bearer, token: s3cret-token}}"
        )))
        .await
        .unwrap();

    let auth = "Authorization: Bearer s3cret-token";
    let (status, body) = request(
        port,
        &format!("POST /inbound/results HTTP/1.1\r\n{auth}\r\nX-Device: chem-1\r\nContent-Type: application/json"),
        br#"{"glucose":"5.40"}"#,
    )
    .await;
    assert_eq!(status, 202, "{body}");
    assert!(body.contains("\"message_id\""), "{body}");
    wait_until("the message is stored", || recorder.payloads().len() == 1).await;
    assert_eq!(recorder.payloads()[0], br#"{"glucose":"5.40"}"#);

    let (status, _) = request(port, "POST /inbound HTTP/1.1", b"{}").await;
    assert_eq!(status, 401);
    let (status, _) = request(
        port,
        "POST /inbound HTTP/1.1\r\nAuthorization: Bearer wrong",
        b"{}",
    )
    .await;
    assert_eq!(status, 401);
    let (status, _) = request(port, &format!("GET /inbound HTTP/1.1\r\n{auth}"), b"").await;
    assert_eq!(status, 405);
    let (status, _) = request(port, &format!("POST /other HTTP/1.1\r\n{auth}"), b"{}").await;
    assert_eq!(status, 404);
    let (status, _) = request(
        port,
        &format!("POST /inbound HTTP/1.1\r\n{auth}"),
        &[b'x'; 65],
    )
    .await;
    assert_eq!(status, 413);
    assert_eq!(recorder.payloads().len(), 1);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_with_the_reply() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    let config = ChannelConfig::from_yaml(&format!(
        "id: echo
source:
  type: http
  data_type: hl7v2
  response: {{mode: pipeline, encoder: {{type: passthrough}}}}
  settings: {{listen: '127.0.0.1:{port}'}}
transformers:
  - {{type: set, path: MSH-3, value: OXIM}}
"
    ))
    .unwrap();
    engine.deploy(config).await.unwrap();
    let (status, body) = request(
        port,
        "POST / HTTP/1.1\r\nContent-Type: x-application/hl7-v2+er7",
        b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|M1|P|2.5.1\r",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.starts_with("MSH|^~\\&|OXIM|HOSP|"), "{body}");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn https_from_the_http_destination() {
    let dir = tempfile::tempdir().unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, certificate.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();
    let path = |p: &std::path::Path| p.display().to_string().replace('\\', "/");

    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: http
  data_type: json
  settings:
    listen: '127.0.0.1:{port}'
    auth: {{type: basic, username: lab, password: not-a-real-secret}}
    tls: {{cert_file: '{}', key_file: '{}'}}",
            path(&cert_path),
            path(&key_path)
        )))
        .await
        .unwrap();
    let sender = destination(
        "http",
        &format!(
            "      url: 'https://localhost:{port}/'
      ca_file: '{}'
      headers: {{Authorization: 'Basic bGFiOm5vdC1hLXJlYWwtc2VjcmV0'}}",
            path(&cert_path)
        ),
    );
    let mut answer = None;
    for _ in 0..100 {
        match sender
            .send(&delivery(1, br#"{"hello":"tls"}"#, DataType::Json))
            .await
        {
            Ok(body) => {
                answer = body;
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    let answer = String::from_utf8(answer.expect("no answer")).unwrap();
    assert!(answer.contains("message_id"), "{answer}");
    wait_until("the message is stored", || recorder.payloads().len() == 1).await;
    engine.shutdown().await;
}

#[test]
fn rejects_bad_settings() {
    let mut registry = oxim_core::Registry::new();
    oxim_connectors::register(&mut registry);
    for settings in [
        "{listen: '127.0.0.1:0', methods: []}",
        "{listen: '127.0.0.1:0', status: 404}",
        "{listen: '127.0.0.1:0', path: inbound}",
        "{listen: '127.0.0.1:0', auth: {type: digest}}",
    ] {
        let config = ChannelConfig::from_yaml(&format!(
            "id: x\nsource: {{type: http, data_type: json, settings: {settings}}}\n"
        ))
        .unwrap();
        assert!(registry.source(&config.source).is_err(), "{settings}");
    }
}
