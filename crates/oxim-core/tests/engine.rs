//! End-to-end behavior of the engine with in-process test connectors.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineError, EngineOptions, Registry, SendError,
    SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_model::{ChannelId, DestinationStatus, MessageId, MessageStatus};
use oxim_store::{Delivery, MessageStore, SqliteStore, Stage};
use tokio::sync::{Mutex, mpsc, oneshot};

type Submission = (Vec<u8>, oneshot::Sender<Result<MessageId, String>>);

/// A source fed by the test through a channel.
#[derive(Debug)]
struct TestSource {
    inbox: Mutex<mpsc::Receiver<Submission>>,
}

#[async_trait]
impl SourceConnector for TestSource {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => {
                    let Some((raw, reply)) = next else { return Ok(()) };
                    let result = context.submit(raw, SubmitInfo::default()).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
            }
        }
    }
}

/// A destination that records payloads and fails the first attempts.
#[derive(Debug, Default)]
struct Recorder {
    sent: std::sync::Mutex<Vec<Vec<u8>>>,
    fail_first: AtomicU32,
    permanent: bool,
    attempts: AtomicU32,
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self
            .fail_first
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(if self.permanent {
                SendError::permanent("rejected")
            } else {
                SendError::temporary("connection refused")
            });
        }
        self.sent.lock().unwrap().push(delivery.payload.clone());
        Ok(Some(b"ACK".to_vec()))
    }
}

struct Harness {
    engine: Engine,
    inbox: mpsc::Sender<Submission>,
    recorders: std::collections::BTreeMap<String, Arc<Recorder>>,
}

fn options() -> EngineOptions {
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    options.shutdown_grace = Duration::from_secs(2);
    options.source_restart_delay = Duration::from_millis(50);
    options
}

async fn harness(store: Box<dyn MessageStore>, recorders: &[(&str, Recorder)]) -> Harness {
    let (inbox, rx) = mpsc::channel(16);
    let source = Arc::new(TestSource {
        inbox: Mutex::new(rx),
    });
    let recorders: std::collections::BTreeMap<String, Arc<Recorder>> = recorders
        .iter()
        .map(|(name, recorder)| {
            (
                (*name).to_owned(),
                Arc::new(Recorder {
                    sent: std::sync::Mutex::new(Vec::new()),
                    fail_first: AtomicU32::new(recorder.fail_first.load(Ordering::SeqCst)),
                    permanent: recorder.permanent,
                    attempts: AtomicU32::new(0),
                }),
            )
        })
        .collect();
    let mut registry = Registry::new();
    registry.add_source("test", move |_| {
        Ok(source.clone() as Arc<dyn SourceConnector>)
    });
    let lookup = recorders.clone();
    registry.add_destination("recorder", move |config| {
        let name = config.settings["name"].as_str().unwrap_or_default();
        Ok(lookup[name].clone() as Arc<dyn DestinationConnector>)
    });
    let engine = Engine::start(store, registry, Arc::new(SystemClock), options())
        .await
        .unwrap();
    Harness {
        engine,
        inbox,
        recorders,
    }
}

impl Harness {
    async fn submit(&self, raw: &[u8]) -> MessageId {
        let (reply, response) = oneshot::channel();
        self.inbox.send((raw.to_vec(), reply)).await.unwrap();
        response.await.unwrap().unwrap()
    }

    async fn status(&self, id: MessageId) -> MessageStatus {
        self.engine
            .store()
            .run(move |store| store.message(id))
            .await
            .unwrap()
            .unwrap()
            .status
    }

    async fn wait_for(&self, id: MessageId, status: MessageStatus) {
        for _ in 0..400 {
            if self.status(id).await == status {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "message {id} did not reach {status}; it is {}",
            self.status(id).await
        );
    }

    fn sent(&self, name: &str) -> Vec<Vec<u8>> {
        self.recorders[name].sent.lock().unwrap().clone()
    }
}

const ORU: &[u8] = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|1|P|2.5.1\rPID|1||42||Doe^Jane\rOBX|1|NM|GLU||5.4|mmol/L\r";
const ADT: &[u8] =
    b"MSH|^~\\&|HIS|HOSP|LIS|HOSP|20260929120000||ADT^A01|2|P|2.5.1\rPID|1||42||Doe^Jane\r";

fn channel(yaml_destinations: &str) -> ChannelConfig {
    ChannelConfig::from_yaml(&format!(
        "id: lab\nsource:\n  type: test\n  data_type: hl7v2\n{yaml_destinations}"
    ))
    .unwrap()
}

const ONE_DESTINATION: &str = "destinations:
  - id: lis
    type: recorder
    queue: {retry: {initial_delay: 10ms, max_delay: 40ms}}
    settings: {name: lis}
";

#[tokio::test(flavor = "multi_thread")]
async fn delivers_messages_unchanged() {
    let h = harness(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        &[("lis", Recorder::default())],
    )
    .await;
    h.engine.deploy(channel(ONE_DESTINATION)).await.unwrap();
    let id = h.submit(ORU).await;
    h.wait_for(id, MessageStatus::Completed).await;
    assert_eq!(h.sent("lis"), vec![ORU.to_vec()]);
    let record = h
        .engine
        .store()
        .run(move |s| s.message(id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.destinations[0].status, DestinationStatus::Sent);
    let lis = oxim_model::ConnectorId::new("lis").unwrap();
    let response = h
        .engine
        .store()
        .run(move |s| s.content(id, Stage::Response, Some(&lis)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.data, b"ACK");
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_temporary_failures() {
    let failing = Recorder {
        fail_first: AtomicU32::new(3),
        ..Recorder::default()
    };
    let h = harness(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        &[("lis", failing)],
    )
    .await;
    h.engine.deploy(channel(ONE_DESTINATION)).await.unwrap();
    let id = h.submit(ORU).await;
    h.wait_for(id, MessageStatus::Completed).await;
    assert_eq!(h.recorders["lis"].attempts.load(Ordering::SeqCst), 4);
    let record = h
        .engine
        .store()
        .run(move |s| s.message(id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.destinations[0].attempts, 4);
    assert_eq!(record.destinations[0].status, DestinationStatus::Sent);
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fails_permanent_rejections_and_gives_up() {
    let rejecting = Recorder {
        fail_first: AtomicU32::new(1),
        permanent: true,
        ..Recorder::default()
    };
    let flaky = Recorder {
        fail_first: AtomicU32::new(100),
        ..Recorder::default()
    };
    let h = harness(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        &[("lis", rejecting), ("archive", flaky)],
    )
    .await;
    let config = channel(
        "destinations:
  - id: lis
    type: recorder
    settings: {name: lis}
  - id: archive
    type: recorder
    queue: {retry: {initial_delay: 5ms, max_delay: 5ms, max_attempts: 3}}
    settings: {name: archive}
",
    );
    h.engine.deploy(config).await.unwrap();
    let id = h.submit(ORU).await;
    h.wait_for(id, MessageStatus::Completed).await;
    let record = h
        .engine
        .store()
        .run(move |s| s.message(id))
        .await
        .unwrap()
        .unwrap();
    let states: Vec<_> = record
        .destinations
        .iter()
        .map(|d| (d.destination.as_str().to_owned(), d.status, d.attempts))
        .collect();
    assert_eq!(
        states,
        [
            ("archive".to_owned(), DestinationStatus::Failed, 3),
            ("lis".to_owned(), DestinationStatus::Failed, 1),
        ]
    );
    assert!(
        record.destinations[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("gave up after 3 attempts")
    );
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_transforms_and_encodes_per_destination() {
    let h = harness(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        &[
            ("lis", Recorder::default()),
            ("archive", Recorder::default()),
        ],
    )
    .await;
    let config = channel(
        "filters:
  - {type: path-in, path: MSH-9.1, values: [ORU, ADT]}
transformers:
  - {type: set, path: MSH-5, value: OXIM}
destinations:
  - id: lis
    type: recorder
    filters:
      - {type: path-equals, path: MSH-9.1, value: ORU}
    transformers:
      - {type: copy, from: PID-3, to: PID-2}
    settings: {name: lis}
  - id: archive
    type: recorder
    settings: {name: archive}
",
    );
    h.engine.deploy(config).await.unwrap();
    let oru = h.submit(ORU).await;
    let adt = h.submit(ADT).await;
    let other = h
        .submit(&[b"MSH|^~\\&|X||||||QRY^A19|3|P|2.5\r".as_slice()].concat())
        .await;
    h.wait_for(oru, MessageStatus::Completed).await;
    h.wait_for(adt, MessageStatus::Completed).await;
    h.wait_for(other, MessageStatus::Filtered).await;

    let lis = h.sent("lis");
    assert_eq!(lis.len(), 1);
    let text = String::from_utf8(lis[0].clone()).unwrap();
    assert!(text.contains("|LAB|HOSP|OXIM|HOSP|"), "{text}");
    assert!(text.contains("PID|1|42|42||Doe^Jane"), "{text}");
    let archive = h.sent("archive");
    assert_eq!(archive.len(), 2);
    assert!(
        String::from_utf8(archive[1].clone())
            .unwrap()
            .contains("ADT^A01")
    );
    // The archive copy did not get the LIS-only transformation.
    assert!(
        String::from_utf8(archive[0].clone())
            .unwrap()
            .contains("PID|1||42||")
    );

    let record = h
        .engine
        .store()
        .run(move |s| s.message(adt))
        .await
        .unwrap()
        .unwrap();
    let lis_state = record
        .destinations
        .iter()
        .find(|d| d.destination.as_str() == "lis")
        .unwrap();
    assert_eq!(lis_state.status, DestinationStatus::Filtered);
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn records_parse_errors_without_delivering() {
    let h = harness(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        &[("lis", Recorder::default())],
    )
    .await;
    h.engine.deploy(channel(ONE_DESTINATION)).await.unwrap();
    let id = h.submit(b"not an HL7 message").await;
    h.wait_for(id, MessageStatus::Error).await;
    let record = h
        .engine
        .store()
        .run(move |s| s.message(id))
        .await
        .unwrap()
        .unwrap();
    assert!(record.error.unwrap().starts_with("parse:"));
    assert!(h.sent("lis").is_empty());
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_unknown_types_and_double_deployment() {
    let h = harness(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        &[("lis", Recorder::default())],
    )
    .await;
    let unknown =
        ChannelConfig::from_yaml("id: lab\nsource: {type: carrier-pigeon, data_type: hl7v2}\n")
            .unwrap();
    assert!(matches!(
        h.engine.deploy(unknown).await,
        Err(EngineError::UnknownType { .. })
    ));
    h.engine.deploy(channel(ONE_DESTINATION)).await.unwrap();
    assert!(matches!(
        h.engine.deploy(channel(ONE_DESTINATION)).await,
        Err(EngineError::AlreadyDeployed(_))
    ));
    assert_eq!(h.engine.deployed().await, [ChannelId::new("lab").unwrap()]);
    h.engine
        .undeploy(&ChannelId::new("lab").unwrap())
        .await
        .unwrap();
    assert!(h.engine.deployed().await.is_empty());
    h.engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resumes_delivery_after_a_restart() {
    let dir = tempfile_dir();
    let path = dir.join("oxim.db");
    let id = {
        let down = Recorder {
            fail_first: AtomicU32::new(u32::MAX),
            ..Recorder::default()
        };
        let h = harness(
            Box::new(SqliteStore::open(&path).unwrap()),
            &[("lis", down)],
        )
        .await;
        h.engine.deploy(channel(ONE_DESTINATION)).await.unwrap();
        let id = h.submit(ORU).await;
        // Wait until the message sits in the retry queue, then stop.
        for _ in 0..200 {
            if h.recorders["lis"].attempts.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        h.engine.shutdown().await;
        id
    };
    let h = harness(
        Box::new(SqliteStore::open(&path).unwrap()),
        &[("lis", Recorder::default())],
    )
    .await;
    h.engine.deploy(channel(ONE_DESTINATION)).await.unwrap();
    h.wait_for(id, MessageStatus::Completed).await;
    assert_eq!(h.sent("lis"), vec![ORU.to_vec()]);
    h.engine.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oxim-core-test-{}-{}",
        std::process::id(),
        fastrand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fastrand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
