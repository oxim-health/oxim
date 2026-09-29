//! An engine channel that normalizes ASTM results and delivers HL7 ORU^R01.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, ConnectorError, DestinationConnector, Engine, EngineOptions, Registry,
    SendError, SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_model::MessageStatus;
use oxim_store::{Delivery, SqliteStore, Stage};
use tokio::sync::{mpsc, oneshot};

type Submission = (Vec<u8>, oneshot::Sender<oxim_model::MessageId>);

#[derive(Debug)]
struct TestSource {
    inbox: tokio::sync::Mutex<mpsc::Receiver<Submission>>,
}

#[async_trait]
impl SourceConnector for TestSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => {
                    let Some((raw, reply)) = next else { return Ok(()) };
                    let id = context.submit(raw, SubmitInfo::default()).await.map_err(|e| ConnectorError(e.to_string()))?;
                    let _ = reply.send(id);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct Recorder {
    sent: Mutex<Vec<Vec<u8>>>,
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.sent.lock().unwrap().push(delivery.payload.clone());
        Ok(None)
    }
}

const ASTM: &[u8] = b"H|\\^&|||ChemAnalyzer^2.1^SN1234\r\
P|1|PID001\r\
O|1|S123||^^^GLU\r\
R|1|^^^GLU|5.40|mmol/L|3.9-6.1|N||F\r\
L|1|N\r";

#[tokio::test(flavor = "multi_thread")]
async fn normalizes_astm_and_delivers_oru() {
    let (inbox, rx) = mpsc::channel(4);
    let source = Arc::new(TestSource {
        inbox: tokio::sync::Mutex::new(rx),
    });
    let recorder = Arc::new(Recorder::default());
    let mut registry = Registry::new();
    oxim_mapping::register(&mut registry);
    registry.add_source("test", move |_| {
        Ok(source.clone() as Arc<dyn SourceConnector>)
    });
    let target = recorder.clone();
    registry.add_destination("recorder", move |_| {
        Ok(target.clone() as Arc<dyn DestinationConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    let channel = ChannelConfig::from_yaml(
        "id: chemistry
source:
  type: test
  data_type: astm
  normalize: true
destinations:
  - id: lis
    type: recorder
    encoder:
      type: hl7v2-oru-r01
      sending_application: OXIM
      receiving_application: LIS
      utc_offset: 180
",
    )
    .unwrap();
    engine.deploy(channel).await.unwrap();

    let (reply, id) = oneshot::channel();
    inbox.send((ASTM.to_vec(), reply)).await.unwrap();
    let id = id.await.unwrap();
    for _ in 0..400 {
        let status = engine
            .store()
            .run(move |s| s.message(id))
            .await
            .unwrap()
            .unwrap()
            .status;
        if status == MessageStatus::Completed {
            break;
        }
        assert_ne!(status, MessageStatus::Error, "processing failed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let sent = recorder.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let oru = oxim_hl7::Message::parse(&sent[0]).unwrap();
    assert_eq!(oru.get("MSH-9").unwrap(), "ORU^R01^ORU_R01");
    assert_eq!(oru.get("MSH-10").unwrap().to_string_lossy(), id.to_string());
    assert_eq!(oru.get("OBX-5").unwrap(), "5.40");
    assert_eq!(oru.get("OBX-3.1").unwrap(), "GLU");
    assert_eq!(oru.get("PID-3.1").unwrap(), "PID001");

    // The normalized content is stored as a processing stage.
    let normalized = engine
        .store()
        .run(move |s| s.content(id, Stage::Normalized, None))
        .await
        .unwrap()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&normalized.data).unwrap();
    assert_eq!(json["kind"], "results");
    engine.shutdown().await;
}
