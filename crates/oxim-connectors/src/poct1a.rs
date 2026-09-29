//! The `poct1a` source: point-of-care devices speaking CLSI POCT1-A over
//! TCP.
//!
//! OXIM listens and plays the observation reviewer (data manager) role for
//! each device connection: it acknowledges the device's hello and status,
//! requests new observations when the status announces them, and stores every
//! observation (`OBS`) and event (`EVS`) message before acknowledging it
//! with `AA`. If storing fails the message is answered with `AE`, so the
//! device keeps it (ADR 0004).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, EngineError, Registry, Settings, SourceConnector, SourceContext, SubmitInfo,
    async_trait,
};
use oxim_poct1a::{
    DataAcknowledgment, DeliveryOutcome, HostConfig, HostConversation, Message, Now, Output,
    SplitEvent, Splitter, SplitterOptions,
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

fn duration(seconds: u64) -> DurationText {
    DurationText(Duration::from_secs(seconds))
}

fn hello_timeout_default() -> DurationText {
    duration(60)
}

fn response_timeout_default() -> DurationText {
    duration(30)
}

fn idle_timeout_default() -> Option<DurationText> {
    Some(duration(300))
}

fn enabled() -> bool {
    true
}

/// Settings of the `poct1a` source.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Poct1aSettings {
    /// Address to listen on, for example `0.0.0.0:5200`. Several devices may
    /// connect at once.
    pub listen: String,
    /// How long to wait for a device's hello after it connects (60 s).
    #[serde(default = "hello_timeout_default")]
    pub hello_timeout: DurationText,
    /// How long to wait for the device to acknowledge a message (30 s).
    #[serde(default = "response_timeout_default")]
    pub response_timeout: DurationText,
    /// End conversations after this long without device traffic (300 s);
    /// `null` disables it.
    #[serde(default = "idle_timeout_default")]
    pub idle_timeout: Option<DurationText>,
    /// Send keep-alive messages at this interval while idle (off by
    /// default; not every device supports them).
    #[serde(default)]
    pub keep_alive_interval: Option<DurationText>,
    /// Request observations when a status message announces new ones.
    #[serde(default = "enabled")]
    pub request_observations: bool,
    /// `HDR.version_id` of OXIM's messages.
    #[serde(default)]
    pub version_id: Option<String>,
    /// Largest XML document accepted, in bytes (4 MiB by default).
    #[serde(default)]
    pub max_document_len: Option<usize>,
}

/// Receives POCT1-A observations and events from devices.
#[derive(Debug)]
pub struct Poct1aSource {
    listen: String,
    host: HostConfig,
    splitter: SplitterOptions,
}

impl Poct1aSource {
    /// Validates the settings.
    pub fn new(settings: Poct1aSettings) -> Result<Self, EngineError> {
        let mut host = HostConfig::default();
        host.hello_timeout = settings.hello_timeout.0;
        host.response_timeout = settings.response_timeout.0;
        host.idle_timeout = settings.idle_timeout.map(|timeout| timeout.0);
        host.keep_alive_interval = settings.keep_alive_interval.map(|interval| interval.0);
        host.request_observations_on_status = settings.request_observations;
        host.data_acknowledgment = DataAcknowledgment::Manual;
        if let Some(version) = settings.version_id {
            host.version_id = version;
        }
        let durations = [
            Some(host.hello_timeout),
            Some(host.response_timeout),
            host.idle_timeout,
            host.keep_alive_interval,
        ];
        if durations.into_iter().flatten().any(|d| d.is_zero()) {
            return Err(EngineError::Config(
                "poct1a timeouts must be positive".into(),
            ));
        }
        // Validate the text settings once, at deployment.
        HostConversation::new(host.clone(), Instant::now())
            .map_err(|e| EngineError::Config(format!("poct1a settings: {e}")))?;
        let mut splitter = SplitterOptions::default();
        if let Some(max) = settings.max_document_len {
            if max == 0 {
                return Err(EngineError::Config(
                    "max_document_len must be positive".into(),
                ));
            }
            splitter.max_document_len = max;
        }
        Ok(Self {
            listen: settings.listen,
            host,
            splitter,
        })
    }
}

#[async_trait]
impl SourceConnector for Poct1aSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let listener = TcpListener::bind(self.listen.as_str())
            .await
            .map_err(|e| ConnectorError(format!("cannot listen on {}: {e}", self.listen)))?;
        info!(address = %self.listen, "poct1a listening");
        // Connections are aborted when this set is dropped.
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        connections.spawn(serve(
                            stream,
                            peer.to_string(),
                            context.clone(),
                            self.host.clone(),
                            self.splitter,
                        ));
                    }
                    Err(error) => warn!(address = %self.listen, %error, "cannot accept a connection"),
                },
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }
}

/// Runs one device conversation until it ends or the connection closes.
async fn serve(
    stream: TcpStream,
    peer: String,
    context: SourceContext,
    host: HostConfig,
    splitter_options: SplitterOptions,
) {
    let _ = stream.set_nodelay(true);
    let mut conversation = match HostConversation::new(host, Instant::now()) {
        Ok(conversation) => conversation,
        Err(error) => {
            warn!(%peer, %error, "cannot start a POCT1-A conversation");
            return;
        }
    };
    let mut splitter = Splitter::new(splitter_options);
    let (mut reader, mut writer) = stream.into_split();
    let mut buffer = vec![0u8; 8192];
    let mut metadata = BTreeMap::new();
    info!(%peer, "POCT1-A device connected");
    loop {
        while let Some(output) = conversation.poll_output() {
            match output {
                Output::Send(message) => {
                    if writer.write_all(message.as_bytes()).await.is_err() {
                        return;
                    }
                }
                Output::Hello(device) => {
                    for (key, value) in [
                        ("poct1a.device_id", &device.device_id),
                        ("poct1a.vendor_id", &device.vendor_id),
                        ("poct1a.model_id", &device.model_id),
                        ("poct1a.serial_id", &device.serial_id),
                    ] {
                        if let Some(value) = value {
                            metadata.insert(key.to_owned(), value.clone());
                        }
                    }
                    info!(%peer, device = ?device.device_id, "POCT1-A device identified");
                }
                Output::Observations(message) | Output::Events(message) => {
                    let control_id = message.control_id().unwrap_or_default().to_owned();
                    let info = SubmitInfo {
                        peer: Some(peer.clone()),
                        metadata: metadata.clone(),
                        ..SubmitInfo::default()
                    };
                    let outcome = match context.submit(message.into_bytes(), info).await {
                        Ok(_) => DeliveryOutcome::Accepted,
                        Err(error) => {
                            warn!(%peer, %error, "cannot store a POCT1-A message; answering AE");
                            DeliveryOutcome::Error("the message could not be stored".into())
                        }
                    };
                    let datetime = context.now().to_string();
                    let now = Now {
                        instant: Instant::now(),
                        datetime: &datetime,
                    };
                    if let Err(error) = conversation.acknowledge(&control_id, outcome, now) {
                        warn!(%peer, %error, "cannot acknowledge a POCT1-A message");
                    }
                }
                Output::Closed(reason) => {
                    info!(%peer, ?reason, "POCT1-A conversation ended");
                    let _ = writer.shutdown().await;
                    return;
                }
                other => debug!(%peer, ?other, "POCT1-A conversation"),
            }
        }
        let deadline = conversation.poll_timeout();
        tokio::select! {
            () = context.cancelled() => return,
            read = reader.read(&mut buffer) => {
                let n = match read {
                    Ok(0) | Err(_) => {
                        info!(%peer, "POCT1-A device disconnected");
                        return;
                    }
                    Ok(n) => n,
                };
                splitter.push(&buffer[..n]);
                while let Some(event) = splitter.next_event() {
                    match event {
                        SplitEvent::Document(bytes) => match Message::parse(&bytes) {
                            Ok(message) => {
                                let datetime = context.now().to_string();
                                let now = Now { instant: Instant::now(), datetime: &datetime };
                                if let Err(error) = conversation.handle_message(message, now) {
                                    warn!(%peer, %error, "cannot handle a POCT1-A message");
                                }
                            }
                            Err(error) => warn!(%peer, %error, "ignoring an invalid POCT1-A document"),
                        },
                        SplitEvent::Discarded { bytes, reason } => {
                            debug!(%peer, bytes, ?reason, "POCT1-A bytes discarded");
                        }
                    }
                }
            }
            () = sleep_until(deadline) => {
                let datetime = context.now().to_string();
                let now = Now { instant: Instant::now(), datetime: &datetime };
                if let Err(error) = conversation.handle_timeout(now) {
                    warn!(%peer, %error, "POCT1-A timer failed");
                }
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending().await,
    }
}

/// Parses connector settings into their typed form.
fn parse(settings: &Settings) -> Result<Poct1aSettings, EngineError> {
    serde_json::from_value(serde_json::Value::Object(settings.clone()))
        .map_err(|e| EngineError::Config(format!("poct1a settings: {e}")))
}

/// Registers the `poct1a` source.
pub fn register(registry: &mut Registry) {
    registry.add_source("poct1a", |config| {
        Ok(Arc::new(Poct1aSource::new(parse(&config.settings)?)?) as Arc<dyn SourceConnector>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_settings() {
        let settings = parse(
            serde_json::json!({"listen": "0.0.0.0:5200", "idle_timeout": null, "keep_alive_interval": "30s"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let source = Poct1aSource::new(settings).unwrap();
        assert_eq!(source.host.idle_timeout, None);
        assert_eq!(
            source.host.keep_alive_interval,
            Some(Duration::from_secs(30))
        );
        assert_eq!(source.host.data_acknowledgment, DataAcknowledgment::Manual);

        assert!(parse(serde_json::json!({}).as_object().unwrap()).is_err());
        assert!(
            parse(
                serde_json::json!({"listen": "x", "other": 1})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
        let zero = parse(
            serde_json::json!({"listen": "x", "hello_timeout": "0s"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert!(Poct1aSource::new(zero).is_err());
    }
}
