//! The STOW-RS destination against a minimal local HTTP server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::{Synthetic, delivery};
use oxim_core::{DestinationConnector, Registry};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A received request: head (request line and headers) and body.
#[derive(Debug)]
struct Received {
    head: String,
    body: Vec<u8>,
}

/// Answers each connection with the next `(status line, body)`.
async fn server(
    listener: TcpListener,
    replies: Vec<(&'static str, &'static str)>,
) -> Vec<Received> {
    let mut received = Vec::new();
    for (status, body) in replies {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        let mut buffer = [0u8; 65536];
        let head_end = loop {
            let n = stream.read(&mut buffer).await.unwrap();
            data.extend_from_slice(&buffer[..n]);
            if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while data.len() < head_end + length {
            let n = stream.read(&mut buffer).await.unwrap();
            data.extend_from_slice(&buffer[..n]);
        }
        received.push(Received {
            head,
            body: data[head_end..head_end + length].to_vec(),
        });
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/dicom+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }
    received
}

fn destination(settings: serde_json::Value) -> Arc<dyn DestinationConnector> {
    let config = oxim_core::ChannelConfig::from_yaml(&format!(
        "id: imaging\nsource: {{type: dicom-scp, data_type: dicom, settings: {{listen: '127.0.0.1:0'}}}}\ndestinations:\n  - id: out\n    type: dicomweb-stow\n    settings: {settings}\n"
    ))
    .unwrap();
    let mut registry = Registry::new();
    oxim_dicom::register(&mut registry);
    registry.destination(&config.destinations[0]).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn stores_instances_with_stow_rs() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let failed = r#"{"00081198":{"vr":"SQ","Value":[{"00081197":{"vr":"US","Value":[49152]}}]}}"#;
    let server = tokio::spawn(server(
        listener,
        vec![
            ("200 OK", r#"{"00081199":{"vr":"SQ","Value":[{}]}}"#),
            ("202 Accepted", failed),
            ("503 Service Unavailable", "busy"),
            ("401 Unauthorized", ""),
        ],
    ));
    let stow = destination(serde_json::json!({
        "url": format!("http://127.0.0.1:{port}/dicom-web/"),
        "bearer_token": "synthetic-token",
        "headers": {"X-Site": "lab"},
    }));
    let object = Synthetic::ct(1).bytes();

    let response = stow.send(&delivery(1, &object)).await.unwrap().unwrap();
    assert!(String::from_utf8(response).unwrap().contains("00081199"));
    let error = stow.send(&delivery(2, &object)).await.unwrap_err();
    assert!(error.permanent, "{error}");
    assert!(error.message.contains("C000"), "{error}");
    assert!(
        !stow
            .send(&delivery(3, &object))
            .await
            .unwrap_err()
            .permanent
    );
    assert!(
        stow.send(&delivery(4, &object))
            .await
            .unwrap_err()
            .permanent
    );

    let received = server.await.unwrap();
    let first = &received[0];
    let head = first.head.to_ascii_lowercase();
    assert!(
        head.starts_with("post /dicom-web/studies http/1.1"),
        "{head}"
    );
    assert!(
        head.contains("authorization: bearer synthetic-token"),
        "{head}"
    );
    assert!(head.contains("accept: application/dicom+json"), "{head}");
    assert!(head.contains("x-site: lab"), "{head}");
    let content_type = head
        .lines()
        .find_map(|line| line.strip_prefix("content-type: "))
        .unwrap();
    assert!(
        content_type.starts_with("multipart/related; type=\"application/dicom\"; boundary="),
        "{content_type}"
    );
    let boundary = &first.head[first.head.to_ascii_lowercase().find("boundary=").unwrap() + 9..]
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let mut expected =
        format!("--{boundary}\r\nContent-Type: application/dicom\r\n\r\n").into_bytes();
    expected.extend_from_slice(&object);
    expected.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    assert_eq!(first.body, expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_when_the_server_is_down() {
    let port = common::free_port();
    let stow = destination(serde_json::json!({"url": format!("http://127.0.0.1:{port}")}));
    let error = stow
        .send(&delivery(1, &Synthetic::ct(1).bytes()))
        .await
        .unwrap_err();
    assert!(!error.permanent, "{error}");
}
