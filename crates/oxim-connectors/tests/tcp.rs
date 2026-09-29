//! Raw TCP source and destination over loopback sockets.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Recorder, channel, connect, delivery, destination, engine, free_port, wait_until};
use oxim_model::DataType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread")]
async fn source_frames_messages_and_responds_after_storing() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: tcp\n  data_type: raw\n  settings:\n    listen: '127.0.0.1:{port}'\n    framing: {{mode: delimited, start: 'hex:02', end: 'hex:03'}}\n    response: 'hex:06'"
        )))
        .await
        .unwrap();
    let mut stream = connect(port).await;
    stream
        .write_all(b"noise\x02first\x03\x02second\x03")
        .await
        .unwrap();
    let mut acks = [0u8; 2];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut acks))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acks, [0x06, 0x06]);
    wait_until("both messages are delivered", || {
        recorder.payloads().len() == 2
    })
    .await;
    assert_eq!(
        recorder.payloads(),
        vec![b"first".to_vec(), b"second".to_vec()]
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn source_reads_one_message_per_connection() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: tcp\n  data_type: raw\n  settings:\n    listen: '127.0.0.1:{port}'\n    framing: {{mode: none}}\n    response: OK"
        )))
        .await
        .unwrap();
    let mut stream = connect(port).await;
    stream.write_all(b"whole ").await.unwrap();
    stream.write_all(b"message").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response, b"OK");
    wait_until("the message is delivered", || {
        recorder.payloads().len() == 1
    })
    .await;
    assert_eq!(recorder.payloads()[0], b"whole message");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_waits_for_framed_responses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        for reply in [b"OK".as_slice(), b"NO"] {
            let mut header = [0u8; 4];
            stream.read_exact(&mut header).await.unwrap();
            let mut payload = vec![0; u32::from_be_bytes(header) as usize];
            stream.read_exact(&mut payload).await.unwrap();
            received.push(payload);
            let mut response = (reply.len() as u32).to_be_bytes().to_vec();
            response.extend_from_slice(reply);
            stream.write_all(&response).await.unwrap();
        }
        received
    });
    let tcp = destination(
        "tcp",
        &format!(
            "      target: '127.0.0.1:{port}'\n      framing: {{mode: length_prefix}}\n      wait_for_response: true\n      expected_response: OK\n      response_timeout: 2s"
        ),
    );
    assert_eq!(
        tcp.send(&delivery(1, b"alpha", DataType::Raw))
            .await
            .unwrap(),
        Some(b"OK".to_vec())
    );
    let unexpected = tcp
        .send(&delivery(2, b"beta", DataType::Raw))
        .await
        .unwrap_err();
    assert!(!unexpected.permanent && unexpected.message.contains("unexpected response"));
    assert_eq!(
        server.await.unwrap(),
        vec![b"alpha".to_vec(), b"beta".to_vec()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_uses_one_connection_per_message_without_framing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let mut received = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            stream.read_to_end(&mut data).await.unwrap();
            stream.write_all(b"ACK").await.unwrap();
            received.push(data);
        }
        received
    });
    let tcp = destination(
        "tcp",
        &format!(
            "      target: '127.0.0.1:{port}'\n      framing: {{mode: none}}\n      wait_for_response: true"
        ),
    );
    for (n, payload) in [b"one".as_slice(), b"two"].into_iter().enumerate() {
        let response = tcp
            .send(&delivery(n as u64, payload, DataType::Raw))
            .await
            .unwrap();
        assert_eq!(response, Some(b"ACK".to_vec()));
    }
    assert_eq!(
        server.await.unwrap(),
        vec![b"one".to_vec(), b"two".to_vec()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_rejects_payloads_containing_the_delimiter() {
    let tcp = destination(
        "tcp",
        "      target: '127.0.0.1:9'\n      framing: {mode: delimited, end: '\\r'}",
    );
    let error = tcp
        .send(&delivery(1, b"a\rb", DataType::Raw))
        .await
        .unwrap_err();
    assert!(error.permanent);
}
