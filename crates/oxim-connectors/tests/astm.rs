//! ASTM connectors driven through the engine over loopback TCP, with a
//! simulated analyzer on the other end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod lab_common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use oxim_astm::frame::{ACK, NAK};
use oxim_astm::session::{Event, Output, Role, Session, SessionConfig};
use oxim_model::{DestinationStatus, MessageStatus};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use lab_common::{connect, free_port, harness};

const RESULT: &[u8] =
    b"H|\\^&|||ANALYZER^1.0\rP|1||PID001\rO|1|SMP001||^^^GLU\rR|1|^^^GLU|5.4|mmol/L||N||F\rL|1|N\r";
const SECOND: &[u8] = b"H|\\^&|||ANALYZER^1.0\rP|1||PID002\rR|1|^^^HGB|13.2|g/dL\rL|1|N\r";
const WORKLIST: &[u8] = b"H|\\^&|||OXIM\rP|1||PID003\rO|1|SMP003||^^^GLU\\^^^HGB|R\rL|1|N\r";

/// An analyzer: a LIS01 session in the instrument role over TCP.
struct Analyzer {
    session: Session,
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
    log: Vec<(Instant, Output)>,
}

impl Analyzer {
    fn new(stream: TcpStream) -> Self {
        let mut config = SessionConfig::default();
        config.role = Role::Instrument;
        let (reader, writer) = stream.into_split();
        Self {
            session: Session::new(config),
            reader,
            writer,
            log: Vec::new(),
        }
    }

    fn send(&mut self, message: &[u8]) {
        self.session.send(message.to_vec(), Instant::now()).unwrap();
    }

    /// Runs the link until `done` holds for the log.
    async fn run_until(&mut self, done: impl Fn(&[(Instant, Output)]) -> bool) {
        let work = async {
            let mut buffer = [0u8; 1024];
            loop {
                while let Some(output) = self.session.poll_output() {
                    if let Output::Transmit(bytes) = &output {
                        self.writer.write_all(bytes).await.unwrap();
                    }
                    self.log.push((Instant::now(), output));
                }
                if done(&self.log) {
                    return;
                }
                let deadline = self.session.poll_timeout();
                tokio::select! {
                    read = self.reader.read(&mut buffer) => {
                        let n = read.unwrap();
                        assert!(n > 0, "the host closed the connection");
                        self.session.handle_input(&buffer[..n], Instant::now());
                    }
                    () = sleep_until(deadline) => self.session.handle_timeout(Instant::now()),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), work)
            .await
            .expect("the analyzer scenario timed out");
    }

    fn delivered(&self) -> Vec<Instant> {
        self.log
            .iter()
            .filter(|(_, o)| matches!(o, Output::Delivered(_)))
            .map(|(at, _)| *at)
            .collect()
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending().await,
    }
}

fn delivered(count: usize) -> impl Fn(&[(Instant, Output)]) -> bool {
    move |log| {
        log.iter()
            .filter(|(_, o)| matches!(o, Output::Delivered(_)))
            .count()
            >= count
    }
}

fn results_channel(port: u16) -> String {
    format!(
        "id: results\nsource:\n  type: astm-tcp\n  data_type: astm\n  settings:\n    listen: 127.0.0.1:{port}\n"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn stores_before_acknowledging_the_final_frame() {
    let h = harness().await;
    // Storing takes 300 ms, so an early acknowledgment would be visible.
    h.probe.delay_ms.store(300, Ordering::SeqCst);
    let port = free_port();
    h.deploy(&results_channel(port)).await;

    let mut analyzer = Analyzer::new(connect(port).await);
    analyzer.send(RESULT);
    analyzer.run_until(delivered(1)).await;

    let stored_at = h.probe.stored_at.lock().unwrap()[0];
    let acknowledged_at = analyzer.delivered()[0];
    assert!(
        stored_at <= acknowledged_at,
        "the final frame was acknowledged before the message was stored"
    );
    let records = h.wait_for("results", 1, MessageStatus::Completed).await;
    assert_eq!(h.raw(records[0].id).await, RESULT);
    assert!(
        records[0]
            .peer
            .as_deref()
            .unwrap()
            .starts_with("127.0.0.1:")
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_the_final_frame_when_storing_fails() {
    let h = harness().await;
    h.probe.fail_receives.store(1, Ordering::SeqCst);
    let port = free_port();
    h.deploy(&results_channel(port)).await;

    let mut analyzer = Analyzer::new(connect(port).await);
    analyzer.send(RESULT);
    analyzer.run_until(delivered(1)).await;

    assert!(
        analyzer
            .log
            .iter()
            .any(|(_, o)| matches!(o, Output::Event(Event::Retransmission { .. })))
    );
    let records = h.wait_for("results", 1, MessageStatus::Completed).await;
    assert_eq!(records.len(), 1, "the message was stored once");
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_mode_reconnects() {
    let h = harness().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    h.deploy(&format!(
        "id: results\nsource:\n  type: astm-tcp\n  data_type: astm\n  settings:\n    mode: client\n    connect: 127.0.0.1:{port}\n    reconnect_delay: 50ms\n    max_reconnect_delay: 200ms\n"
    ))
    .await;

    let (stream, _) = listener.accept().await.unwrap();
    let mut first = Analyzer::new(stream);
    first.send(RESULT);
    first.run_until(delivered(1)).await;
    drop(first);

    let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("the connector did not reconnect")
        .unwrap();
    let mut second = Analyzer::new(stream);
    second.send(SECOND);
    second.run_until(delivered(1)).await;

    let records = h.wait_for("results", 2, MessageStatus::Completed).await;
    assert_eq!(h.raw(records[0].id).await, RESULT);
    assert_eq!(h.raw(records[1].id).await, SECOND);
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn source_and_destination_share_one_link() {
    let h = harness().await;
    let port = free_port();
    h.deploy(&results_channel(port)).await;
    h.deploy(&format!(
        "id: worklists\nsource:\n  type: test\n  data_type: astm\ndestinations:\n  - id: analyzer\n    type: astm-tcp\n    queue: {{retry: {{initial_delay: 50ms, max_delay: 200ms}}}}\n    settings:\n      listen: 127.0.0.1:{port}\n"
    ))
    .await;

    let mut analyzer = Analyzer::new(connect(port).await);
    analyzer.send(RESULT);
    analyzer.run_until(delivered(1)).await;

    let worklist = h.submit(WORKLIST).await;
    analyzer
        .run_until(|log| {
            log.iter()
                .any(|(_, o)| *o == Output::Received(WORKLIST.to_vec()))
        })
        .await;

    h.wait_for("results", 1, MessageStatus::Completed).await;
    let records = h.wait_for("worklists", 1, MessageStatus::Completed).await;
    assert_eq!(records[0].id, worklist);
    assert_eq!(records[0].destinations[0].status, DestinationStatus::Sent);
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_link_with_other_settings_is_refused() {
    let h = harness().await;
    let port = free_port();
    h.deploy(&results_channel(port)).await;
    let conflicting = format!(
        "id: worklists\nsource:\n  type: test\n  data_type: astm\ndestinations:\n  - id: analyzer\n    type: astm-tcp\n    settings:\n      listen: 127.0.0.1:{port}\n      role: instrument\n"
    );
    let error = h
        .engine
        .deploy(oxim_core::ChannelConfig::from_yaml(&conflicting).unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different settings"), "{error}");
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_tcp_acknowledges_after_storing() {
    let h = harness().await;
    let port = free_port();
    h.deploy(&format!(
        "id: raw\nsource:\n  type: astm-raw-tcp\n  data_type: astm\n  settings:\n    listen: 127.0.0.1:{port}\n    acknowledge: true\n"
    ))
    .await;
    let mut stream = connect(port).await;
    let mut reply = [0u8; 1];

    stream.write_all(RESULT).await.unwrap();
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[0], ACK);

    h.probe.fail_receives.store(1, Ordering::SeqCst);
    stream.write_all(SECOND).await.unwrap();
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[0], NAK);

    let records = h.wait_for("raw", 1, MessageStatus::Completed).await;
    assert_eq!(records.len(), 1);
    assert_eq!(h.raw(records[0].id).await, RESULT);
    h.engine.shutdown().await;
}
