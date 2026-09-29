//! The MQTT source and destination against a small in-process broker.

use std::sync::Mutex as StdMutex;

use oxim_core::{ChannelConfig, Engine, EngineOptions, SystemClock};
use oxim_model::{ChannelId, ConnectorId, DataType, MessageId};
use oxim_store::SqliteStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::codec::{Decoder, encode};
use super::*;

/// What the broker saw.
#[derive(Debug, Default)]
struct Log {
    clients: Vec<String>,
    pubacks: Vec<u16>,
    publishes: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone)]
struct Broker {
    log: Arc<StdMutex<Log>>,
    /// Messages published to subscribers, with QoS 1.
    outgoing: Vec<(String, Vec<u8>)>,
    /// Whether client publishes are acknowledged.
    ack_publishes: bool,
    /// Close each connection after this many client publishes.
    close_after: Option<usize>,
}

impl Broker {
    fn new() -> Self {
        Self {
            log: Arc::new(StdMutex::new(Log::default())),
            outgoing: Vec::new(),
            ack_publishes: true,
            close_after: None,
        }
    }

    async fn start(self) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(self.clone().serve(stream));
            }
        });
        address
    }

    async fn serve(self, mut stream: TcpStream) {
        let mut decoder = Decoder::new(DEFAULT_MAX_PACKET);
        let mut buffer = [0u8; 4096];
        let mut published = 0;
        loop {
            let packet = loop {
                match decoder.next() {
                    Ok(Some(packet)) => break packet,
                    Ok(None) => {}
                    Err(_) => return,
                }
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => decoder.push(&buffer[..n]),
                }
            };
            let replies = match packet {
                Packet::Connect { client_id, .. } => {
                    self.log.lock().unwrap().clients.push(client_id);
                    vec![Packet::ConnAck {
                        session_present: false,
                        code: 0,
                    }]
                }
                Packet::Subscribe { packet_id, filters } => {
                    let mut replies = vec![Packet::SubAck {
                        packet_id,
                        codes: filters.iter().map(|(_, qos)| *qos).collect(),
                    }];
                    for (i, (topic, payload)) in self.outgoing.iter().enumerate() {
                        replies.push(Packet::Publish {
                            dup: false,
                            qos: 1,
                            retain: false,
                            topic: topic.clone(),
                            packet_id: Some(100 + u16::try_from(i).unwrap()),
                            payload: payload.clone(),
                        });
                    }
                    replies
                }
                Packet::PubAck(id) => {
                    self.log.lock().unwrap().pubacks.push(id);
                    Vec::new()
                }
                Packet::Publish {
                    topic,
                    payload,
                    packet_id,
                    ..
                } => {
                    self.log.lock().unwrap().publishes.push((topic, payload));
                    published += 1;
                    let mut replies = Vec::new();
                    if self.ack_publishes
                        && let Some(id) = packet_id
                    {
                        replies.push(Packet::PubAck(id));
                    }
                    replies
                }
                Packet::PingReq => vec![Packet::PingResp],
                Packet::Disconnect => return,
                _ => Vec::new(),
            };
            for reply in replies {
                if stream.write_all(&encode(&reply).unwrap()).await.is_err() {
                    return;
                }
            }
            if self.close_after.is_some_and(|limit| published >= limit) {
                return;
            }
        }
    }
}

#[derive(Debug, Default)]
struct Recorder {
    sent: StdMutex<Vec<Vec<u8>>>,
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.sent.lock().unwrap().push(delivery.payload.clone());
        Ok(None)
    }
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn source_acknowledges_stored_messages() {
    let mut broker = Broker::new();
    broker.outgoing = vec![
        ("lab/chem-1".into(), b"first".to_vec()),
        ("lab/chem-2".into(), b"second".to_vec()),
    ];
    let log = broker.log.clone();
    let address = broker.start().await;

    let recorder = Arc::new(Recorder::default());
    let mut registry = Registry::new();
    register(&mut registry);
    let captured = recorder.clone();
    registry.add_destination("recorder", move |_| {
        Ok(captured.clone() as Arc<dyn DestinationConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    engine
        .deploy(
            ChannelConfig::from_yaml(&format!(
                "id: devices
source:
  type: mqtt
  data_type: raw
  settings: {{broker: '{address}', topics: ['lab/#'], keep_alive: 1s}}
destinations:
  - {{id: out, type: recorder}}
"
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    wait_until("both messages are acknowledged", || {
        log.lock().unwrap().pubacks == [100, 101]
    })
    .await;
    wait_until("both messages are delivered", || {
        recorder.sent.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(log.lock().unwrap().clients, ["oxim-devices"]);
    // The keep-alive keeps the connection open past its interval.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(log.lock().unwrap().clients.len(), 1);
    engine.shutdown().await;
}

fn delivery(n: u64, payload: &[u8]) -> Delivery {
    Delivery {
        message_id: MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        channel: ChannelId::new("lab").unwrap(),
        destination: ConnectorId::new("broker").unwrap(),
        attempts: 0,
        payload: payload.to_vec(),
        data_type: Some(DataType::Hl7V2),
    }
}

const HL7: &[u8] = b"MSH|^~\\&|CHEM|LAB|LIS|HOSP|20260929120000||ORU^R01|C1|P|2.5.1\r";

fn destination(settings: serde_json::Value) -> MqttDestination {
    MqttDestination::new(serde_json::from_value(settings).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_waits_for_the_broker() {
    let broker = Broker::new();
    let log = broker.log.clone();
    let address = broker.start().await;
    let sender = destination(serde_json::json!({
        "broker": address,
        "topic": "results/{MSH-3}",
    }));
    sender.send(&delivery(1, HL7)).await.unwrap();
    sender.send(&delivery(2, HL7)).await.unwrap();
    {
        let log = log.lock().unwrap();
        assert_eq!(log.publishes.len(), 2);
        assert_eq!(log.publishes[0].0, "results/CHEM");
        assert_eq!(log.publishes[0].1, HL7);
        // One connection for both deliveries.
        assert_eq!(log.clients, ["oxim-lab-broker"]);
    }

    // A message without the topic value cannot be sent anywhere.
    let error = sender.send(&delivery(3, b"not hl7")).await.unwrap_err();
    assert!(error.permanent, "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_acknowledgment_is_retried() {
    let mut broker = Broker::new();
    broker.ack_publishes = false;
    let address = broker.start().await;
    let sender = destination(serde_json::json!({
        "broker": address,
        "topic": "results",
        "ack_timeout": "200ms",
    }));
    let error = sender.send(&delivery(1, HL7)).await.unwrap_err();
    assert!(
        !error.permanent && error.message.contains("acknowledge"),
        "{error}"
    );

    // QoS 0 does not wait.
    let fire_and_forget = destination(serde_json::json!({
        "broker": address,
        "topic": "results",
        "qos": 0,
    }));
    fire_and_forget.send(&delivery(2, HL7)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_connection_is_replaced() {
    let mut broker = Broker::new();
    broker.close_after = Some(1);
    let log = broker.log.clone();
    let address = broker.start().await;
    let sender = destination(serde_json::json!({"broker": address, "topic": "results"}));
    sender.send(&delivery(1, HL7)).await.unwrap();
    // The broker closed the connection; the next delivery reconnects.
    sender.send(&delivery(2, HL7)).await.unwrap();
    assert_eq!(log.lock().unwrap().publishes.len(), 2);
    assert_eq!(log.lock().unwrap().clients.len(), 2);

    // Nobody listening: a temporary failure.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let nowhere = closed.local_addr().unwrap().to_string();
    drop(closed);
    let unreachable = destination(serde_json::json!({
        "broker": nowhere,
        "topic": "results",
        "connect_timeout": "500ms",
    }));
    assert!(
        !unreachable
            .send(&delivery(3, HL7))
            .await
            .unwrap_err()
            .permanent
    );
}

#[test]
fn settings_are_checked() {
    let mut registry = Registry::new();
    register(&mut registry);
    for source in [
        "{broker: 'b:1883', topics: []}",
        "{broker: 'b:1883', topics: [a], qos: 2}",
        "{broker: 'b:1883', topics: [a], password: x, password_env: Y}",
        "{broker: 'b:1883', topics: [a], color: red}",
    ] {
        let config = ChannelConfig::from_yaml(&format!(
            "id: x\nsource: {{type: mqtt, data_type: raw, settings: {source}}}\n"
        ))
        .unwrap();
        assert!(registry.source(&config.source).is_err(), "{source}");
    }
    let config = ChannelConfig::from_yaml(
        "id: x\nsource: {type: timer, data_type: raw, settings: {interval: 1s}}\ndestinations:\n  - {id: d, type: mqtt, settings: {broker: 'b:1883', topic: 'a/{PID-3'}}\n",
    )
    .unwrap();
    assert!(registry.destination(&config.destinations[0]).is_err());
}
