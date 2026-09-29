//! Round trips against real brokers. Each test runs only when its
//! environment variable names a broker, for example:
//!
//! ```text
//! OXIM_TEST_MQTT=127.0.0.1:1883
//! OXIM_TEST_AMQP=amqp://guest:guest@127.0.0.1:5672/%2f
//! OXIM_TEST_KAFKA=127.0.0.1:9092
//! OXIM_TEST_NATS=nats://127.0.0.1:4222   (JetStream enabled)
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, SendError, SystemClock,
    async_trait,
};
use oxim_model::{ChannelId, ConnectorId, DataType, MessageId};
use oxim_store::{Delivery, SqliteStore};

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

fn registry(recorder: Arc<Recorder>) -> Registry {
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    oxim_connectors_messaging::register(&mut registry);
    registry.add_destination("recorder", move |_| {
        Ok(recorder.clone() as Arc<dyn DestinationConnector>)
    });
    registry
}

fn delivery(n: u64, payload: &[u8]) -> Delivery {
    Delivery {
        message_id: MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        channel: ChannelId::new("lab").unwrap(),
        destination: ConnectorId::new("out").unwrap(),
        attempts: 0,
        payload: payload.to_vec(),
        data_type: Some(DataType::Raw),
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}-{}", std::process::id(), time_suffix())
}

fn time_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() % 1_000_000)
}

/// Publishes two messages with the destination and reads them back with
/// the source channel. `source_first` deploys the source before publishing
/// (for systems that do not keep messages for absent subscribers).
async fn round_trip(
    kind: &str,
    source_settings: &str,
    destination_settings: &str,
    source_first: bool,
) {
    let recorder = Arc::new(Recorder::default());
    let registry_for_destination = registry(Arc::new(Recorder::default()));
    let config = ChannelConfig::from_yaml(&format!(
        "id: out\nsource: {{type: timer, data_type: raw, settings: {{interval: 1h}}}}\ndestinations:\n  - {{id: out, type: {kind}, settings: {destination_settings}}}\n"
    ))
    .unwrap();
    let destination = registry_for_destination
        .destination(&config.destinations[0])
        .unwrap();
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    options.source_restart_delay = Duration::from_millis(200);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry(recorder.clone()),
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    let source = ChannelConfig::from_yaml(&format!(
        "id: in\nsource: {{type: {kind}, data_type: raw, settings: {source_settings}}}\ndestinations:\n  - {{id: rec, type: recorder}}\n"
    ))
    .unwrap();
    if source_first {
        engine.deploy(source.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    destination
        .send(&delivery(1, b"first synthetic message"))
        .await
        .unwrap();
    destination
        .send(&delivery(2, b"second synthetic message"))
        .await
        .unwrap();
    if !source_first {
        engine.deploy(source).await.unwrap();
    }
    for _ in 0..1500 {
        if recorder.sent.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let received = recorder.sent.lock().unwrap().clone();
    assert_eq!(
        received,
        [
            b"first synthetic message".to_vec(),
            b"second synthetic message".to_vec()
        ]
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mqtt_round_trip() {
    let Ok(broker) = std::env::var("OXIM_TEST_MQTT") else {
        eprintln!("OXIM_TEST_MQTT is not set; skipping");
        return;
    };
    let topic = unique("oxim/test");
    round_trip(
        "mqtt",
        &format!(
            "{{broker: '{broker}', topics: ['{topic}'], client_id: '{}'}}",
            unique("oxim-in")
        ),
        &format!("{{broker: '{broker}', topic: '{topic}'}}"),
        true,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn amqp_round_trip() {
    let Ok(url) = std::env::var("OXIM_TEST_AMQP") else {
        eprintln!("OXIM_TEST_AMQP is not set; skipping");
        return;
    };
    let queue = unique("oxim-test");
    let connection = lapin::Connection::connect(&url, lapin::ConnectionProperties::default())
        .await
        .unwrap();
    let channel = connection.create_channel().await.unwrap();
    channel
        .queue_declare(
            queue.as_str().into(),
            lapin::options::QueueDeclareOptions {
                auto_delete: true,
                ..lapin::options::QueueDeclareOptions::default()
            },
            lapin::types::FieldTable::default(),
        )
        .await
        .unwrap();
    round_trip(
        "amqp",
        &format!("{{url: '{url}', queue: '{queue}'}}"),
        &format!("{{url: '{url}', routing_key: '{queue}'}}"),
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kafka_round_trip() {
    let Ok(broker) = std::env::var("OXIM_TEST_KAFKA") else {
        eprintln!("OXIM_TEST_KAFKA is not set; skipping");
        return;
    };
    let topic = unique("oxim-test");
    let client = rskafka::client::ClientBuilder::new(vec![broker.clone()])
        .build()
        .await
        .unwrap();
    client
        .controller_client()
        .unwrap()
        .create_topic(&topic, 1, 1, 5_000)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let offsets = dir
        .path()
        .join("offsets.json")
        .display()
        .to_string()
        .replace('\\', "/");
    round_trip(
        "kafka",
        &format!("{{brokers: ['{broker}'], topic: '{topic}', offsets_file: '{offsets}'}}"),
        &format!("{{brokers: ['{broker}'], topic: '{topic}', key: '{{$message_id}}'}}"),
        false,
    )
    .await;
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("offsets.json")).unwrap()).unwrap();
    assert_eq!(stored["partitions"]["0"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn nats_round_trips() {
    let Ok(url) = std::env::var("OXIM_TEST_NATS") else {
        eprintln!("OXIM_TEST_NATS is not set; skipping");
        return;
    };
    // Core NATS: the subscriber must be there first.
    let subject = unique("oxim.test").replace('-', ".");
    round_trip(
        "nats",
        &format!("{{servers: ['{url}'], subject: '{subject}'}}"),
        &format!("{{servers: ['{url}'], subject: '{subject}'}}"),
        true,
    )
    .await;

    // JetStream: the stream keeps the messages until the consumer reads them.
    let stream = unique("OXIM_TEST").replace('-', "_");
    let subject = format!("{}.results", stream.to_lowercase());
    let client = async_nats::connect(&url).await.unwrap();
    async_nats::jetstream::new(client)
        .create_stream(async_nats::jetstream::stream::Config {
            name: stream.clone(),
            subjects: vec![subject.clone()],
            ..async_nats::jetstream::stream::Config::default()
        })
        .await
        .unwrap();
    round_trip(
        "nats",
        &format!("{{servers: ['{url}'], subject: '{subject}', jetstream: {{stream: '{stream}', consumer: oxim}}}}"),
        &format!("{{servers: ['{url}'], subject: '{subject}', jetstream: true}}"),
        false,
    )
    .await;
}
