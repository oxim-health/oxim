//! Sources that answer their senders: replies produced by the pipeline or
//! relayed from a destination.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Encoded, Encoder, Engine, EngineOptions, MessageContext,
    Registry, Reply, SendError, SourceConnector, SourceContext, StepError, SubmitInfo, SystemClock,
    Transformer, async_trait,
};
use oxim_model::{ConnectorId, DataType, MessageStatus};
use oxim_store::{Delivery, SqliteStore, Stage};
use tokio::sync::{Mutex, mpsc, oneshot};

type Request = (Vec<u8>, oneshot::Sender<Reply>);

#[derive(Debug)]
struct RequestSource {
    inbox: Mutex<mpsc::Receiver<Request>>,
}

#[async_trait]
impl SourceConnector for RequestSource {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => {
                    let Some((raw, reply)) = next else { return Ok(()) };
                    assert!(context.responds());
                    let answer = context.request(raw, SubmitInfo::default()).await.unwrap();
                    let _ = reply.send(answer);
                }
            }
        }
    }
}

/// Answers with the control ID of the HL7 message.
#[derive(Debug)]
struct EchoReply;

impl Transformer for EchoReply {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let control = context.document.get("MSH-10")?.unwrap_or_default();
        context.response = Some(Encoded {
            data_type: DataType::Raw,
            data: format!("REPLY:{control}").into_bytes(),
        });
        Ok(())
    }
}

/// An encoder for content that never arrives.
#[derive(Debug)]
struct Picky;

impl Encoder for Picky {
    fn encode(&self, _context: &MessageContext) -> Result<Encoded, StepError> {
        Err(StepError::new("picky", "cannot encode this"))
    }

    fn handles(&self, _context: &MessageContext) -> bool {
        false
    }
}

/// A destination that answers `ANSWER`, or keeps failing when told to.
#[derive(Debug, Default)]
struct Answering {
    down: AtomicBool,
}

#[async_trait]
impl DestinationConnector for Answering {
    async fn send(&self, _delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        if self.down.load(Ordering::SeqCst) {
            Err(SendError::temporary("connection refused"))
        } else {
            Ok(Some(b"ANSWER".to_vec()))
        }
    }
}

async fn engine(destination: Arc<Answering>) -> (Engine, mpsc::Sender<Request>) {
    let (inbox, rx) = mpsc::channel(8);
    let source = Arc::new(RequestSource {
        inbox: Mutex::new(rx),
    });
    let mut registry = Registry::new();
    registry
        .add_source("request", move |_| {
            Ok(source.clone() as Arc<dyn SourceConnector>)
        })
        .add_destination("answering", move |_| {
            Ok(destination.clone() as Arc<dyn DestinationConnector>)
        })
        .add_transformer("echo-reply", |_| {
            Ok(Arc::new(EchoReply) as Arc<dyn Transformer>)
        })
        .add_encoder("picky", |_| Ok(Arc::new(Picky) as Arc<dyn Encoder>));
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    options.shutdown_grace = Duration::from_secs(2);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    (engine, inbox)
}

async fn ask(inbox: &mpsc::Sender<Request>, raw: &[u8]) -> Reply {
    let (reply, answer) = oneshot::channel();
    inbox.send((raw.to_vec(), reply)).await.unwrap();
    answer.await.unwrap()
}

const QUERY: &[u8] = b"MSH|^~\\&|ANALYZER|LAB|OXIM|LAB|20260929120000||QRY^Q02|Q123|P|2.5\r";

#[tokio::test(flavor = "multi_thread")]
async fn pipeline_steps_produce_replies() {
    let (engine, inbox) = engine(Arc::new(Answering::default())).await;
    let config = ChannelConfig::from_yaml(
        "id: queries
source:
  type: request
  data_type: hl7v2
  response: {mode: pipeline}
transformers:
  - {type: echo-reply}
",
    )
    .unwrap();
    engine.deploy(config).await.unwrap();

    let reply = ask(&inbox, QUERY).await;
    assert_eq!(reply.data.as_deref(), Some(&b"REPLY:Q123"[..]));
    assert_eq!(reply.data_type, Some(DataType::Raw));
    assert_eq!(reply.status, MessageStatus::Completed);
    let id = reply.message_id;
    let stored = engine
        .store()
        .run(move |store| store.content(id, Stage::Reply, None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.data, b"REPLY:Q123");

    // A message that cannot be parsed is stored and answered without data.
    let reply = ask(&inbox, b"garbage").await;
    assert_eq!(reply.status, MessageStatus::Error);
    assert!(reply.data.is_none());
    assert!(reply.error.unwrap().starts_with("parse:"));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_responses_are_relayed() {
    let lis = Arc::new(Answering::default());
    let (engine, inbox) = engine(lis.clone()).await;
    let config = ChannelConfig::from_yaml(
        "id: relay
source:
  type: request
  data_type: hl7v2
  response: {mode: destination, destination: lis, timeout: 300ms}
destinations:
  - id: lis
    type: answering
    queue: {retry: {initial_delay: 50ms, max_delay: 50ms}}
",
    )
    .unwrap();
    engine.deploy(config).await.unwrap();

    let reply = ask(&inbox, QUERY).await;
    assert_eq!(reply.data.as_deref(), Some(&b"ANSWER"[..]));
    assert!(reply.error.is_none());

    // While the LIS is down the sender gets no reply in time, but the
    // message stays queued and is delivered once the LIS is back.
    lis.down.store(true, Ordering::SeqCst);
    let reply = ask(&inbox, QUERY).await;
    assert!(reply.data.is_none());
    assert!(reply.error.unwrap().contains("did not answer within 300ms"));
    let id = reply.message_id;
    lis.down.store(false, Ordering::SeqCst);
    for _ in 0..200 {
        let record = engine
            .store()
            .run(move |s| s.message(id))
            .await
            .unwrap()
            .unwrap();
        if record.status == MessageStatus::Completed {
            let lis_id = ConnectorId::new("lis").unwrap();
            let response = engine
                .store()
                .run(move |s| s.content(id, Stage::Response, Some(&lis_id)))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.data, b"ANSWER");
            engine.shutdown().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the queued message was not delivered after the LIS came back");
}

#[test]
fn response_configuration_is_validated() {
    for yaml in [
        "id: a\nsource: {type: request, data_type: hl7v2, response: {mode: destination}}\n",
        "id: a\nsource: {type: request, data_type: hl7v2, response: {mode: destination, destination: nowhere}}\n",
        "id: a\nsource: {type: request, data_type: hl7v2, response: {mode: pipeline, destination: x}}\ndestinations:\n  - {id: x, type: answering}\n",
        "id: a\nsource: {type: request, data_type: hl7v2, response: {mode: sometimes}}\n",
    ] {
        assert!(ChannelConfig::from_yaml(yaml).is_err(), "{yaml}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registered_encoder_can_produce_the_reply() {
    let (engine, inbox) = engine(Arc::new(Answering::default())).await;
    let config = ChannelConfig::from_yaml(
        "id: echo
source:
  type: request
  data_type: hl7v2
  response: {mode: pipeline, encoder: {type: passthrough}}
transformers:
  - {type: set, path: MSH-3, value: OXIM}
",
    )
    .unwrap();
    engine.deploy(config).await.unwrap();
    let reply = ask(&inbox, QUERY).await;
    let text = String::from_utf8(reply.data.unwrap()).unwrap();
    assert!(text.starts_with(r"MSH|^~\&|OXIM|LAB|"), "{text}");
    assert_eq!(reply.data_type, Some(DataType::Hl7V2));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_the_encoder_does_not_handle_get_no_reply() {
    let (engine, inbox) = engine(Arc::new(Answering::default())).await;
    let config = ChannelConfig::from_yaml(
        "id: picky
source:
  type: request
  data_type: hl7v2
  response: {mode: pipeline, encoder: {type: picky}}
",
    )
    .unwrap();
    engine.deploy(config).await.unwrap();
    let reply = ask(&inbox, QUERY).await;
    assert!(reply.data.is_none());
    assert_eq!(reply.status, MessageStatus::Completed, "{:?}", reply.error);
    engine.shutdown().await;
}
