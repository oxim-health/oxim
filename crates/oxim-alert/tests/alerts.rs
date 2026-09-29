//! Alerts from a running engine to syslog, SNMP and webhook receivers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use oxim_alert::{AlertEngine, AlertSettings, AlertState, Sources};
use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, SendError, SystemClock,
    async_trait,
};
use oxim_store::{Delivery, SqliteStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

/// A destination that is always down.
#[derive(Debug)]
struct Down;

#[async_trait]
impl DestinationConnector for Down {
    async fn send(&self, _delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        Err(SendError::temporary("connection refused"))
    }
}

async fn engine() -> Engine {
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    registry.add_destination("down", |_| {
        Ok(Arc::new(Down) as Arc<dyn DestinationConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    options.shutdown_grace = Duration::from_secs(1);
    Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap()
}

/// Answers one HTTP request with 200 and returns its body.
async fn http_once(listener: TcpListener) -> String {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let n = stream.read(&mut buffer).await.unwrap();
        request.extend_from_slice(&buffer[..n]);
        let text = String::from_utf8_lossy(&request).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
                return body.to_owned();
            }
        }
        if n == 0 {
            panic!("connection closed");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backed_up_queue_notifies_every_target() {
    let engine = engine().await;
    engine
        .deploy(
            ChannelConfig::from_yaml(
                "id: lab-results
source: {type: timer, data_type: raw, settings: {interval: 10ms, payload: result, immediately: true}}
destinations:
  - id: lis
    type: down
    queue: {retry: {initial_delay: 10s, max_delay: 10s}}
",
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let syslog = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let snmp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let web = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let settings: AlertSettings = serde_json::from_value(serde_json::json!({
        "targets": [
            {"id": "noc", "type": "syslog", "address": syslog.local_addr().unwrap().to_string()},
            {"id": "nms", "type": "snmp", "address": snmp.local_addr().unwrap().to_string()},
            {"id": "ops", "type": "webhook", "url": format!("http://{}/alerts", web.local_addr().unwrap())},
            {"id": "quiet", "type": "log", "min_severity": "critical"}
        ],
        "rules": [
            {"id": "lis-backlog", "kind": "queue_depth", "destination": "lis", "above": 3, "severity": "warning"},
            {"id": "certs", "kind": "certificate_expiry", "files": ["missing-cert.pem"], "within": "30d", "targets": ["quiet"]}
        ]
    }))
    .unwrap();
    let alerts = AlertEngine::new(settings, engine.registry()).unwrap();
    let sources = Sources {
        engine: engine.clone(),
        devices: None,
        data_dir: std::env::temp_dir(),
        certificates: Vec::new(),
    };
    let webhook = tokio::spawn(http_once(web));

    let mut fired = Vec::new();
    for _ in 0..200 {
        let snapshot = alerts.collect(&sources).await;
        fired.extend(alerts.process(&snapshot));
        if fired.iter().any(|n| n.rule == "lis-backlog") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let backlog = fired
        .iter()
        .find(|n| n.rule == "lis-backlog")
        .expect("the backlog alert fires");
    assert_eq!(backlog.state, AlertState::Firing);
    assert_eq!(backlog.subject, "lab-results/lis");
    // The unreadable certificate is reported too, to its own target.
    assert!(fired.iter().any(|n| n.rule == "certs"));

    let mut buffer = [0u8; 2048];
    let n = tokio::time::timeout(Duration::from_secs(10), syslog.recv(&mut buffer))
        .await
        .expect("syslog message")
        .unwrap();
    let line = String::from_utf8_lossy(&buffer[..n]).into_owned();
    // local0 (16) * 8 + warning (4) = 132.
    assert!(line.starts_with("<132>1 "), "{line}");
    assert!(
        line.contains("FIRING warning lis-backlog [lab-results/lis]"),
        "{line}"
    );

    let n = tokio::time::timeout(Duration::from_secs(10), snmp.recv(&mut buffer))
        .await
        .expect("SNMP trap")
        .unwrap();
    let trap = &buffer[..n];
    assert_eq!(trap[0], 0x30);
    // Version 2c, community "public", SNMPv2-Trap-PDU.
    assert!(trap.windows(3).any(|w| w == [0x02, 0x01, 0x01]));
    assert!(trap.windows(6).any(|w| w == b"public"));
    assert!(trap.contains(&0xa7));
    assert!(trap.windows(11).any(|w| w == b"lis-backlog"));

    let body = tokio::time::timeout(Duration::from_secs(10), webhook)
        .await
        .expect("webhook request")
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["rule"], "lis-backlog");
    assert_eq!(json["state"], "firing");
    assert_eq!(json["subject"], "lab-results/lis");

    assert!(!alerts.active().is_empty());
    engine.shutdown().await;
}

#[test]
fn unknown_connectors_and_targets_are_configuration_errors() {
    let registry = Registry::new();
    let settings: AlertSettings = serde_json::from_value(serde_json::json!({
        "targets": [{"id": "mail", "type": "email", "settings": {"to": "ops@example.org"}}]
    }))
    .unwrap();
    let error = AlertEngine::new(settings, &registry).unwrap_err();
    assert!(error.to_string().contains("mail"), "{error}");
    let settings: AlertSettings = serde_json::from_value(serde_json::json!({
        "targets": [{"id": "noc", "type": "syslog", "address": "127.0.0.1:514", "facility": "local9"}]
    }))
    .unwrap();
    assert!(AlertEngine::new(settings, &registry).is_err());
}
