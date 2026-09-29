//! Running captured inbound messages through an OXIM channel in an
//! isolated in-process engine.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, Reply, SendError,
    Settings, SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_model::{ConnectorId, MessageId, MessageStatus};
use oxim_store::{Delivery, SqliteStore};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use crate::ShadowError;

/// What OXIM produced for the replayed messages.
#[derive(Debug, Clone, Default)]
pub struct Replay {
    /// Encoded messages per destination, in delivery order, with the index
    /// of the inbound message that produced each.
    pub outputs: BTreeMap<ConnectorId, Vec<(usize, Vec<u8>)>>,
    /// The reply to each inbound message, when the channel answers
    /// requests.
    pub replies: Vec<Option<Vec<u8>>>,
    /// Inbound messages that ended in error, with the reason.
    pub errors: Vec<(usize, String)>,
    /// Inbound messages that the channel filtered.
    pub filtered: Vec<usize>,
}

type Request = (
    Vec<u8>,
    oneshot::Sender<Result<(MessageId, Option<Vec<u8>>), String>>,
);

/// Feeds the captured messages to the channel.
struct ShadowSource {
    inbox: AsyncMutex<mpsc::Receiver<Request>>,
}

impl std::fmt::Debug for ShadowSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShadowSource")
    }
}

#[async_trait]
impl SourceConnector for ShadowSource {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            let (raw, answer) = tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => match next {
                    Some(request) => request,
                    None => return Ok(()),
                },
            };
            let info = SubmitInfo {
                peer: Some("shadow".to_owned()),
                ..SubmitInfo::default()
            };
            let outcome = if context.responds() {
                context
                    .request(raw, info)
                    .await
                    .map(|reply: Reply| (reply.message_id, reply.data))
                    .map_err(|e| e.to_string())
            } else {
                context
                    .submit(raw, info)
                    .await
                    .map(|id| (id, None))
                    .map_err(|e| e.to_string())
            };
            let _ = answer.send(outcome);
        }
    }
}

type Recorded = Arc<Mutex<Vec<(MessageId, ConnectorId, Vec<u8>)>>>;

/// Records what a destination would have sent.
struct Sink {
    sent: Recorded,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sink")
    }
}

#[async_trait]
impl DestinationConnector for Sink {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        if let Ok(mut sent) = self.sent.lock() {
            sent.push((
                delivery.message_id,
                delivery.destination.clone(),
                delivery.payload.clone(),
            ));
        }
        Ok(None)
    }
}

/// Replays `inbound` through `channel`: its source is replaced by the
/// captured messages and its destinations by recorders, while its
/// filters, transformers, encoders and replies run unchanged with the
/// step types of `registry`.
pub async fn run(
    channel: &ChannelConfig,
    mut registry: Registry,
    inbound: &[Vec<u8>],
    timeout: Duration,
) -> Result<Replay, ShadowError> {
    let (sender, receiver) = mpsc::channel::<Request>(1);
    let source = Arc::new(ShadowSource {
        inbox: AsyncMutex::new(receiver),
    });
    let sent: Recorded = Arc::new(Mutex::new(Vec::new()));
    let sink_sent = sent.clone();
    registry
        .add_source("shadow-source", move |_| {
            Ok(source.clone() as Arc<dyn SourceConnector>)
        })
        .add_destination("shadow-sink", move |_| {
            Ok(Arc::new(Sink {
                sent: sink_sent.clone(),
            }) as Arc<dyn DestinationConnector>)
        });

    let mut config = channel.clone();
    config.enabled = true;
    config.source.kind = "shadow-source".to_owned();
    config.source.settings = Settings::new();
    for destination in &mut config.destinations {
        destination.kind = "shadow-sink".to_owned();
        destination.settings = Settings::new();
    }

    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(10);
    options.shutdown_grace = Duration::from_secs(2);
    let store = SqliteStore::open_in_memory().map_err(|e| ShadowError::Engine(e.to_string()))?;
    let engine = Engine::start(Box::new(store), registry, Arc::new(SystemClock), options)
        .await
        .map_err(|e| ShadowError::Engine(e.to_string()))?;
    let result = replay(&engine, &sender, &config, inbound, &sent, timeout).await;
    engine.shutdown().await;
    result
}

async fn replay(
    engine: &Engine,
    sender: &mpsc::Sender<Request>,
    config: &ChannelConfig,
    inbound: &[Vec<u8>],
    sent: &Recorded,
    timeout: Duration,
) -> Result<Replay, ShadowError> {
    engine
        .deploy(config.clone())
        .await
        .map_err(|e| ShadowError::Engine(e.to_string()))?;
    let mut replay = Replay::default();
    let mut ids: Vec<(MessageId, usize)> = Vec::new();
    for (index, raw) in inbound.iter().enumerate() {
        let (answer, response) = oneshot::channel();
        sender
            .send((raw.clone(), answer))
            .await
            .map_err(|_| ShadowError::Engine("the channel stopped".into()))?;
        let (id, reply) = response
            .await
            .map_err(|_| ShadowError::Engine("the channel stopped".into()))?
            .map_err(ShadowError::Engine)?;
        replay.replies.push(reply);
        ids.push((id, index));
        // One message at a time, so outputs keep the capture's order.
        let deadline = Instant::now() + timeout;
        loop {
            let record = engine
                .store()
                .run(move |store| store.message(id))
                .await
                .map_err(|e| ShadowError::Engine(e.to_string()))?;
            let done = record.as_ref().is_some_and(|record| match record.status {
                MessageStatus::Filtered | MessageStatus::Error | MessageStatus::Completed => true,
                MessageStatus::Transformed => {
                    record.destinations.iter().all(|d| d.status.is_final())
                }
                MessageStatus::Received => false,
            });
            if done {
                if let Some(record) = record {
                    match record.status {
                        MessageStatus::Error => replay.errors.push((
                            index,
                            record.error.unwrap_or_else(|| "processing failed".into()),
                        )),
                        MessageStatus::Filtered => replay.filtered.push(index),
                        _ => {}
                    }
                }
                break;
            }
            if Instant::now() > deadline {
                replay
                    .errors
                    .push((index, "not processed within the replay timeout".into()));
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let index_of: BTreeMap<MessageId, usize> = ids.into_iter().collect();
    let recorded = sent.lock().map(|sent| sent.clone()).unwrap_or_default();
    for (id, destination, payload) in recorded {
        let index = index_of.get(&id).copied().unwrap_or(usize::MAX);
        replay
            .outputs
            .entry(destination)
            .or_default()
            .push((index, payload));
    }
    Ok(replay)
}
