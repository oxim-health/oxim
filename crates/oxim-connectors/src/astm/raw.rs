//! The `astm-raw-tcp` source: ASTM E1394 records over TCP without LIS01
//! framing.
//!
//! Many analyzers connected over a network send CR-separated records
//! without the ENQ/STX/ACK handshake. Messages are cut from the header (`H`)
//! to the terminator (`L`) record. When `acknowledge` is set, the source
//! answers each message with ACK (0x06) after it is stored durably, or NAK
//! (0x15) if storing failed, which some analyzers expect.

use std::time::Duration;

use oxim_astm::frame::{ACK, NAK};
use oxim_astm::raw::{RawEvent, RawOptions, RawSplitter};
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, EngineError, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use super::{Mode, delays, max_reconnect_delay_default, reconnect_delay_default};

/// Settings of the `astm-raw-tcp` source.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AstmRawTcpSettings {
    /// `server` (default) or `client`.
    #[serde(default)]
    pub mode: Mode,
    /// Address to listen on in server mode. Several analyzers may connect.
    #[serde(default)]
    pub listen: Option<String>,
    /// Address to connect to in client mode.
    #[serde(default)]
    pub connect: Option<String>,
    /// First delay before reconnecting in client mode.
    #[serde(default = "reconnect_delay_default")]
    pub reconnect_delay: DurationText,
    /// Upper bound of the reconnect delay.
    #[serde(default = "max_reconnect_delay_default")]
    pub max_reconnect_delay: DurationText,
    /// Answer each message with ACK after storing it, or NAK if storing
    /// failed.
    #[serde(default)]
    pub acknowledge: bool,
    /// Largest message accepted, in bytes (16 MiB by default).
    #[serde(default)]
    pub max_message_len: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Endpoint {
    Listen(String),
    Connect(String),
}

/// Receives unframed ASTM messages over TCP.
#[derive(Debug)]
pub struct AstmRawTcpSource {
    endpoint: Endpoint,
    acknowledge: bool,
    options: RawOptions,
    reconnect_delay: Duration,
    max_reconnect_delay: Duration,
}

impl AstmRawTcpSource {
    /// Validates the settings.
    pub fn new(settings: AstmRawTcpSettings) -> Result<Self, EngineError> {
        let endpoint = match (settings.mode, settings.listen, settings.connect) {
            (Mode::Server, Some(listen), None) => Endpoint::Listen(listen),
            (Mode::Client, None, Some(connect)) => Endpoint::Connect(connect),
            (Mode::Server, _, _) => {
                return Err(EngineError::Config(
                    "astm-raw-tcp in server mode needs `listen` and no `connect`".into(),
                ));
            }
            (Mode::Client, _, _) => {
                return Err(EngineError::Config(
                    "astm-raw-tcp in client mode needs `connect` and no `listen`".into(),
                ));
            }
        };
        let (reconnect_delay, max_reconnect_delay) =
            delays(settings.reconnect_delay, settings.max_reconnect_delay)?;
        let mut options = RawOptions::default();
        if let Some(max) = settings.max_message_len {
            if max == 0 {
                return Err(EngineError::Config(
                    "max_message_len must be positive".into(),
                ));
            }
            options.max_message_len = max;
        }
        Ok(Self {
            endpoint,
            acknowledge: settings.acknowledge,
            options,
            reconnect_delay,
            max_reconnect_delay,
        })
    }
}

#[async_trait]
impl SourceConnector for AstmRawTcpSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        match &self.endpoint {
            Endpoint::Listen(address) => {
                let listener = TcpListener::bind(address.as_str())
                    .await
                    .map_err(|e| ConnectorError(format!("cannot listen on {address}: {e}")))?;
                info!(%address, "astm-raw-tcp listening");
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
                                    self.acknowledge,
                                    self.options,
                                ));
                            }
                            Err(error) => warn!(%address, %error, "cannot accept a connection"),
                        },
                        Some(_) = connections.join_next(), if !connections.is_empty() => {}
                    }
                }
            }
            Endpoint::Connect(address) => {
                let mut delay = self.reconnect_delay;
                loop {
                    let connected = tokio::select! {
                        () = context.cancelled() => return Ok(()),
                        connected = TcpStream::connect(address.as_str()) => connected,
                    };
                    match connected {
                        Ok(stream) => {
                            delay = self.reconnect_delay;
                            info!(%address, "astm-raw-tcp connected");
                            tokio::select! {
                                () = context.cancelled() => return Ok(()),
                                () = serve(stream, address.clone(), context.clone(), self.acknowledge, self.options) => {}
                            }
                            info!(%address, "astm-raw-tcp disconnected");
                        }
                        Err(error) => warn!(%address, %error, "cannot connect"),
                    }
                    tokio::select! {
                        () = context.cancelled() => return Ok(()),
                        () = tokio::time::sleep(delay) => {}
                    }
                    delay = delay.saturating_mul(2).min(self.max_reconnect_delay);
                }
            }
        }
    }
}

async fn serve(
    stream: TcpStream,
    peer: String,
    context: SourceContext,
    acknowledge: bool,
    options: RawOptions,
) {
    let _ = stream.set_nodelay(true);
    let mut splitter = RawSplitter::new(options);
    let (mut reader, mut writer) = stream.into_split();
    let mut buffer = vec![0u8; 8192];
    loop {
        let n = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        splitter.push(&buffer[..n]);
        while let Some(event) = splitter.next_event() {
            match event {
                RawEvent::Message(message) => {
                    let info = SubmitInfo {
                        peer: Some(peer.clone()),
                        ..SubmitInfo::default()
                    };
                    let stored = context.submit(message, info).await;
                    if let Err(error) = &stored {
                        warn!(%peer, %error, "cannot store an ASTM message");
                    }
                    if acknowledge {
                        let reply = if stored.is_ok() { ACK } else { NAK };
                        if writer.write_all(&[reply]).await.is_err() {
                            return;
                        }
                    }
                }
                RawEvent::Discarded { bytes, reason } => {
                    debug!(%peer, bytes, ?reason, "astm-raw-tcp discarded bytes");
                }
            }
        }
    }
    if let Some(RawEvent::Discarded { bytes, reason }) = splitter.finish() {
        warn!(%peer, bytes, ?reason, "the connection closed inside an ASTM message");
    }
}
