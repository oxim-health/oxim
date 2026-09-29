//! In-process connectors: channel-to-channel routing and a timer.
//!
//! **`channel`** passes messages from one channel to another inside the
//! same OXIM process, for example to split a flow into a receiving channel
//! and several processing channels.
//!
//! - Source: no settings. It receives what `channel` destinations send to
//!   its channel.
//! - Destination: `channel` (required), the identifier of the receiving
//!   channel. A delivery succeeds once the receiving channel has stored
//!   the message durably; while that channel is not deployed the delivery
//!   is retried according to the destination's retry policy.
//!
//! The received message's correlation identifier is the sending message's
//! identifier, and `channel.from` and `channel.message` metadata name the
//! sending channel and message, so a message can be traced end to end.
//!
//! **`timer`** produces a message at a fixed interval, for example to poll
//! with a scripted step or to send a heartbeat.
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `interval` | required | Time between messages, for example `5m` |
//! | `payload` | empty | Text of each message |
//! | `immediately` | `false` | Whether the first message is produced at start instead of after one interval |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_model::{ChannelId, MessageId};
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::net::settings;

/// One message passed between channels, with the answer channel for the
/// storage outcome.
struct Handoff {
    payload: Vec<u8>,
    info: SubmitInfo,
    stored: oneshot::Sender<Result<MessageId, String>>,
}

/// A running `channel` source: its start generation and inbox.
type Inbox = (u64, mpsc::Sender<Handoff>);

/// The receiving `channel` sources of one engine, by channel.
#[derive(Clone, Default)]
struct Hub {
    inboxes: Arc<Mutex<HashMap<ChannelId, Inbox>>>,
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Hub {
    fn attach(&self, channel: &ChannelId, sender: mpsc::Sender<Handoff>) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut inboxes) = self.inboxes.lock() {
            inboxes.insert(channel.clone(), (generation, sender));
        }
        generation
    }

    fn detach(&self, channel: &ChannelId, generation: u64) {
        if let Ok(mut inboxes) = self.inboxes.lock()
            && inboxes.get(channel).is_some_and(|(g, _)| *g == generation)
        {
            inboxes.remove(channel);
        }
    }

    fn sender(&self, channel: &ChannelId) -> Option<mpsc::Sender<Handoff>> {
        self.inboxes
            .lock()
            .ok()
            .and_then(|inboxes| inboxes.get(channel).map(|(_, sender)| sender.clone()))
    }
}

/// Receives messages from `channel` destinations of other channels.
#[derive(Clone)]
pub struct ChannelSource {
    hub: Hub,
}

impl std::fmt::Debug for ChannelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSource").finish_non_exhaustive()
    }
}

#[async_trait]
impl SourceConnector for ChannelSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let (sender, mut inbox) = mpsc::channel::<Handoff>(64);
        let channel = context.channel().clone();
        let generation = self.hub.attach(&channel, sender);
        loop {
            let handoff = tokio::select! {
                () = context.cancelled() => break,
                next = inbox.recv() => match next {
                    Some(handoff) => handoff,
                    None => break,
                },
            };
            let outcome = context
                .submit(handoff.payload, handoff.info)
                .await
                .map_err(|e| e.to_string());
            if let Err(error) = &outcome {
                warn!(%channel, %error, "a message from another channel could not be stored");
            }
            let _ = handoff.stored.send(outcome);
        }
        self.hub.detach(&channel, generation);
        Ok(())
    }
}

/// Settings of the `channel` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelDestinationSettings {
    /// The receiving channel.
    pub channel: ChannelId,
}

/// Passes deliveries to another channel's `channel` source.
#[derive(Clone)]
pub struct ChannelDestination {
    hub: Hub,
    target: ChannelId,
}

impl std::fmt::Debug for ChannelDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelDestination")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl DestinationConnector for ChannelDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let target = &self.target;
        let sender = self.hub.sender(target).ok_or_else(|| {
            SendError::temporary(format!(
                "channel {target} is not deployed with a channel source"
            ))
        })?;
        let mut info = SubmitInfo {
            peer: Some(format!("channel:{}", delivery.channel)),
            correlation_id: Some(delivery.message_id.to_string()),
            ..SubmitInfo::default()
        };
        info.metadata
            .insert("channel.from".to_owned(), delivery.channel.to_string());
        info.metadata.insert(
            "channel.message".to_owned(),
            delivery.message_id.to_string(),
        );
        let (stored, outcome) = oneshot::channel();
        sender
            .send(Handoff {
                payload: delivery.payload.clone(),
                info,
                stored,
            })
            .await
            .map_err(|_| SendError::temporary(format!("channel {target} stopped")))?;
        match outcome.await {
            Ok(Ok(id)) => {
                debug!(from = %delivery.channel, to = %target, %id, "passed to channel");
                Ok(Some(id.to_string().into_bytes()))
            }
            Ok(Err(error)) => Err(SendError::temporary(format!(
                "channel {target} could not store the message: {error}"
            ))),
            Err(_) => Err(SendError::temporary(format!("channel {target} stopped"))),
        }
    }
}

/// Settings of the `timer` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimerSettings {
    /// Time between messages.
    pub interval: DurationText,
    /// Text of each message.
    #[serde(default)]
    pub payload: String,
    /// Whether the first message is produced at start.
    #[serde(default)]
    pub immediately: bool,
}

/// Produces a message at a fixed interval.
#[derive(Debug, Clone)]
pub struct TimerSource {
    settings: TimerSettings,
}

impl TimerSource {
    /// Validates the settings.
    pub fn new(settings: TimerSettings) -> Result<Self, EngineError> {
        if settings.interval.0 < Duration::from_millis(10) {
            return Err(EngineError::Config(
                "timer interval must be at least 10ms".into(),
            ));
        }
        Ok(Self { settings })
    }
}

#[async_trait]
impl SourceConnector for TimerSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let interval = self.settings.interval.0;
        let start = if self.settings.immediately {
            tokio::time::Instant::now()
        } else {
            tokio::time::Instant::now() + interval
        };
        let mut ticks = tokio::time::interval_at(start, interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                _ = ticks.tick() => {
                    let info = SubmitInfo {
                        peer: Some("timer".to_owned()),
                        ..SubmitInfo::default()
                    };
                    if let Err(error) = context.submit(self.settings.payload.clone().into_bytes(), info).await {
                        warn!(channel = %context.channel(), %error, "timer message could not be stored");
                    }
                }
            }
        }
    }
}

/// Registers `channel` (source and destination) and `timer`.
pub fn register(registry: &mut Registry) {
    let hub = Hub::default();
    let sources = hub.clone();
    registry
        .add_source("channel", move |config: &SourceConfig| {
            if !config.settings.is_empty() {
                return Err(EngineError::Config(
                    "the channel source has no settings".into(),
                ));
            }
            Ok(Arc::new(ChannelSource {
                hub: sources.clone(),
            }) as Arc<dyn SourceConnector>)
        })
        .add_destination("channel", move |config: &DestinationConfig| {
            let settings: ChannelDestinationSettings =
                settings(&config.settings, "channel destination")?;
            Ok(Arc::new(ChannelDestination {
                hub: hub.clone(),
                target: settings.channel,
            }) as Arc<dyn DestinationConnector>)
        })
        .add_source("timer", |config: &SourceConfig| {
            let settings: TimerSettings = settings(&config.settings, "timer source")?;
            Ok(Arc::new(TimerSource::new(settings)?) as Arc<dyn SourceConnector>)
        });
}
