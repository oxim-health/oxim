//! Shared test helpers: an engine with the connectors of this crate and a
//! recording destination.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, SendError, SystemClock,
    async_trait,
};
use oxim_model::{ChannelId, ConnectorId, DataType, MessageId};
use oxim_store::{Delivery, SqliteStore};

/// A destination that records every payload.
#[derive(Debug, Default)]
pub struct Recorder {
    pub sent: Mutex<Vec<Vec<u8>>>,
}

impl Recorder {
    pub fn payloads(&self) -> Vec<Vec<u8>> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.sent.lock().unwrap().push(delivery.payload.clone());
        Ok(None)
    }
}

/// An engine with every connector of this crate plus a `recorder`
/// destination.
pub async fn engine(recorder: Arc<Recorder>) -> Engine {
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    registry.add_destination("recorder", move |_| {
        Ok(recorder.clone() as Arc<dyn DestinationConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    options.shutdown_grace = Duration::from_secs(5);
    options.source_restart_delay = Duration::from_millis(100);
    Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap()
}

/// A channel whose destination is the recorder.
pub fn channel(source_yaml: &str) -> ChannelConfig {
    ChannelConfig::from_yaml(&format!(
        "id: test\nsource:\n{source_yaml}\ndestinations:\n  - id: out\n    type: recorder\n"
    ))
    .unwrap()
}

/// A local port that was free a moment ago.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Polls `condition` every 10 ms for up to 10 s.
pub async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

/// Connects to `port`, retrying while the listener starts.
pub async fn connect(port: u16) -> tokio::net::TcpStream {
    for _ in 0..500 {
        if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing is listening on port {port}");
}

/// A delivery of `payload` for direct destination tests.
pub fn delivery(n: u64, payload: &[u8], data_type: DataType) -> Delivery {
    Delivery {
        message_id: MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        channel: ChannelId::new("lab").unwrap(),
        destination: ConnectorId::new("out").unwrap(),
        attempts: 0,
        payload: payload.to_vec(),
        data_type: Some(data_type),
    }
}

/// Builds a destination of this crate from YAML settings.
pub fn destination(kind: &str, settings_yaml: &str) -> Arc<dyn DestinationConnector> {
    let config = ChannelConfig::from_yaml(&format!(
        "id: lab\nsource: {{type: mllp, data_type: hl7v2, settings: {{listen: '127.0.0.1:0'}}}}\ndestinations:\n  - id: out\n    type: {kind}\n    settings:\n{settings_yaml}\n"
    ))
    .unwrap();
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    registry.destination(&config.destinations[0]).unwrap()
}
