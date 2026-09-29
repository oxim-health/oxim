//! Apache Kafka with rskafka (pure Rust).
//!
//! The source reads assigned partitions directly and keeps its position in
//! an offsets file that is written after each stored batch; Kafka consumer
//! groups are not used. The producer waits for all in-sync replicas
//! (`acks=all`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_connectors::values::DeliveryValues;
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_store::Delivery;
use rskafka::BackoffConfig;
use rskafka::client::error::{Error as KafkaError, ProtocolError};
use rskafka::client::partition::{Compression, OffsetAt, PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder, Credentials, SaslConfig};
use rskafka::record::Record;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::common::{Secret, metadata_text, parse};
use crate::template::Template;

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

fn default_max_wait() -> DurationText {
    DurationText(Duration::from_millis(500))
}

fn default_max_bytes() -> usize {
    1024 * 1024
}

/// SASL authentication.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaslSettings {
    /// `plain`, `scram-sha-256` or `scram-sha-512`.
    pub mechanism: String,
    /// User name.
    pub username: String,
    /// Password, inline (discouraged).
    #[serde(default)]
    pub password: Option<String>,
    /// Environment variable holding the password.
    #[serde(default)]
    pub password_env: Option<String>,
}

/// How to reach the cluster.
#[derive(Debug, Clone)]
struct Cluster {
    brokers: Vec<String>,
    client_id: Option<String>,
    tls: Option<Arc<rustls::ClientConfig>>,
    sasl: Option<(String, String, Secret)>,
    timeout: Duration,
}

impl Cluster {
    fn new(
        brokers: &[String],
        client_id: Option<String>,
        tls: Option<&ClientTlsSettings>,
        sasl: Option<&SaslSettings>,
        timeout: Duration,
    ) -> Result<Self, EngineError> {
        if brokers.is_empty() {
            return Err(EngineError::Config(
                "kafka needs at least one broker".into(),
            ));
        }
        let tls = tls
            .map(|settings| {
                if settings.server_name.is_some() {
                    return Err(EngineError::Config(
                        "kafka verifies each broker's certificate against its address; server_name is not supported"
                            .into(),
                    ));
                }
                client_config(settings).map(Arc::new)
            })
            .transpose()?;
        let sasl = sasl
            .map(|sasl| {
                let mechanism = sasl.mechanism.to_ascii_lowercase();
                if !["plain", "scram-sha-256", "scram-sha-512"].contains(&mechanism.as_str()) {
                    return Err(EngineError::Config(format!(
                        "kafka sasl mechanism must be plain, scram-sha-256 or scram-sha-512, not {:?}",
                        sasl.mechanism
                    )));
                }
                let secret = Secret::new(sasl.password.clone(), sasl.password_env.clone(), "password")?;
                Ok((mechanism, sasl.username.clone(), secret))
            })
            .transpose()?;
        Ok(Self {
            brokers: brokers.to_vec(),
            client_id,
            tls,
            sasl,
            timeout,
        })
    }

    async fn connect(&self, default_client_id: String) -> Result<Client, String> {
        let mut builder = ClientBuilder::new(self.brokers.clone())
            .client_id(self.client_id.clone().unwrap_or(default_client_id))
            .backoff_config(BackoffConfig {
                init_backoff: Duration::from_millis(100),
                max_backoff: Duration::from_secs(5),
                base: 2.0,
                deadline: Some(self.timeout),
            });
        if let Some(tls) = &self.tls {
            builder = builder.tls_config(tls.clone());
        }
        if let Some((mechanism, username, secret)) = &self.sasl {
            let password = secret.resolve()?.unwrap_or_default();
            let credentials = Credentials::new(username.clone(), password);
            builder = builder.sasl_config(match mechanism.as_str() {
                "plain" => SaslConfig::Plain(credentials),
                "scram-sha-256" => SaslConfig::ScramSha256(credentials),
                _ => SaslConfig::ScramSha512(credentials),
            });
        }
        tokio::time::timeout(self.timeout, builder.build())
            .await
            .map_err(|_| "connecting to the brokers timed out".to_owned())?
            .map_err(|e| format!("cannot connect to the brokers: {e}"))
    }
}

/// Where a new partition starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// The oldest record still kept.
    #[default]
    Earliest,
    /// Only records produced from now on.
    Latest,
}

/// Settings of the `kafka` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KafkaSourceSettings {
    /// Bootstrap brokers, `host:port`.
    pub brokers: Vec<String>,
    /// The topic to read.
    pub topic: String,
    /// Partitions to read; all partitions of the topic by default.
    #[serde(default)]
    pub partitions: Option<Vec<i32>>,
    /// Where partitions without a stored offset start.
    #[serde(default)]
    pub start: Start,
    /// File holding the next offset of each partition.
    pub offsets_file: PathBuf,
    /// Longest wait of one fetch for new records.
    #[serde(default = "default_max_wait")]
    pub max_wait: DurationText,
    /// Most bytes fetched at once per partition.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: usize,
    /// Client identifier; `oxim-<channel>` by default.
    #[serde(default)]
    pub client_id: Option<String>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// SASL authentication.
    #[serde(default)]
    pub sasl: Option<SaslSettings>,
    /// Limit for connecting and for retrying failed requests.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

/// The next offset of each partition, persisted as JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Offsets {
    topic: String,
    partitions: BTreeMap<i32, i64>,
}

impl Offsets {
    fn load(path: &Path, topic: &str) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let offsets: Self = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                if offsets.topic != topic {
                    return Err(format!(
                        "{} holds offsets of topic {:?}, not {topic:?}",
                        path.display(),
                        offsets.topic
                    ));
                }
                Ok(offsets)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                topic: topic.to_owned(),
                partitions: BTreeMap::new(),
            }),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the file atomically: a temporary file, then a rename.
    fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, json)
            .and_then(|()| std::fs::rename(&temporary, path))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

/// Reads a topic and keeps offsets in a file.
#[derive(Debug)]
pub struct KafkaSource {
    settings: KafkaSourceSettings,
    cluster: Cluster,
}

impl KafkaSource {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: KafkaSourceSettings) -> Result<Self, EngineError> {
        if settings.topic.is_empty() {
            return Err(EngineError::Config("kafka source needs a topic".into()));
        }
        if settings.max_bytes == 0 || i32::try_from(settings.max_bytes).is_err() {
            return Err(EngineError::Config(
                "kafka max_bytes must be between 1 and 2147483647".into(),
            ));
        }
        let cluster = Cluster::new(
            &settings.brokers,
            settings.client_id.clone(),
            settings.tls.as_ref(),
            settings.sasl.as_ref(),
            settings.timeout.0,
        )?;
        Ok(Self { settings, cluster })
    }
}

fn error(e: impl std::fmt::Display) -> ConnectorError {
    ConnectorError(format!("kafka: {e}"))
}

fn is_offset_out_of_range(e: &KafkaError) -> bool {
    matches!(
        e,
        KafkaError::ServerError {
            protocol_error: ProtocolError::OffsetOutOfRange,
            ..
        }
    )
}

/// Reads one partition until the channel stops.
#[allow(clippy::too_many_arguments)]
async fn read_partition(
    context: SourceContext,
    partition: PartitionClient,
    offsets: Arc<Mutex<Offsets>>,
    path: PathBuf,
    start: Start,
    max_bytes: i32,
    max_wait: Duration,
    peer: String,
) -> Result<(), ConnectorError> {
    let id = partition.partition();
    let stored = offsets.lock().await.partitions.get(&id).copied();
    let mut offset = match stored {
        Some(offset) => offset,
        None => partition
            .get_offset(match start {
                Start::Earliest => OffsetAt::Earliest,
                Start::Latest => OffsetAt::Latest,
            })
            .await
            .map_err(error)?,
    };
    let max_wait_ms = i32::try_from(max_wait.as_millis()).unwrap_or(i32::MAX);
    loop {
        let fetched = tokio::select! {
            () = context.cancelled() => return Ok(()),
            fetched = partition.fetch_records(offset, 1..max_bytes, max_wait_ms) => fetched,
        };
        let records = match fetched {
            Ok((records, _high_watermark)) => records,
            Err(e) if is_offset_out_of_range(&e) => {
                let earliest = partition
                    .get_offset(OffsetAt::Earliest)
                    .await
                    .map_err(error)?;
                warn!(channel = %context.channel(), partition = id, offset, earliest, "offset out of range; continuing at the earliest kept record");
                offset = earliest;
                continue;
            }
            Err(e) => return Err(error(e)),
        };
        let mut stored_any = false;
        for found in records {
            // Compressed batches may start before the requested offset.
            if found.offset < offset {
                continue;
            }
            let record = found.record;
            let mut metadata = BTreeMap::new();
            metadata.insert("kafka.topic".to_owned(), partition.topic().to_owned());
            metadata.insert("kafka.partition".to_owned(), id.to_string());
            metadata.insert("kafka.offset".to_owned(), found.offset.to_string());
            metadata.insert(
                "kafka.timestamp".to_owned(),
                record.timestamp.timestamp_millis().to_string(),
            );
            for (name, value) in &record.headers {
                if let Some(text) = metadata_text(value) {
                    metadata.insert(format!("kafka.header.{name}"), text);
                }
            }
            let info = SubmitInfo {
                peer: Some(peer.clone()),
                correlation_id: record.key.as_deref().and_then(metadata_text),
                metadata,
                ..SubmitInfo::default()
            };
            context
                .submit(record.value.unwrap_or_default(), info)
                .await
                .map_err(|e| error(format!("the record could not be stored: {e}")))?;
            offset = found.offset + 1;
            stored_any = true;
        }
        if stored_any {
            let mut guard = offsets.lock().await;
            guard.partitions.insert(id, offset);
            let snapshot = guard.clone();
            drop(guard);
            let path = path.clone();
            tokio::task::spawn_blocking(move || snapshot.save(&path))
                .await
                .map_err(error)?
                .map_err(error)?;
            debug!(channel = %context.channel(), partition = id, offset, "stored records");
        }
    }
}

#[async_trait]
impl SourceConnector for KafkaSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let s = &self.settings;
        let client = self
            .cluster
            .connect(format!("oxim-{}", context.channel()))
            .await
            .map_err(error)?;
        let partitions: Vec<i32> = match &s.partitions {
            Some(list) => list.clone(),
            None => client
                .list_topics()
                .await
                .map_err(error)?
                .into_iter()
                .find(|topic| topic.name == s.topic)
                .ok_or_else(|| error(format!("topic {} does not exist", s.topic)))?
                .partitions
                .into_iter()
                .collect(),
        };
        let path = s.offsets_file.clone();
        let topic = s.topic.clone();
        let offsets = tokio::task::spawn_blocking(move || Offsets::load(&path, &topic))
            .await
            .map_err(error)?
            .map_err(error)?;
        let offsets = Arc::new(Mutex::new(offsets));
        let peer = self.cluster.brokers.join(",");
        let max_bytes = i32::try_from(s.max_bytes).unwrap_or(i32::MAX);
        let mut readers = JoinSet::new();
        for id in partitions {
            let partition = client
                .partition_client(s.topic.clone(), id, UnknownTopicHandling::Retry)
                .await
                .map_err(error)?;
            readers.spawn(read_partition(
                context.clone(),
                partition,
                offsets.clone(),
                s.offsets_file.clone(),
                s.start,
                max_bytes,
                s.max_wait.0,
                peer.clone(),
            ));
        }
        // The first failing partition stops the others; the engine starts
        // the source again after a delay.
        while let Some(result) = readers.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    readers.abort_all();
                    return Err(e);
                }
                Err(e) => {
                    readers.abort_all();
                    return Err(error(e));
                }
            }
        }
        Ok(())
    }
}

/// Settings of the `kafka` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KafkaDestinationSettings {
    /// Bootstrap brokers, `host:port`.
    pub brokers: Vec<String>,
    /// The topic to write.
    pub topic: String,
    /// The partition to write.
    #[serde(default)]
    pub partition: i32,
    /// Record key; `{...}` placeholders take delivery values.
    #[serde(default)]
    pub key: Option<String>,
    /// `none`, `gzip`, `lz4`, `snappy` or `zstd`.
    #[serde(default)]
    pub compression: Option<String>,
    /// Client identifier; `oxim-<channel>-<destination>` by default.
    #[serde(default)]
    pub client_id: Option<String>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// SASL authentication.
    #[serde(default)]
    pub sasl: Option<SaslSettings>,
    /// Limit for connecting and for retrying a failed produce request.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

fn compression(name: Option<&str>) -> Result<Compression, EngineError> {
    Ok(match name.unwrap_or("none") {
        "none" => Compression::NoCompression,
        "gzip" => Compression::Gzip,
        "lz4" => Compression::Lz4,
        "snappy" => Compression::Snappy,
        "zstd" => Compression::Zstd,
        other => {
            return Err(EngineError::Config(format!(
                "kafka compression must be none, gzip, lz4, snappy or zstd, not {other:?}"
            )));
        }
    })
}

/// Writes deliveries to a partition.
#[derive(Debug)]
pub struct KafkaDestination {
    settings: KafkaDestinationSettings,
    key: Option<Template>,
    compression: Compression,
    cluster: Cluster,
    partition: Mutex<Option<PartitionClient>>,
}

impl KafkaDestination {
    /// Validates the settings and reads the TLS files.
    pub fn new(settings: KafkaDestinationSettings) -> Result<Self, EngineError> {
        if settings.topic.is_empty() {
            return Err(EngineError::Config(
                "kafka destination needs a topic".into(),
            ));
        }
        let key = settings
            .key
            .as_deref()
            .map(|key| Template::parse(key, "kafka key"))
            .transpose()?;
        let compression = compression(settings.compression.as_deref())?;
        let cluster = Cluster::new(
            &settings.brokers,
            settings.client_id.clone(),
            settings.tls.as_ref(),
            settings.sasl.as_ref(),
            settings.timeout.0,
        )?;
        Ok(Self {
            settings,
            key,
            compression,
            cluster,
            partition: Mutex::new(None),
        })
    }
}

/// The current time as a record timestamp, in milliseconds.
fn now() -> rskafka::chrono::DateTime<rskafka::chrono::Utc> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
    rskafka::chrono::DateTime::from_timestamp_millis(millis).unwrap_or_default()
}

/// Errors that repeating the same record cannot fix.
fn is_permanent(e: &KafkaError) -> bool {
    matches!(
        e,
        KafkaError::ServerError {
            protocol_error: ProtocolError::MessageTooLarge
                | ProtocolError::RecordListTooLarge
                | ProtocolError::CorruptMessage
                | ProtocolError::InvalidRecord,
            ..
        }
    )
}

#[async_trait]
impl DestinationConnector for KafkaDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let s = &self.settings;
        let key = self
            .key
            .as_ref()
            .map(|key| key.render(&mut DeliveryValues::new(delivery)))
            .transpose()
            .map_err(|e| SendError::permanent(format!("kafka key: {e}")))?;
        let mut slot = self.partition.lock().await;
        if slot.is_none() {
            let client = self
                .cluster
                .connect(format!(
                    "oxim-{}-{}",
                    delivery.channel, delivery.destination
                ))
                .await
                .map_err(|e| SendError::temporary(format!("kafka: {e}")))?;
            let partition = client
                .partition_client(s.topic.clone(), s.partition, UnknownTopicHandling::Retry)
                .await
                .map_err(|e| SendError::temporary(format!("kafka: {e}")))?;
            *slot = Some(partition);
        }
        let Some(partition) = slot.as_ref() else {
            return Err(SendError::temporary("kafka: no connection"));
        };
        let mut headers = BTreeMap::new();
        headers.insert(
            "oxim-message-id".to_owned(),
            delivery.message_id.to_string().into_bytes(),
        );
        headers.insert(
            "oxim-channel".to_owned(),
            delivery.channel.to_string().into_bytes(),
        );
        let record = Record {
            key: key.map(String::into_bytes),
            value: Some(delivery.payload.clone()),
            headers,
            timestamp: now(),
        };
        match partition.produce(vec![record], self.compression).await {
            Ok(offsets) => Ok(offsets
                .first()
                .map(|offset| format!("offset {offset}").into_bytes())),
            Err(e) if is_permanent(&e) => Err(SendError::permanent(format!("kafka: {e}"))),
            Err(e) => {
                *slot = None;
                Err(SendError::temporary(format!("kafka: {e}")))
            }
        }
    }
}

/// Registers the `kafka` source and destination.
pub(crate) fn register(registry: &mut Registry) {
    registry
        .add_source("kafka", |config: &SourceConfig| {
            let settings: KafkaSourceSettings = parse(&config.settings, "kafka source")?;
            Ok(Arc::new(KafkaSource::new(settings)?) as Arc<dyn SourceConnector>)
        })
        .add_destination("kafka", |config: &DestinationConfig| {
            let settings: KafkaDestinationSettings = parse(&config.settings, "kafka destination")?;
            Ok(Arc::new(KafkaDestination::new(settings)?) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_round_trip_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.json");
        let mut offsets = Offsets::load(&path, "lab-results").unwrap();
        assert!(offsets.partitions.is_empty());
        offsets.partitions.insert(0, 42);
        offsets.partitions.insert(3, 7);
        offsets.save(&path).unwrap();
        assert_eq!(Offsets::load(&path, "lab-results").unwrap(), offsets);
        assert!(Offsets::load(&path, "other-topic").is_err());
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn validates_settings() {
        let source: KafkaSourceSettings = serde_json::from_value(serde_json::json!({
            "brokers": ["kafka-1:9092"],
            "topic": "lab-results",
            "offsets_file": "offsets.json",
            "sasl": {"mechanism": "SCRAM-SHA-512", "username": "oxim", "password_env": "OXIM_KAFKA_PASSWORD"},
        }))
        .unwrap();
        assert!(KafkaSource::new(source).is_ok());
        for bad in [
            serde_json::json!({"brokers": [], "topic": "t", "offsets_file": "o"}),
            serde_json::json!({"brokers": ["b:9092"], "topic": "", "offsets_file": "o"}),
            serde_json::json!({"brokers": ["b:9092"], "topic": "t", "offsets_file": "o", "max_bytes": 0}),
            serde_json::json!({"brokers": ["b:9092"], "topic": "t", "offsets_file": "o", "sasl": {"mechanism": "gssapi", "username": "u"}}),
        ] {
            let settings: KafkaSourceSettings = serde_json::from_value(bad.clone()).unwrap();
            assert!(KafkaSource::new(settings).is_err(), "{bad}");
        }
        let destination: KafkaDestinationSettings = serde_json::from_value(serde_json::json!({
            "brokers": ["kafka-1:9092"], "topic": "t", "compression": "brotli"
        }))
        .unwrap();
        assert!(KafkaDestination::new(destination).is_err());
        let destination: KafkaDestinationSettings = serde_json::from_value(serde_json::json!({
            "brokers": ["kafka-1:9092"], "topic": "t", "key": "{PID-3.1}", "compression": "zstd"
        }))
        .unwrap();
        assert!(KafkaDestination::new(destination).is_ok());
    }
}
