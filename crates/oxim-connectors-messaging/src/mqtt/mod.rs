//! MQTT 3.1.1 over TCP or TLS, with a client written for OXIM's delivery
//! rules: an inbound QoS 1 message is acknowledged (`PUBACK`) only after it
//! is stored, and a delivery succeeds only when the broker acknowledged it.

mod codec;
mod session;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use oxim_connectors::tls::{ClientTlsSettings, Dialer};
use oxim_connectors::values::DeliveryValues;
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::debug;

use codec::{DEFAULT_MAX_PACKET, Packet};
use session::{Login, Session};

#[cfg(test)]
mod tests;

use crate::common::{Secret, parse};
use crate::template::Template;

fn default_qos() -> u8 {
    1
}

fn default_keep_alive() -> DurationText {
    DurationText(Duration::from_secs(30))
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_ack_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

fn default_max_message_len() -> usize {
    DEFAULT_MAX_PACKET
}

/// Settings of the `mqtt` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MqttSourceSettings {
    /// Broker address, `host:port` (1883, or 8883 with TLS).
    pub broker: String,
    /// Topic filters to subscribe to (`+` and `#` wildcards allowed).
    pub topics: Vec<String>,
    /// Requested quality of service: 0 (at most once) or 1 (at least once).
    #[serde(default = "default_qos")]
    pub qos: u8,
    /// Client identifier; `oxim-<channel>` by default. Stable identifiers
    /// let the broker keep QoS 1 messages while OXIM is offline.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Whether the broker discards the session on connect.
    #[serde(default)]
    pub clean_session: bool,
    /// Keep-alive interval.
    #[serde(default = "default_keep_alive")]
    pub keep_alive: DurationText,
    /// User name.
    #[serde(default)]
    pub username: Option<String>,
    /// Password, inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Time to connect and log in.
    #[serde(default = "default_timeout")]
    pub connect_timeout: DurationText,
    /// Largest accepted message.
    #[serde(default = "default_max_message_len")]
    pub max_message_len: usize,
}

fn check_qos(qos: u8) -> Result<(), EngineError> {
    if qos > 1 {
        return Err(EngineError::Config("mqtt qos must be 0 or 1".into()));
    }
    Ok(())
}

/// Receives messages from MQTT topics.
#[derive(Debug)]
pub struct MqttSource {
    settings: MqttSourceSettings,
    secret: Secret,
    dialer: Dialer,
}

impl MqttSource {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: MqttSourceSettings) -> Result<Self, EngineError> {
        check_qos(settings.qos)?;
        if settings.topics.is_empty() || settings.topics.iter().any(|t| t.is_empty()) {
            return Err(EngineError::Config(
                "mqtt source needs at least one non-empty topic".into(),
            ));
        }
        let secret = Secret::new(
            settings.password.clone(),
            settings.password_env.clone(),
            "password",
        )?;
        let dialer = Dialer::new(
            &settings.broker,
            settings.connect_timeout.0,
            settings.tls.as_ref(),
        )?;
        Ok(Self {
            settings,
            secret,
            dialer,
        })
    }
}

fn error(e: impl std::fmt::Display) -> ConnectorError {
    ConnectorError(format!("mqtt: {e}"))
}

#[async_trait]
impl SourceConnector for MqttSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let s = &self.settings;
        let login = Login {
            client_id: s
                .client_id
                .clone()
                .unwrap_or_else(|| format!("oxim-{}", context.channel())),
            clean_session: s.clean_session,
            keep_alive: s.keep_alive.0,
            username: s.username.clone(),
            password: self.secret.resolve().map_err(error)?,
        };
        let (mut session, resumed) =
            Session::open(&self.dialer, &login, s.connect_timeout.0, s.max_message_len)
                .await
                .map_err(error)?;
        debug!(channel = %context.channel(), broker = %s.broker, resumed, "connected");
        let subscribe = session.packet_id();
        session
            .send(&Packet::Subscribe {
                packet_id: subscribe,
                filters: s.topics.iter().map(|t| (t.clone(), s.qos)).collect(),
            })
            .await
            .map_err(error)?;

        let keep_alive = s.keep_alive.0;
        let never = Duration::from_secs(365 * 86_400);
        let mut last_sent = Instant::now();
        let mut ping_sent: Option<Instant> = None;
        loop {
            let deadline = match (keep_alive.is_zero(), ping_sent) {
                (true, _) => Instant::now() + never,
                (false, Some(at)) => at + keep_alive,
                (false, None) => last_sent + keep_alive,
            };
            let packet = tokio::select! {
                () = context.cancelled() => {
                    session.close().await;
                    return Ok(());
                }
                () = tokio::time::sleep_until(deadline) => {
                    if ping_sent.is_some() {
                        return Err(error("the broker stopped answering"));
                    }
                    session.send(&Packet::PingReq).await.map_err(error)?;
                    last_sent = Instant::now();
                    ping_sent = Some(last_sent);
                    continue;
                }
                packet = session.read() => packet.map_err(error)?,
            };
            match packet {
                Packet::Publish {
                    qos,
                    retain,
                    topic,
                    packet_id,
                    payload,
                    ..
                } => {
                    let mut metadata = BTreeMap::new();
                    metadata.insert("mqtt.topic".to_owned(), topic);
                    if retain {
                        metadata.insert("mqtt.retain".to_owned(), "true".to_owned());
                    }
                    let info = SubmitInfo {
                        peer: Some(s.broker.clone()),
                        metadata,
                        ..SubmitInfo::default()
                    };
                    // Without PUBACK the broker delivers the message again
                    // after reconnecting.
                    context
                        .submit(payload, info)
                        .await
                        .map_err(|e| error(format!("the message could not be stored: {e}")))?;
                    if qos > 0
                        && let Some(id) = packet_id
                    {
                        session.send(&Packet::PubAck(id)).await.map_err(error)?;
                        last_sent = Instant::now();
                    }
                }
                Packet::SubAck { packet_id, codes } if packet_id == subscribe => {
                    if codes.contains(&0x80) {
                        return Err(error("the broker refused a subscription"));
                    }
                }
                Packet::PingResp => ping_sent = None,
                _ => {}
            }
        }
    }
}

/// Settings of the `mqtt` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MqttDestinationSettings {
    /// Broker address, `host:port`.
    pub broker: String,
    /// Topic; `{...}` placeholders take delivery values, for example
    /// `lab/{MSH-3}/results`.
    pub topic: String,
    /// Quality of service: 0 or 1 (wait for `PUBACK`).
    #[serde(default = "default_qos")]
    pub qos: u8,
    /// Whether the broker keeps the message for new subscribers.
    #[serde(default)]
    pub retain: bool,
    /// Client identifier; `oxim-<channel>-<destination>` by default.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Keep-alive interval.
    #[serde(default = "default_keep_alive")]
    pub keep_alive: DurationText,
    /// User name.
    #[serde(default)]
    pub username: Option<String>,
    /// Password, inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Time to connect and log in.
    #[serde(default = "default_timeout")]
    pub connect_timeout: DurationText,
    /// Time to wait for `PUBACK`.
    #[serde(default = "default_ack_timeout")]
    pub ack_timeout: DurationText,
}

/// Publishes deliveries to an MQTT topic.
#[derive(Debug)]
pub struct MqttDestination {
    settings: MqttDestinationSettings,
    topic: Template,
    secret: Secret,
    dialer: Dialer,
    session: Mutex<Option<(Session, Instant)>>,
}

impl MqttDestination {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: MqttDestinationSettings) -> Result<Self, EngineError> {
        check_qos(settings.qos)?;
        let topic = Template::parse(&settings.topic, "mqtt topic")?;
        let secret = Secret::new(
            settings.password.clone(),
            settings.password_env.clone(),
            "password",
        )?;
        let dialer = Dialer::new(
            &settings.broker,
            settings.connect_timeout.0,
            settings.tls.as_ref(),
        )?;
        Ok(Self {
            settings,
            topic,
            secret,
            dialer,
            session: Mutex::new(None),
        })
    }

    async fn open(&self, delivery: &Delivery) -> Result<Session, String> {
        let s = &self.settings;
        let login = Login {
            client_id: s
                .client_id
                .clone()
                .unwrap_or_else(|| format!("oxim-{}-{}", delivery.channel, delivery.destination)),
            clean_session: true,
            keep_alive: s.keep_alive.0,
            username: s.username.clone(),
            password: self.secret.resolve()?,
        };
        Session::open(&self.dialer, &login, s.connect_timeout.0, 64 * 1024)
            .await
            .map(|(session, _)| session)
    }

    async fn publish(
        &self,
        session: &mut Session,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), String> {
        let s = &self.settings;
        let packet_id = (s.qos > 0).then(|| session.packet_id());
        session
            .send(&Packet::Publish {
                dup: false,
                qos: s.qos,
                retain: s.retain,
                topic: topic.to_owned(),
                packet_id,
                payload: payload.to_vec(),
            })
            .await?;
        let Some(id) = packet_id else {
            return Ok(());
        };
        let deadline = Instant::now() + s.ack_timeout.0;
        loop {
            match tokio::time::timeout_at(deadline, session.read()).await {
                Err(_) => return Err("the broker did not acknowledge the message".into()),
                Ok(Err(e)) => return Err(e),
                Ok(Ok(Packet::PubAck(acked))) if acked == id => return Ok(()),
                Ok(Ok(_)) => {}
            }
        }
    }
}

#[async_trait]
impl DestinationConnector for MqttDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let topic = self
            .topic
            .render(&mut DeliveryValues::new(delivery))
            .map_err(|e| SendError::permanent(format!("mqtt topic: {e}")))?;
        let mut slot = self.session.lock().await;
        // A connection idle for longer than the keep-alive may have been
        // dropped by the broker: start afresh.
        if slot
            .as_ref()
            .is_some_and(|(_, used)| used.elapsed() >= self.settings.keep_alive.0)
            && let Some((old, _)) = slot.take()
        {
            old.close().await;
        }
        let reused = slot.is_some();
        for attempt in 0..2 {
            if slot.is_none() {
                let session = self
                    .open(delivery)
                    .await
                    .map_err(|e| SendError::temporary(format!("mqtt: {e}")))?;
                *slot = Some((session, Instant::now()));
            }
            let Some((session, used)) = slot.as_mut() else {
                return Err(SendError::temporary("mqtt: no connection"));
            };
            match self.publish(session, &topic, &delivery.payload).await {
                Ok(()) => {
                    *used = Instant::now();
                    return Ok(None);
                }
                Err(e) => {
                    *slot = None;
                    if !(reused && attempt == 0) {
                        return Err(SendError::temporary(format!("mqtt: {e}")));
                    }
                    debug!(broker = %self.settings.broker, error = %e, "reconnecting");
                }
            }
        }
        Err(SendError::temporary("mqtt: publishing failed"))
    }
}

/// Registers the `mqtt` source and destination.
pub(crate) fn register(registry: &mut Registry) {
    registry
        .add_source("mqtt", |config: &SourceConfig| {
            let settings: MqttSourceSettings = parse(&config.settings, "mqtt source")?;
            Ok(Arc::new(MqttSource::new(settings)?) as Arc<dyn SourceConnector>)
        })
        .add_destination("mqtt", |config: &DestinationConfig| {
            let settings: MqttDestinationSettings = parse(&config.settings, "mqtt destination")?;
            Ok(Arc::new(MqttDestination::new(settings)?) as Arc<dyn DestinationConnector>)
        });
}
