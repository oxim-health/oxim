//! The `poct1a` source driven through the engine over loopback TCP, with a
//! simulated point-of-care device.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use oxim_model::{DataType, MessageStatus};
use oxim_poct1a::builder::{self, Header};
use oxim_poct1a::{AckType, Element, Message, MessageKind, SplitEvent, Splitter, WriteOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use common::{connect, free_port, harness};

const DATETIME: &str = "2026-09-29T12:00:00+03:00";

fn header(control_id: &str) -> Header<'_> {
    Header {
        control_id,
        version_id: "POCT1",
        creation_dttm: DATETIME,
    }
}

fn device_message(message_type: &str, control_id: &str, body: Vec<Element>) -> Message {
    builder::build(
        message_type,
        &header(control_id),
        body,
        &WriteOptions::default(),
    )
    .unwrap()
}

fn hello(control_id: &str) -> Message {
    device_message(
        "HEL.R01",
        control_id,
        vec![
            Element::new("DEV")
                .child(Element::with_v("DEV.device_id", "D1"))
                .child(Element::with_v("DEV.model_id", "BG-100")),
        ],
    )
}

fn status(control_id: &str) -> Message {
    device_message(
        "DST.R01",
        control_id,
        vec![
            Element::new("DST")
                .child(Element::with_v("DST.new_observations_qty", "1"))
                .child(Element::with_v("DST.condition_cd", "R")),
        ],
    )
}

fn observation(control_id: &str) -> Message {
    device_message(
        "OBS.R01",
        control_id,
        vec![
            Element::new("SVC").child(
                Element::new("PT").child(
                    Element::new("OBS")
                        .child(Element::with_v("OBS.observation_id", "GLU"))
                        .child(Element::with_v("OBS.value", "5.4").attribute("U", "mmol/L")),
                ),
            ),
        ],
    )
}

/// A device on the other end of the connection.
struct Device {
    stream: TcpStream,
    splitter: Splitter,
    received: VecDeque<Message>,
}

impl Device {
    async fn connect(port: u16) -> Self {
        Self {
            stream: connect(port).await,
            splitter: Splitter::default(),
            received: VecDeque::new(),
        }
    }

    async fn send(&mut self, message: &Message) {
        self.stream.write_all(message.as_bytes()).await.unwrap();
    }

    /// The next message from the host, or `None` when it closed the
    /// connection.
    async fn next(&mut self) -> Option<Message> {
        let mut buffer = [0u8; 4096];
        loop {
            if let Some(message) = self.received.pop_front() {
                return Some(message);
            }
            let read = tokio::time::timeout(Duration::from_secs(5), self.stream.read(&mut buffer))
                .await
                .expect("the host did not answer")
                .unwrap();
            if read == 0 {
                return None;
            }
            self.splitter.push(&buffer[..read]);
            while let Some(event) = self.splitter.next_event() {
                if let SplitEvent::Document(bytes) = event {
                    self.received.push_back(Message::parse(&bytes).unwrap());
                }
            }
        }
    }

    /// Waits for the host's acknowledgment of `control_id`.
    async fn expect_ack(&mut self, control_id: &str) -> AckType {
        let message = self.next().await.expect("the host closed the connection");
        assert_eq!(message.kind(), MessageKind::Acknowledgment, "{message:?}");
        let info = message.ack_info().unwrap();
        assert_eq!(info.acked_control_id.as_deref(), Some(control_id));
        info.ack_type.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stores_observations_before_acknowledging_them() {
    let h = harness().await;
    h.probe.delay_ms.store(200, Ordering::SeqCst);
    let port = free_port();
    h.deploy(&format!(
        "id: poct\nsource:\n  type: poct1a\n  data_type: poct1a\n  settings:\n    listen: 127.0.0.1:{port}\n"
    ))
    .await;

    let mut device = Device::connect(port).await;
    device.send(&hello("1")).await;
    assert_eq!(device.expect_ack("1").await, AckType::Accept);

    // The status announces one observation, so the host requests it.
    device.send(&status("2")).await;
    assert_eq!(device.expect_ack("2").await, AckType::Accept);
    let request = device.next().await.unwrap();
    assert_eq!(request.kind(), MessageKind::Request);
    let request_id = request.control_id().unwrap().to_owned();
    let ack = builder::ack(&header("3"), AckType::Accept, &request_id, None).unwrap();
    device.send(&ack).await;

    let obs = observation("4");
    device.send(&obs).await;
    assert_eq!(device.expect_ack("4").await, AckType::Accept);
    let acknowledged_at = Instant::now();
    let stored_at = h.probe.stored_at.lock().unwrap()[0];
    assert!(stored_at <= acknowledged_at);

    device
        .send(&builder::end_of_topic(&header("5"), "OBS").unwrap())
        .await;
    assert_eq!(device.expect_ack("5").await, AckType::Accept);
    device
        .send(&builder::terminate(&header("6"), None).unwrap())
        .await;
    assert_eq!(device.expect_ack("6").await, AckType::Accept);
    assert!(device.next().await.is_none(), "the host closes after END");

    let records = h.wait_for("poct", 1, MessageStatus::Completed).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].data_type, DataType::Poct1a);
    assert_eq!(records[0].metadata["poct1a.device_id"], "D1");
    assert_eq!(h.raw(records[0].id).await, obs.as_bytes());
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_ae_when_storing_fails() {
    let h = harness().await;
    let port = free_port();
    h.deploy(&format!(
        "id: poct\nsource:\n  type: poct1a\n  data_type: poct1a\n  settings:\n    listen: 127.0.0.1:{port}\n    request_observations: false\n"
    ))
    .await;
    let mut device = Device::connect(port).await;
    device.send(&hello("1")).await;
    device.expect_ack("1").await;
    device.send(&status("2")).await;
    device.expect_ack("2").await;

    h.probe.fail_receives.store(1, Ordering::SeqCst);
    device.send(&observation("3")).await;
    assert_eq!(device.expect_ack("3").await, AckType::Error);
    // The device keeps the observation and sends it again.
    device.send(&observation("4")).await;
    assert_eq!(device.expect_ack("4").await, AckType::Accept);

    let records = h.wait_for("poct", 1, MessageStatus::Completed).await;
    assert_eq!(records.len(), 1);
    h.engine.shutdown().await;
}
