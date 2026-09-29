//! Sending notifications to their targets.

use std::sync::Arc;
use std::time::Duration;

use oxim_core::{DestinationConfig, DestinationConnector, EngineError, Registry};
use oxim_model::{ChannelId, ConnectorId, DataType, MessageId, Timestamp};
use oxim_store::Delivery;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpStream, UdpSocket};
use tracing::{debug, info, warn};

use crate::config::{Format, Protocol, Severity, TargetConfig, TargetKind};
use crate::evaluate::{AlertState, Notification};
use crate::snmp;

/// How a target sends.
enum Sender {
    Log,
    Destination {
        connector: Arc<dyn DestinationConnector>,
        body: Body,
    },
    Syslog {
        address: String,
        protocol: Protocol,
        facility: u8,
        app_name: String,
    },
    Snmp {
        address: String,
        community: String,
        trap_oid: Vec<u32>,
    },
}

#[derive(Debug, Clone, Copy)]
enum Body {
    Text,
    Json,
    Teams,
    Slack,
}

/// A configured target.
pub(crate) struct Target {
    id: String,
    min_severity: Option<Severity>,
    sender: Sender,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

fn facility(name: &str) -> Result<u8, EngineError> {
    Ok(match name {
        "kern" => 0,
        "user" => 1,
        "daemon" => 3,
        "auth" => 4,
        "syslog" => 5,
        "local0" => 16,
        "local1" => 17,
        "local2" => 18,
        "local3" => 19,
        "local4" => 20,
        "local5" => 21,
        "local6" => 22,
        "local7" => 23,
        other => {
            return Err(EngineError::Config(format!(
                "alerts: unknown syslog facility {other:?}"
            )));
        }
    })
}

fn destination(
    registry: &Registry,
    id: &str,
    connector: &str,
    settings: serde_json::Value,
) -> Result<Arc<dyn DestinationConnector>, EngineError> {
    let config: DestinationConfig = serde_json::from_value(serde_json::json!({
        "id": format!("alert-{id}"),
        "type": connector,
        "settings": settings,
    }))
    .map_err(|e| EngineError::Config(format!("alerts: target {id:?}: {e}")))?;
    registry
        .destination(&config)
        .map_err(|e| EngineError::Config(format!("alerts: target {id:?}: {e}")))
}

impl Target {
    /// Builds a target; destination-based targets use `registry`.
    pub(crate) fn new(config: &TargetConfig, registry: &Registry) -> Result<Self, EngineError> {
        let id = config.id.as_str();
        let webhook =
            |url: &str, headers: serde_json::Value, ca_file: Option<&std::path::PathBuf>| {
                let mut settings = serde_json::json!({
                    "url": url,
                    "content_type": "application/json",
                    "headers": headers,
                    "timeout": "15s",
                });
                if let Some(ca) = ca_file {
                    settings["ca_file"] = serde_json::json!(ca);
                }
                destination(registry, id, "http", settings)
            };
        let sender = match &config.kind {
            TargetKind::Log => Sender::Log,
            TargetKind::Webhook {
                url,
                headers,
                ca_file,
            } => Sender::Destination {
                connector: webhook(url, serde_json::json!(headers), ca_file.as_ref())?,
                body: Body::Json,
            },
            TargetKind::Teams { url } => Sender::Destination {
                connector: webhook(url, serde_json::json!({}), None)?,
                body: Body::Teams,
            },
            TargetKind::Slack { url } => Sender::Destination {
                connector: webhook(url, serde_json::json!({}), None)?,
                body: Body::Slack,
            },
            TargetKind::Email { settings } => Sender::Destination {
                connector: destination(
                    registry,
                    id,
                    "smtp",
                    serde_json::Value::Object(settings.clone()),
                )?,
                body: Body::Text,
            },
            TargetKind::Destination {
                connector,
                settings,
                format,
            } => Sender::Destination {
                connector: destination(
                    registry,
                    id,
                    connector,
                    serde_json::Value::Object(settings.clone()),
                )?,
                body: match format {
                    Format::Text => Body::Text,
                    Format::Json => Body::Json,
                },
            },
            TargetKind::Syslog {
                address,
                protocol,
                facility: name,
                app_name,
            } => Sender::Syslog {
                address: address.clone(),
                protocol: *protocol,
                facility: facility(name)?,
                app_name: app_name.clone(),
            },
            TargetKind::Snmp {
                address,
                community_env,
                trap_oid,
            } => Sender::Snmp {
                address: address.clone(),
                community: community_env
                    .as_ref()
                    .and_then(|name| std::env::var(name).ok())
                    .unwrap_or_else(|| "public".to_owned()),
                trap_oid: snmp::parse_oid(trap_oid)
                    .map_err(|e| EngineError::Config(format!("alerts: target {id:?}: {e}")))?,
            },
        };
        Ok(Self {
            id: config.id.clone(),
            min_severity: config.min_severity,
            sender,
        })
    }

    /// The target identifier.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Whether the target takes `notification`.
    pub(crate) fn accepts(&self, notification: &Notification) -> bool {
        (notification.targets.is_empty() || notification.targets.contains(&self.id))
            && self
                .min_severity
                .is_none_or(|minimum| notification.severity >= minimum)
    }

    /// Sends one notification once.
    pub(crate) async fn send(
        &self,
        notification: &Notification,
        sequence: u64,
    ) -> Result<(), String> {
        match &self.sender {
            Sender::Log => {
                match notification.state {
                    AlertState::Firing => warn!(
                        rule = %notification.rule,
                        subject = %notification.subject,
                        severity = notification.severity.as_str(),
                        "alert: {}", notification.summary
                    ),
                    AlertState::Resolved => info!(
                        rule = %notification.rule,
                        subject = %notification.subject,
                        "alert: {}", notification.summary
                    ),
                }
                Ok(())
            }
            Sender::Destination { connector, body } => {
                let (payload, data_type) = render(notification, *body);
                let delivery = Delivery {
                    message_id: MessageId::from_parts(
                        u64::try_from(notification.at.unix_millis()).unwrap_or(0),
                        u128::from(sequence),
                    ),
                    channel: ChannelId::new("alerts").map_err(|e| e.to_string())?,
                    destination: ConnectorId::new(format!("alert-{}", self.id))
                        .map_err(|e| e.to_string())?,
                    attempts: 0,
                    payload,
                    data_type: Some(data_type),
                };
                connector
                    .send(&delivery)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
            Sender::Syslog {
                address,
                protocol,
                facility,
                app_name,
            } => {
                let line = syslog_line(notification, *facility, app_name);
                match protocol {
                    Protocol::Udp => {
                        let socket = UdpSocket::bind(if address.starts_with('[') {
                            "[::]:0"
                        } else {
                            "0.0.0.0:0"
                        })
                        .await
                        .map_err(|e| e.to_string())?;
                        socket
                            .send_to(line.as_bytes(), address)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    }
                    Protocol::Tcp => {
                        let mut stream = tokio::time::timeout(
                            Duration::from_secs(10),
                            TcpStream::connect(address),
                        )
                        .await
                        .map_err(|_| format!("connecting to {address} timed out"))?
                        .map_err(|e| e.to_string())?;
                        let frame = format!("{} {line}", line.len());
                        stream
                            .write_all(frame.as_bytes())
                            .await
                            .map_err(|e| e.to_string())?;
                        stream.shutdown().await.map_err(|e| e.to_string())
                    }
                }
            }
            Sender::Snmp {
                address,
                community,
                trap_oid,
            } => {
                let packet = snmp::trap(community, trap_oid, notification, sequence);
                let socket = UdpSocket::bind(if address.starts_with('[') {
                    "[::]:0"
                } else {
                    "0.0.0.0:0"
                })
                .await
                .map_err(|e| e.to_string())?;
                socket
                    .send_to(&packet, address)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        }
    }
}

fn timestamp_text(at: Timestamp) -> String {
    oxim_model::ClinicalDateTime::from_timestamp(at, 0)
        .map(|time| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                time.year(),
                time.month().unwrap_or(1),
                time.day().unwrap_or(1),
                time.hour().unwrap_or(0),
                time.minute().unwrap_or(0),
                time.second().unwrap_or(0)
            )
        })
        .unwrap_or_else(|| "-".to_owned())
}

/// The payload of a notification for a destination-based target.
fn render(notification: &Notification, body: Body) -> (Vec<u8>, DataType) {
    let json = |value: serde_json::Value| (value.to_string().into_bytes(), DataType::Json);
    match body {
        Body::Text => (
            format!(
                "{}\nsince {} (at {})\n",
                notification.text(),
                timestamp_text(notification.since),
                timestamp_text(notification.at)
            )
            .into_bytes(),
            DataType::Raw,
        ),
        Body::Json => json(serde_json::json!({
            "rule": notification.rule,
            "kind": notification.kind,
            "subject": notification.subject,
            "severity": notification.severity,
            "state": notification.state,
            "summary": notification.summary,
            "since": timestamp_text(notification.since),
            "at": timestamp_text(notification.at),
            "repeat": notification.repeat,
        })),
        Body::Slack => json(serde_json::json!({ "text": notification.text() })),
        Body::Teams => json(serde_json::json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {
                    "type": "AdaptiveCard",
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "version": "1.4",
                    "body": [
                        {
                            "type": "TextBlock",
                            "size": "Medium",
                            "weight": "Bolder",
                            "wrap": true,
                            "text": format!("OXIM {} {}: {}", notification.state.as_str(), notification.severity.as_str(), notification.rule),
                        },
                        {"type": "TextBlock", "wrap": true, "text": notification.summary},
                        {
                            "type": "FactSet",
                            "facts": [
                                {"title": "Subject", "value": notification.subject},
                                {"title": "Since", "value": timestamp_text(notification.since)},
                            ]
                        }
                    ]
                }
            }]
        })),
    }
}

/// A syslog (RFC 5424) line.
pub(crate) fn syslog_line(notification: &Notification, facility: u8, app_name: &str) -> String {
    let severity: u8 = match (notification.state, notification.severity) {
        (AlertState::Resolved, _) => 5,
        (_, Severity::Critical) => 2,
        (_, Severity::Warning) => 4,
        (_, Severity::Info) => 6,
    };
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|name| !name.is_empty() && name.is_ascii() && !name.contains(' '))
        .unwrap_or_else(|| "-".to_owned());
    format!(
        "<{}>1 {} {host} {app_name} - {} - {}",
        u16::from(facility) * 8 + u16::from(severity),
        timestamp_text(notification.at),
        notification.kind,
        notification.text()
    )
}

/// Sends `notification` to every target that takes it, retrying failed
/// sends a few times in the background.
pub(crate) fn dispatch(targets: &[Arc<Target>], notification: &Notification, sequence: u64) {
    for target in targets.iter().filter(|t| t.accepts(notification)) {
        let target = target.clone();
        let notification = notification.clone();
        tokio::spawn(async move {
            let delays = [
                Duration::from_secs(2),
                Duration::from_secs(15),
                Duration::from_secs(60),
            ];
            for (attempt, delay) in std::iter::once(None)
                .chain(delays.into_iter().map(Some))
                .enumerate()
            {
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                match target.send(&notification, sequence).await {
                    Ok(()) => {
                        debug!(target = target.id(), rule = %notification.rule, "alert notification sent");
                        return;
                    }
                    Err(error) => warn!(
                        target = target.id(),
                        rule = %notification.rule,
                        attempt = attempt + 1,
                        %error,
                        "alert notification failed"
                    ),
                }
            }
        });
    }
}
