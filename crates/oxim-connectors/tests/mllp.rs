//! MLLP source and destination over loopback sockets.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{Recorder, channel, connect, delivery, destination, engine, free_port, wait_until};
use oxim_hl7::Message;
use oxim_mllp::{Decoder, Event, encode};
use oxim_model::DataType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn message(control_id: &str, extra: &str) -> Vec<u8> {
    format!(
        "MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01^ORU_R01|{control_id}|P|2.5.1{extra}\rPID|1||42\rOBX|1|NM|GLU||5.4|mmol/L\r"
    )
    .into_bytes()
}

/// Reads one MLLP frame.
async fn read_frame(stream: &mut TcpStream, decoder: &mut Decoder) -> Vec<u8> {
    let mut buffer = [0u8; 4096];
    loop {
        if let Some(event) = decoder.next_event() {
            match event {
                Event::Frame(payload) => return payload,
                other => panic!("unexpected event {other:?}"),
            }
        }
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .expect("timed out waiting for a frame")
            .unwrap();
        assert!(n > 0, "connection closed");
        decoder.push(&buffer[..n]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn source_acknowledges_stored_messages() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: mllp\n  data_type: hl7v2\n  settings: {{listen: '127.0.0.1:{port}'}}"
        )))
        .await
        .unwrap();

    let mut stream = connect(port).await;
    let mut decoder = Decoder::default();
    let first = message("MSG1", "");
    stream.write_all(&encode(&first).unwrap()).await.unwrap();
    let ack = Message::parse(&read_frame(&mut stream, &mut decoder).await).unwrap();
    assert_eq!(ack.get("MSA-1").unwrap(), "AA");
    assert_eq!(ack.get("MSA-2").unwrap(), "MSG1");
    assert_eq!(ack.get("MSH-3").unwrap(), "LIS");
    assert_eq!(ack.get("MSH-5").unwrap(), "ANALYZER");

    // Enhanced mode with MSH-15 = NE gets no acknowledgment; the next
    // message's acknowledgment is the next frame on the wire.
    let quiet = message("MSG2", "|||NE|NE");
    let commit = message("MSG3", "|||AL|NE");
    stream
        .write_all(&[encode(&quiet).unwrap(), encode(&commit).unwrap()].concat())
        .await
        .unwrap();
    let ack = Message::parse(&read_frame(&mut stream, &mut decoder).await).unwrap();
    assert_eq!(ack.get("MSA-1").unwrap(), "CA");
    assert_eq!(ack.get("MSA-2").unwrap(), "MSG3");

    // Not HL7 at all: stored for inspection, rejected with AR.
    stream.write_all(&encode(b"hello").unwrap()).await.unwrap();
    let ack = Message::parse(&read_frame(&mut stream, &mut decoder).await).unwrap();
    assert_eq!(ack.get("MSA-1").unwrap(), "AR");

    wait_until("three messages are delivered", || {
        recorder.payloads().len() == 3
    })
    .await;
    assert_eq!(recorder.payloads(), vec![first, quiet, commit]);
    engine.shutdown().await;
}

/// A test receiver that answers each message with the next code of
/// `codes`, echoing MSH-10 in MSA-2 (or `wrong` when the code is `AA!`).
async fn receiver(listener: TcpListener, codes: Vec<&'static str>, connections: Arc<AtomicUsize>) {
    let mut codes = codes.into_iter();
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        connections.fetch_add(1, Ordering::SeqCst);
        let mut decoder = Decoder::default();
        let mut buffer = [0u8; 4096];
        while let Ok(n) = stream.read(&mut buffer).await {
            if n == 0 {
                break;
            }
            decoder.push(&buffer[..n]);
            while let Some(Event::Frame(payload)) = decoder.next_event() {
                let inbound = Message::parse(&payload).unwrap();
                let control = inbound
                    .get("MSH-10")
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                match codes.next() {
                    Some("SILENT") => {}
                    Some("CLOSE") => return,
                    Some(code) => {
                        let (code, control) = match code {
                            "AA!" => ("AA", "wrong".to_owned()),
                            other => (other, control),
                        };
                        let ack = format!(
                            "MSH|^~\\&|LIS||ANALYZER||20260929||ACK|A|P|2.5.1\rMSA|{code}|{control}|because\r"
                        );
                        stream
                            .write_all(&encode(ack.as_bytes()).unwrap())
                            .await
                            .unwrap();
                    }
                    None => return,
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_interprets_acknowledgments() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    tokio::spawn(receiver(
        listener,
        vec!["AA", "AE", "AR", "AA!", "SILENT", "AA"],
        connections.clone(),
    ));
    let mllp = destination(
        "mllp",
        &format!("      target: '127.0.0.1:{port}'\n      ack_timeout: 300ms"),
    );

    let ack = mllp
        .send(&delivery(1, &message("M1", ""), DataType::Hl7V2))
        .await
        .unwrap();
    assert!(ack.unwrap().windows(7).any(|w| w == b"MSA|AA|"));

    let error = mllp
        .send(&delivery(2, &message("M2", ""), DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(
        !error.permanent && error.message.contains("because"),
        "{error:?}"
    );

    let reject = mllp
        .send(&delivery(3, &message("M3", ""), DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(reject.permanent, "{reject:?}");

    // An acknowledgment for another message: retried on a new connection.
    let mismatch = mllp
        .send(&delivery(4, &message("M4", ""), DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(
        !mismatch.permanent && mismatch.message.contains("does not match"),
        "{mismatch:?}"
    );

    // No answer within ack_timeout: temporary error, then a fresh connection works.
    let silent = mllp
        .send(&delivery(5, &message("M5", ""), DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(
        !silent.permanent && silent.message.contains("no acknowledgment"),
        "{silent:?}"
    );
    mllp.send(&delivery(6, &message("M6", ""), DataType::Hl7V2))
        .await
        .unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_reconnects_after_the_receiver_restarts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let first = tokio::spawn(receiver(
        listener,
        vec!["AA", "CLOSE"],
        Arc::new(AtomicUsize::new(0)),
    ));
    let mllp = destination(
        "mllp",
        &format!(
            "      target: '127.0.0.1:{port}'\n      ack_timeout: 2s\n      connect_timeout: 1s"
        ),
    );
    mllp.send(&delivery(1, &message("M1", ""), DataType::Hl7V2))
        .await
        .unwrap();
    // The receiver drops the connection instead of answering and stops.
    assert!(
        mllp.send(&delivery(2, &message("M2", ""), DataType::Hl7V2))
            .await
            .is_err()
    );
    first.await.unwrap();

    // While nothing listens, attempts fail temporarily.
    let refused = mllp
        .send(&delivery(3, &message("M3", ""), DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(!refused.permanent);

    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(receiver(
        listener,
        vec!["AA"],
        Arc::new(AtomicUsize::new(0)),
    ));
    mllp.send(&delivery(4, &message("M4", ""), DataType::Hl7V2))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_without_acknowledgments() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        stream.read_to_end(&mut data).await.unwrap();
        data
    });
    let mllp = destination(
        "mllp",
        &format!("      target: '127.0.0.1:{port}'\n      ack: none"),
    );
    assert_eq!(
        mllp.send(&delivery(1, b"MSH|^~\\&|A\r", DataType::Hl7V2))
            .await
            .unwrap(),
        None
    );
    drop(mllp);
    assert_eq!(received.await.unwrap(), encode(b"MSH|^~\\&|A\r").unwrap());
}
