//! AMQP 0-9-1 (RabbitMQ) with lapin.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use lapin::options::{
    BasicAckOptions, BasicConsumeOptions, BasicNackOptions, BasicPublishOptions, BasicQosOptions,
    ConfirmSelectOptions,
};
use lapin::tcp::{OwnedIdentity, OwnedTLSConfig};
use lapin::types::{AMQPValue, FieldTable, ShortString};
use lapin::uri::{AMQPScheme, AMQPUri, AMQPUserInfo};
use lapin::{BasicProperties, Channel, Confirmation, Connection, ConnectionProperties};
use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_connectors::values::DeliveryValues;
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_model::DataType;
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::debug;

use crate::common::{Secret, metadata_text, parse};
use crate::template::Template;

fn default_prefetch() -> u16 {
    10
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_confirm_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

fn default_true() -> bool {
    true
}

/// Whether the URL's user information holds a password (`user:password@`).
/// The parsed URI cannot tell: it fills in `guest` when none is given.
fn url_has_password(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

/// How to reach the broker.
#[derive(Debug, Clone)]
struct Broker {
    uri: AMQPUri,
    secret: Secret,
    tls: OwnedTLSConfig,
    timeout: Duration,
}

impl Broker {
    fn new(
        url: &str,
        username: Option<&String>,
        secret: Secret,
        tls: Option<&ClientTlsSettings>,
        timeout: Duration,
    ) -> Result<Self, EngineError> {
        let mut uri =
            AMQPUri::from_str(url).map_err(|e| EngineError::Config(format!("amqp url: {e}")))?;
        if url_has_password(url) && secret.is_set() {
            return Err(EngineError::Config(
                "amqp: the url already holds a password; remove it or the password setting".into(),
            ));
        }
        if let Some(username) = username {
            uri.authority.userinfo.username = username.clone();
        }
        let tls = match tls {
            None => OwnedTLSConfig::default(),
            Some(settings) => {
                if uri.scheme != AMQPScheme::AMQPS {
                    return Err(EngineError::Config(
                        "amqp: tls settings need an amqps:// url".into(),
                    ));
                }
                if settings.server_name.is_some() {
                    return Err(EngineError::Config(
                        "amqp verifies the certificate against the url host; server_name is not supported"
                            .into(),
                    ));
                }
                // Reads and checks the files at deploy time.
                client_config(settings)?;
                let read = |path: &std::path::PathBuf| {
                    std::fs::read(path).map_err(|e| {
                        EngineError::Config(format!(
                            "amqp tls: cannot read {}: {e}",
                            path.display()
                        ))
                    })
                };
                OwnedTLSConfig {
                    cert_chain: settings
                        .ca_file
                        .as_ref()
                        .map(|path| read(path).map(|b| String::from_utf8_lossy(&b).into_owned()))
                        .transpose()?,
                    identity: match (&settings.cert_file, &settings.key_file) {
                        (Some(cert), Some(key)) => Some(OwnedIdentity::PKCS8 {
                            pem: read(cert)?,
                            key: read(key)?,
                        }),
                        _ => None,
                    },
                }
            }
        };
        Ok(Self {
            uri,
            secret,
            tls,
            timeout,
        })
    }

    /// Where the broker is, without credentials.
    fn describe(&self) -> String {
        format!("{}:{}", self.uri.authority.host, self.uri.authority.port)
    }

    async fn connect(&self) -> Result<Connection, String> {
        let mut uri = self.uri.clone();
        if let Some(password) = self.secret.resolve()? {
            uri.authority.userinfo = AMQPUserInfo {
                username: uri.authority.userinfo.username.clone(),
                password,
            };
        }
        let runtime = lapin::runtime::default_runtime().map_err(|e| e.to_string())?;
        let properties = ConnectionProperties::default().with_connection_name("oxim".into());
        tokio::time::timeout(
            self.timeout,
            Connection::connect_uri_with_config(uri, properties, self.tls.clone(), runtime),
        )
        .await
        .map_err(|_| format!("connecting to {} timed out", self.describe()))?
        .map_err(|e| format!("cannot connect to {}: {e}", self.describe()))
    }
}

/// Settings of the `amqp` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmqpSourceSettings {
    /// Broker URL, `amqp://host:5672/vhost` or `amqps://host:5671/vhost`
    /// (`%2f` is the default vhost `/`).
    pub url: String,
    /// The queue to consume.
    pub queue: String,
    /// Unacknowledged messages the broker may send ahead.
    #[serde(default = "default_prefetch")]
    pub prefetch: u16,
    /// Consumer tag; `oxim-<channel>` by default.
    #[serde(default)]
    pub consumer_tag: Option<String>,
    /// User name (overrides the URL's).
    #[serde(default)]
    pub username: Option<String>,
    /// Password, inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// TLS settings for `amqps://` (CA file and client certificate).
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Time to connect.
    #[serde(default = "default_timeout")]
    pub connect_timeout: DurationText,
}

/// Consumes a queue; each message is acknowledged after it is stored.
#[derive(Debug)]
pub struct AmqpSource {
    settings: AmqpSourceSettings,
    broker: Broker,
}

impl AmqpSource {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: AmqpSourceSettings) -> Result<Self, EngineError> {
        if settings.queue.is_empty() {
            return Err(EngineError::Config("amqp source needs a queue".into()));
        }
        let secret = Secret::new(
            settings.password.clone(),
            settings.password_env.clone(),
            "password",
        )?;
        let broker = Broker::new(
            &settings.url,
            settings.username.as_ref(),
            secret,
            settings.tls.as_ref(),
            settings.connect_timeout.0,
        )?;
        Ok(Self { settings, broker })
    }
}

fn error(e: impl std::fmt::Display) -> ConnectorError {
    ConnectorError(format!("amqp: {e}"))
}

/// Text headers as metadata.
fn header_metadata(headers: Option<&FieldTable>, metadata: &mut BTreeMap<String, String>) {
    let Some(headers) = headers else {
        return;
    };
    for (name, value) in headers {
        let text = match value {
            AMQPValue::LongString(text) => metadata_text(text.as_bytes()),
            AMQPValue::ShortString(text) => metadata_text(text.as_str().as_bytes()),
            AMQPValue::Boolean(flag) => Some(flag.to_string()),
            AMQPValue::LongInt(n) => Some(n.to_string()),
            AMQPValue::LongLongInt(n) => Some(n.to_string()),
            AMQPValue::ShortInt(n) => Some(n.to_string()),
            _ => None,
        };
        if let Some(text) = text {
            metadata.insert(format!("amqp.header.{}", name.as_str()), text);
        }
    }
}

#[async_trait]
impl SourceConnector for AmqpSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let s = &self.settings;
        let connection = self.broker.connect().await.map_err(error)?;
        let channel = connection.create_channel().await.map_err(error)?;
        channel
            .basic_qos(s.prefetch, BasicQosOptions::default())
            .await
            .map_err(error)?;
        let tag = s
            .consumer_tag
            .clone()
            .unwrap_or_else(|| format!("oxim-{}", context.channel()));
        let mut consumer = channel
            .basic_consume(
                s.queue.as_str().into(),
                tag.into(),
                BasicConsumeOptions::default(),
                FieldTable::default(),
            )
            .await
            .map_err(error)?;
        debug!(channel = %context.channel(), broker = %self.broker.describe(), queue = %s.queue, "consuming");
        loop {
            let next = tokio::select! {
                () = context.cancelled() => {
                    let _ = channel.close(200, "OXIM is stopping".into()).await;
                    let _ = connection.close(200, "OXIM is stopping".into()).await;
                    return Ok(());
                }
                next = consumer.next() => next,
            };
            let delivery = match next {
                None => return Err(error("the broker cancelled the consumer")),
                Some(result) => result.map_err(error)?,
            };
            let mut metadata = BTreeMap::new();
            metadata.insert("amqp.exchange".to_owned(), delivery.exchange.to_string());
            metadata.insert(
                "amqp.routing_key".to_owned(),
                delivery.routing_key.to_string(),
            );
            if delivery.redelivered {
                metadata.insert("amqp.redelivered".to_owned(), "true".to_owned());
            }
            let properties = &delivery.properties;
            if let Some(id) = properties.message_id() {
                metadata.insert("amqp.message_id".to_owned(), id.to_string());
            }
            header_metadata(properties.headers().as_ref(), &mut metadata);
            let info = SubmitInfo {
                peer: Some(self.broker.describe()),
                correlation_id: properties
                    .correlation_id()
                    .as_ref()
                    .map(ToString::to_string),
                metadata,
                ..SubmitInfo::default()
            };
            match context.submit(delivery.data.clone(), info).await {
                Ok(_) => {
                    delivery
                        .ack(BasicAckOptions::default())
                        .await
                        .map_err(error)?;
                }
                Err(e) => {
                    let _ = delivery
                        .nack(BasicNackOptions {
                            requeue: true,
                            multiple: false,
                        })
                        .await;
                    return Err(error(format!("the message could not be stored: {e}")));
                }
            }
        }
    }
}

/// Settings of the `amqp` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmqpDestinationSettings {
    /// Broker URL.
    pub url: String,
    /// Exchange; empty for the default exchange (routing key = queue).
    #[serde(default)]
    pub exchange: String,
    /// Routing key; `{...}` placeholders take delivery values.
    #[serde(default)]
    pub routing_key: String,
    /// Whether an unroutable message is returned (and the delivery fails)
    /// instead of being dropped by the broker.
    #[serde(default = "default_true")]
    pub mandatory: bool,
    /// Whether the broker writes the message to disk.
    #[serde(default = "default_true")]
    pub persistent: bool,
    /// `content_type` property; by default from the data type.
    #[serde(default)]
    pub content_type: Option<String>,
    /// User name (overrides the URL's).
    #[serde(default)]
    pub username: Option<String>,
    /// Password, inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
    /// TLS settings for `amqps://`.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Time to connect.
    #[serde(default = "default_timeout")]
    pub connect_timeout: DurationText,
    /// Time to wait for the broker's confirmation.
    #[serde(default = "default_confirm_timeout")]
    pub confirm_timeout: DurationText,
}

/// The usual media type of a data type.
pub(crate) fn media_type(data_type: Option<DataType>) -> &'static str {
    match data_type {
        Some(DataType::Hl7V2) => "x-application/hl7-v2+er7",
        Some(DataType::Fhir) => "application/fhir+json",
        Some(DataType::Json) => "application/json",
        Some(DataType::Xml | DataType::Poct1a | DataType::Cda) => "application/xml",
        Some(DataType::Delimited) => "text/csv",
        Some(DataType::Astm | DataType::FixedWidth | DataType::Ncpdp | DataType::X12) => {
            "text/plain"
        }
        Some(DataType::Dicom) => "application/dicom",
        _ => "application/octet-stream",
    }
}

/// Publishes deliveries with publisher confirms.
#[derive(Debug)]
pub struct AmqpDestination {
    settings: AmqpDestinationSettings,
    routing_key: Template,
    broker: Broker,
    channel: Mutex<Option<(Connection, Channel)>>,
}

impl AmqpDestination {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: AmqpDestinationSettings) -> Result<Self, EngineError> {
        let routing_key = Template::parse(&settings.routing_key, "amqp routing_key")?;
        if settings.exchange.is_empty() && settings.routing_key.is_empty() {
            return Err(EngineError::Config(
                "amqp destination needs an exchange or a routing_key (the queue)".into(),
            ));
        }
        let secret = Secret::new(
            settings.password.clone(),
            settings.password_env.clone(),
            "password",
        )?;
        let broker = Broker::new(
            &settings.url,
            settings.username.as_ref(),
            secret,
            settings.tls.as_ref(),
            settings.connect_timeout.0,
        )?;
        Ok(Self {
            settings,
            routing_key,
            broker,
            channel: Mutex::new(None),
        })
    }
}

#[async_trait]
impl DestinationConnector for AmqpDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let s = &self.settings;
        let routing_key = self
            .routing_key
            .render(&mut DeliveryValues::new(delivery))
            .map_err(|e| SendError::permanent(format!("amqp routing_key: {e}")))?;
        let mut slot = self.channel.lock().await;
        if slot.as_ref().is_some_and(|(connection, channel)| {
            !connection.status().connected() || !channel.status().connected()
        }) {
            *slot = None;
        }
        if slot.is_none() {
            let connection = self
                .broker
                .connect()
                .await
                .map_err(|e| SendError::temporary(format!("amqp: {e}")))?;
            let channel = connection
                .create_channel()
                .await
                .map_err(|e| SendError::temporary(format!("amqp: {e}")))?;
            channel
                .confirm_select(ConfirmSelectOptions::default())
                .await
                .map_err(|e| SendError::temporary(format!("amqp: {e}")))?;
            *slot = Some((connection, channel));
        }
        let Some((_, channel)) = slot.as_ref() else {
            return Err(SendError::temporary("amqp: no channel"));
        };
        let content_type = s
            .content_type
            .clone()
            .unwrap_or_else(|| media_type(delivery.data_type).to_owned());
        let mut headers = FieldTable::default();
        headers.insert(
            "oxim-channel".into(),
            AMQPValue::LongString(delivery.channel.to_string().into()),
        );
        let properties = BasicProperties::default()
            .with_message_id(delivery.message_id.to_string().into())
            .with_content_type(ShortString::from(content_type))
            .with_delivery_mode(if s.persistent { 2 } else { 1 })
            .with_app_id("oxim".into())
            .with_headers(headers);
        let published = channel
            .basic_publish(
                s.exchange.as_str().into(),
                routing_key.into(),
                BasicPublishOptions {
                    mandatory: s.mandatory,
                    immediate: false,
                },
                &delivery.payload,
                properties,
            )
            .await;
        let confirm = match published {
            Ok(confirm) => confirm,
            Err(e) => {
                *slot = None;
                return Err(SendError::temporary(format!("amqp: {e}")));
            }
        };
        match tokio::time::timeout(s.confirm_timeout.0, confirm).await {
            Err(_) => {
                *slot = None;
                Err(SendError::temporary(
                    "amqp: the broker did not confirm the message",
                ))
            }
            Ok(Err(e)) => {
                *slot = None;
                Err(SendError::temporary(format!("amqp: {e}")))
            }
            Ok(Ok(Confirmation::Ack(None) | Confirmation::NotRequested)) => Ok(None),
            Ok(Ok(Confirmation::Ack(Some(returned)))) => Err(SendError::permanent(format!(
                "amqp: the broker could not route the message: {} {}",
                returned.reply_code, returned.reply_text
            ))),
            Ok(Ok(Confirmation::Nack(_))) => {
                Err(SendError::temporary("amqp: the broker refused the message"))
            }
        }
    }
}

/// Registers the `amqp` source and destination.
pub(crate) fn register(registry: &mut Registry) {
    registry
        .add_source("amqp", |config: &SourceConfig| {
            let settings: AmqpSourceSettings = parse(&config.settings, "amqp source")?;
            Ok(Arc::new(AmqpSource::new(settings)?) as Arc<dyn SourceConnector>)
        })
        .add_destination("amqp", |config: &DestinationConfig| {
            let settings: AmqpDestinationSettings = parse(&config.settings, "amqp destination")?;
            Ok(Arc::new(AmqpDestination::new(settings)?) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings<T: serde::de::DeserializeOwned>(json: serde_json::Value) -> T {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn validates_settings() {
        let ok: AmqpSourceSettings = settings(serde_json::json!({
            "url": "amqp://oxim@rabbit.example.org:5672/%2f",
            "queue": "lab-results",
            "password_env": "OXIM_AMQP_PASSWORD",
        }));
        let source = AmqpSource::new(ok).unwrap();
        assert_eq!(source.broker.describe(), "rabbit.example.org:5672");
        assert_eq!(source.broker.uri.authority.userinfo.username, "oxim");

        for bad in [
            serde_json::json!({"url": "http://x", "queue": "q"}),
            serde_json::json!({"url": "amqp://x", "queue": ""}),
            serde_json::json!({"url": "amqp://u:p@x", "queue": "q", "password": "other"}),
            serde_json::json!({"url": "amqp://x", "queue": "q", "tls": {}}),
        ] {
            let parsed: AmqpSourceSettings = settings(bad.clone());
            assert!(AmqpSource::new(parsed).is_err(), "{bad}");
        }
        let missing_target: AmqpDestinationSettings =
            settings(serde_json::json!({"url": "amqp://x"}));
        assert!(AmqpDestination::new(missing_target).is_err());
        let templated: AmqpDestinationSettings = settings(serde_json::json!({
            "url": "amqp://x", "exchange": "lab", "routing_key": "results.{MSH-3}"
        }));
        assert!(AmqpDestination::new(templated).is_ok());
    }

    #[test]
    fn maps_headers_to_metadata() {
        let mut headers = FieldTable::default();
        headers.insert("site".into(), AMQPValue::LongString("LAB1".into()));
        headers.insert("count".into(), AMQPValue::LongInt(3));
        headers.insert(
            "blob".into(),
            AMQPValue::LongString(vec![0xff, 0x00].into()),
        );
        let mut metadata = BTreeMap::new();
        header_metadata(Some(&headers), &mut metadata);
        assert_eq!(
            metadata.get("amqp.header.site").map(String::as_str),
            Some("LAB1")
        );
        assert_eq!(
            metadata.get("amqp.header.count").map(String::as_str),
            Some("3")
        );
        assert!(!metadata.contains_key("amqp.header.blob"));
        assert_eq!(
            media_type(Some(DataType::Hl7V2)),
            "x-application/hl7-v2+er7"
        );
    }

    #[test]
    fn finds_passwords_in_urls() {
        assert!(url_has_password("amqp://u:p@host:5672/%2f"));
        assert!(!url_has_password("amqp://u@host:5672/%2f"));
        assert!(!url_has_password("amqp://host:5672/%2f"));
        assert!(!url_has_password("amqp://host/a@b"));
    }
}
