//! NATS and NATS JetStream with async-nats.
//!
//! With `jetstream` the source reads a durable pull consumer and
//! acknowledges each message after it is stored (at-least-once); without
//! it the source subscribes to core NATS, which does not keep messages for
//! subscribers that are offline. The destination publishes to core NATS or,
//! with `jetstream: true`, waits for the stream's acknowledgment; the OXIM
//! message identifier is sent as `Nats-Msg-Id`, so JetStream discards
//! duplicates of a retried delivery.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::{AckPolicy, pull};
use async_nats::{Client, ConnectOptions, HeaderMap};
use futures_util::StreamExt;
use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_connectors::values::DeliveryValues;
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::debug;

use crate::common::{Secret, metadata_text, parse};
use crate::template::Template;

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_ack_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

/// Connection settings shared by the source and the destination.
#[derive(Debug, Clone)]
struct Server {
    servers: Vec<String>,
    username: Option<String>,
    password: Secret,
    token: Secret,
    credentials_file: Option<PathBuf>,
    tls: Option<rustls::ClientConfig>,
    timeout: Duration,
}

impl Server {
    #[allow(clippy::too_many_arguments)]
    fn new(
        servers: &[String],
        username: Option<String>,
        password: Secret,
        token: Secret,
        credentials_file: Option<PathBuf>,
        tls: Option<&ClientTlsSettings>,
        timeout: Duration,
    ) -> Result<Self, EngineError> {
        if servers.is_empty() {
            return Err(EngineError::Config("nats needs at least one server".into()));
        }
        let methods = usize::from(username.is_some())
            + usize::from(token.is_set())
            + usize::from(credentials_file.is_some());
        if methods > 1 {
            return Err(EngineError::Config(
                "nats: use one of username/password, token or credentials_file".into(),
            ));
        }
        if password.is_set() && username.is_none() {
            return Err(EngineError::Config(
                "nats: a password needs a username".into(),
            ));
        }
        let tls = tls
            .map(|settings| {
                if settings.server_name.is_some() {
                    return Err(EngineError::Config(
                        "nats verifies the certificate against the server address; server_name is not supported"
                            .into(),
                    ));
                }
                client_config(settings)
            })
            .transpose()?;
        Ok(Self {
            servers: servers.to_vec(),
            username,
            password,
            token,
            credentials_file,
            tls,
            timeout,
        })
    }

    fn describe(&self) -> String {
        self.servers.join(",")
    }

    async fn connect(&self) -> Result<Client, String> {
        let mut options = ConnectOptions::new()
            .name("oxim")
            .connection_timeout(self.timeout);
        if let Some(path) = &self.credentials_file {
            options = options
                .credentials_file(path)
                .await
                .map_err(|e| format!("credentials file {}: {e}", path.display()))?;
        } else if let Some(token) = self.token.resolve()? {
            options = options.token(token);
        } else if let Some(username) = &self.username {
            let password = self.password.resolve()?.unwrap_or_default();
            options = options.user_and_password(username.clone(), password);
        }
        if let Some(tls) = &self.tls {
            options = options.require_tls(true).tls_client_config(tls.clone());
        }
        options
            .connect(self.servers.as_slice())
            .await
            .map_err(|e| format!("cannot connect to {}: {e}", self.describe()))
    }
}

/// A JetStream durable consumer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JetStreamSource {
    /// The stream.
    pub stream: String,
    /// The durable consumer; created as an explicit-ack pull consumer when
    /// it does not exist.
    pub consumer: String,
}

/// Settings of the `nats` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsSourceSettings {
    /// Server URLs, `nats://host:4222` (`tls://` for TLS).
    pub servers: Vec<String>,
    /// Subject to subscribe to (wildcards allowed); with `jetstream` it
    /// filters the consumer when the consumer is created.
    #[serde(default)]
    pub subject: Option<String>,
    /// Queue group, to share core NATS messages among several subscribers.
    #[serde(default)]
    pub queue_group: Option<String>,
    /// Read a JetStream durable consumer instead of core NATS.
    #[serde(default)]
    pub jetstream: Option<JetStreamSource>,
    /// User name.
    #[serde(default)]
    pub username: Option<String>,
    /// Password, inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// Token, inline (discouraged).
    #[serde(default)]
    pub token: Option<String>,
    /// Environment variable holding the token.
    #[serde(default)]
    pub token_env: Option<String>,
    /// NATS credentials file (JWT and NKey seed).
    #[serde(default)]
    pub credentials_file: Option<PathBuf>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Time to connect.
    #[serde(default = "default_timeout")]
    pub connect_timeout: DurationText,
}

/// Receives messages from NATS or a JetStream consumer.
#[derive(Debug)]
pub struct NatsSource {
    settings: NatsSourceSettings,
    server: Server,
}

impl NatsSource {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: NatsSourceSettings) -> Result<Self, EngineError> {
        if settings.jetstream.is_none() && settings.subject.as_deref().is_none_or(str::is_empty) {
            return Err(EngineError::Config(
                "nats source needs a subject, or a jetstream consumer".into(),
            ));
        }
        if settings.jetstream.is_some() && settings.queue_group.is_some() {
            return Err(EngineError::Config(
                "nats: queue_group applies to core NATS only; share a JetStream consumer instead"
                    .into(),
            ));
        }
        let server = Server::new(
            &settings.servers,
            settings.username.clone(),
            Secret::new(
                settings.password.clone(),
                settings.password_env.clone(),
                "password",
            )?,
            Secret::new(settings.token.clone(), settings.token_env.clone(), "token")?,
            settings.credentials_file.clone(),
            settings.tls.as_ref(),
            settings.connect_timeout.0,
        )?;
        Ok(Self { settings, server })
    }
}

fn error(e: impl std::fmt::Display) -> ConnectorError {
    ConnectorError(format!("nats: {e}"))
}

/// Text headers as metadata.
fn header_metadata(headers: Option<&HeaderMap>, metadata: &mut BTreeMap<String, String>) {
    let Some(headers) = headers else {
        return;
    };
    for (name, values) in headers.iter() {
        if let Some(value) = values
            .first()
            .and_then(|v| metadata_text(v.as_str().as_bytes()))
        {
            metadata.insert(format!("nats.header.{name}"), value);
        }
    }
}

impl NatsSource {
    async fn run_core(
        &self,
        context: &SourceContext,
        client: Client,
        subject: &str,
    ) -> Result<(), ConnectorError> {
        let mut subscriber = match &self.settings.queue_group {
            Some(group) => client
                .queue_subscribe(subject.to_owned(), group.clone())
                .await
                .map_err(error)?,
            None => client.subscribe(subject.to_owned()).await.map_err(error)?,
        };
        loop {
            let message = tokio::select! {
                () = context.cancelled() => {
                    let _ = subscriber.unsubscribe().await;
                    return Ok(());
                }
                next = subscriber.next() => next.ok_or_else(|| error("the subscription ended"))?,
            };
            let mut metadata = BTreeMap::new();
            metadata.insert("nats.subject".to_owned(), message.subject.to_string());
            header_metadata(message.headers.as_ref(), &mut metadata);
            let info = SubmitInfo {
                peer: Some(self.server.describe()),
                metadata,
                ..SubmitInfo::default()
            };
            // A request with a reply subject gets the channel's reply.
            match (&message.reply, context.responds()) {
                (Some(reply), true) => {
                    let answer = context
                        .request(message.payload.to_vec(), info)
                        .await
                        .map_err(|e| error(format!("the message could not be stored: {e}")))?;
                    if let Some(data) = answer.data {
                        client
                            .publish(reply.clone(), data.into())
                            .await
                            .map_err(error)?;
                    }
                }
                _ => {
                    context
                        .submit(message.payload.to_vec(), info)
                        .await
                        .map_err(|e| error(format!("the message could not be stored: {e}")))?;
                }
            }
        }
    }

    async fn run_jetstream(
        &self,
        context: &SourceContext,
        client: Client,
        jetstream: &JetStreamSource,
    ) -> Result<(), ConnectorError> {
        let js = async_nats::jetstream::new(client);
        let stream = js.get_stream(&jetstream.stream).await.map_err(error)?;
        let consumer = stream
            .get_or_create_consumer::<pull::Config>(
                &jetstream.consumer,
                pull::Config {
                    durable_name: Some(jetstream.consumer.clone()),
                    ack_policy: AckPolicy::Explicit,
                    filter_subject: self.settings.subject.clone().unwrap_or_default(),
                    ..pull::Config::default()
                },
            )
            .await
            .map_err(error)?;
        let mut messages = consumer.messages().await.map_err(error)?;
        loop {
            let message = tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = messages.next() => next.ok_or_else(|| error("the consumer ended"))?.map_err(error)?,
            };
            let mut metadata = BTreeMap::new();
            metadata.insert("nats.subject".to_owned(), message.subject.to_string());
            metadata.insert("nats.stream".to_owned(), jetstream.stream.clone());
            header_metadata(message.headers.as_ref(), &mut metadata);
            let info = SubmitInfo {
                peer: Some(self.server.describe()),
                metadata,
                ..SubmitInfo::default()
            };
            match context.submit(message.payload.to_vec(), info).await {
                Ok(_) => message.double_ack().await.map_err(error)?,
                Err(e) => {
                    let _ = message.ack_with(AckKind::Nak(None)).await;
                    return Err(error(format!("the message could not be stored: {e}")));
                }
            }
        }
    }
}

#[async_trait]
impl SourceConnector for NatsSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let client = self.server.connect().await.map_err(error)?;
        debug!(channel = %context.channel(), servers = %self.server.describe(), "connected");
        match (&self.settings.jetstream, &self.settings.subject) {
            (Some(jetstream), _) => self.run_jetstream(&context, client, jetstream).await,
            (None, Some(subject)) => self.run_core(&context, client, subject).await,
            (None, None) => Err(error("no subject")),
        }
    }
}

/// Settings of the `nats` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsDestinationSettings {
    /// Server URLs.
    pub servers: Vec<String>,
    /// Subject; `{...}` placeholders take delivery values.
    pub subject: String,
    /// Publish to JetStream and wait for the stream's acknowledgment.
    #[serde(default)]
    pub jetstream: bool,
    /// User name.
    #[serde(default)]
    pub username: Option<String>,
    /// Password, inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// Token, inline (discouraged).
    #[serde(default)]
    pub token: Option<String>,
    /// Environment variable holding the token.
    #[serde(default)]
    pub token_env: Option<String>,
    /// NATS credentials file.
    #[serde(default)]
    pub credentials_file: Option<PathBuf>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Time to connect.
    #[serde(default = "default_timeout")]
    pub connect_timeout: DurationText,
    /// Time to wait for the JetStream acknowledgment or the flush.
    #[serde(default = "default_ack_timeout")]
    pub ack_timeout: DurationText,
}

/// Publishes deliveries to a subject.
#[derive(Debug)]
pub struct NatsDestination {
    settings: NatsDestinationSettings,
    subject: Template,
    server: Server,
    client: Mutex<Option<Client>>,
}

impl NatsDestination {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: NatsDestinationSettings) -> Result<Self, EngineError> {
        let subject = Template::parse(&settings.subject, "nats subject")?;
        if settings.subject.trim().is_empty() {
            return Err(EngineError::Config(
                "nats destination needs a subject".into(),
            ));
        }
        let server = Server::new(
            &settings.servers,
            settings.username.clone(),
            Secret::new(
                settings.password.clone(),
                settings.password_env.clone(),
                "password",
            )?,
            Secret::new(settings.token.clone(), settings.token_env.clone(), "token")?,
            settings.credentials_file.clone(),
            settings.tls.as_ref(),
            settings.connect_timeout.0,
        )?;
        Ok(Self {
            settings,
            subject,
            server,
            client: Mutex::new(None),
        })
    }
}

#[async_trait]
impl DestinationConnector for NatsDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let subject = self
            .subject
            .render(&mut DeliveryValues::new(delivery))
            .map_err(|e| SendError::permanent(format!("nats subject: {e}")))?;
        let client = {
            let mut slot = self.client.lock().await;
            if slot.is_none() {
                *slot = Some(
                    self.server
                        .connect()
                        .await
                        .map_err(|e| SendError::temporary(format!("nats: {e}")))?,
                );
            }
            slot.clone()
                .ok_or_else(|| SendError::temporary("nats: no connection"))?
        };
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", delivery.message_id.to_string().as_str());
        headers.insert("Oxim-Channel", delivery.channel.to_string().as_str());
        let payload = bytes::Bytes::from(delivery.payload.clone());
        let timeout = self.settings.ack_timeout.0;
        let temporary = |e: String| SendError::temporary(format!("nats: {e}"));
        if self.settings.jetstream {
            let js = async_nats::jetstream::new(client);
            let ack = tokio::time::timeout(timeout, async {
                js.publish_with_headers(subject, headers, payload)
                    .await
                    .map_err(|e| e.to_string())?
                    .await
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|_| temporary("the stream did not acknowledge the message".into()))?
            .map_err(temporary)?;
            Ok(Some(
                format!("stream {} sequence {}", ack.stream, ack.sequence).into_bytes(),
            ))
        } else {
            client
                .publish_with_headers(subject, headers, payload)
                .await
                .map_err(|e| temporary(e.to_string()))?;
            tokio::time::timeout(timeout, client.flush())
                .await
                .map_err(|_| temporary("flushing timed out".into()))?
                .map_err(|e| temporary(e.to_string()))?;
            Ok(None)
        }
    }
}

/// Registers the `nats` source and destination.
pub(crate) fn register(registry: &mut Registry) {
    registry
        .add_source("nats", |config: &SourceConfig| {
            let settings: NatsSourceSettings = parse(&config.settings, "nats source")?;
            Ok(Arc::new(NatsSource::new(settings)?) as Arc<dyn SourceConnector>)
        })
        .add_destination("nats", |config: &DestinationConfig| {
            let settings: NatsDestinationSettings = parse(&config.settings, "nats destination")?;
            Ok(Arc::new(NatsDestination::new(settings)?) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_settings() {
        let source = |json: serde_json::Value| {
            NatsSource::new(serde_json::from_value::<NatsSourceSettings>(json).unwrap())
        };
        assert!(
            source(serde_json::json!({"servers": ["nats://n:4222"], "subject": "lab.>"})).is_ok()
        );
        assert!(
            source(serde_json::json!({
                "servers": ["nats://n:4222"],
                "jetstream": {"stream": "LAB", "consumer": "oxim"},
                "credentials_file": "/etc/oxim/nats.creds"
            }))
            .is_ok()
        );
        for bad in [
            serde_json::json!({"servers": [], "subject": "a"}),
            serde_json::json!({"servers": ["n"]}),
            serde_json::json!({"servers": ["n"], "subject": "a", "username": "u", "token": "t"}),
            serde_json::json!({"servers": ["n"], "subject": "a", "password": "p"}),
            serde_json::json!({"servers": ["n"], "queue_group": "g", "jetstream": {"stream": "S", "consumer": "c"}}),
        ] {
            assert!(source(bad.clone()).is_err(), "{bad}");
        }
        let destination = |json: serde_json::Value| {
            NatsDestination::new(serde_json::from_value::<NatsDestinationSettings>(json).unwrap())
        };
        assert!(
            destination(
                serde_json::json!({"servers": ["n"], "subject": "lab.{MSH-3}", "jetstream": true})
            )
            .is_ok()
        );
        assert!(destination(serde_json::json!({"servers": ["n"], "subject": " "})).is_err());
    }

    #[test]
    fn maps_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("Site", "LAB1");
        let mut metadata = BTreeMap::new();
        header_metadata(Some(&headers), &mut metadata);
        assert_eq!(
            metadata.get("nats.header.Site").map(String::as_str),
            Some("LAB1")
        );
    }
}
