//! Raw TCP with configurable framing, for devices and systems that do not
//! use MLLP.
//!
//! Both the `tcp` source and the `tcp` destination take a `framing` setting:
//!
//! | `framing.mode` | Further settings | A message is |
//! |---|---|---|
//! | `delimited` | `end`, optional `start` | the bytes between `start` (if set) and `end` |
//! | `length_prefix` | `length_bytes`: 2 or 4 (default 4) | a big-endian length followed by that many bytes |
//! | `none` | | everything sent on one connection, until it is closed |
//!
//! Byte sequences are written as text with the escapes `\r`, `\n`, `\t`,
//! `\\` and `\xHH`, or as `hex:` followed by hexadecimal digits
//! (`hex:02`, `"\x1c\r"`).
//!
//! Source type `tcp`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `listen` | required | Address to listen on |
//! | `framing` | required | See above |
//! | `response` | none | Bytes sent after each message is stored, for example `hex:06` |
//! | `error_response` | none | Bytes sent when a message could not be stored, for example `hex:15` |
//! | `max_connections` | `100` | Concurrent connections |
//! | `max_message_len` | 16 MiB | Largest accepted message |
//! | `tls` | none | TLS listener settings (certificate, key, optional client CA for mutual TLS); see [`tls`](crate::tls) |
//!
//! Responses are written as-is, without framing. With framing `none`, the
//! response is sent and the connection closed after the message is stored.
//!
//! Destination type `tcp`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `target` | required | Receiver address |
//! | `framing` | required | See above |
//! | `connect_timeout` | `10s` | Time to establish a connection |
//! | `response_timeout` | `30s` | Time to wait for a response |
//! | `wait_for_response` | `false` | Read one framed response after each message |
//! | `expected_response` | none | When set, a response that differs fails the attempt (retried) |
//! | `max_message_len` | 16 MiB | Largest accepted response |
//! | `tls` | none | TLS sender settings (`tls: {}` for defaults; CA, client certificate for mutual TLS); see [`tls`](crate::tls) |
//!
//! Connections are reused between messages, except with framing `none`,
//! where each message uses its own connection and the response (if waited
//! for) is everything the receiver sends before closing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use memchr::memmem;
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::net::{DEFAULT_MAX_MESSAGE, ListenerTls, parse_bytes, serve, settings};
use crate::tls::{ClientTlsSettings, Dialer, ServerTlsSettings, Stream};

const READ_BUFFER: usize = 64 * 1024;

fn default_max_connections() -> usize {
    100
}

fn default_max_message_len() -> usize {
    DEFAULT_MAX_MESSAGE
}

fn default_length_bytes() -> u8 {
    4
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_response_timeout() -> DurationText {
    DurationText(Duration::from_secs(30))
}

/// How messages are delimited on a TCP stream.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum FramingSettings {
    /// Messages are enclosed by an optional start and a required end
    /// sequence.
    Delimited {
        /// Bytes before each message.
        #[serde(default)]
        start: Option<String>,
        /// Bytes after each message.
        end: String,
    },
    /// Each message is preceded by its length.
    LengthPrefix {
        /// Size of the length field: 2 or 4 bytes, big endian.
        #[serde(default = "default_length_bytes")]
        length_bytes: u8,
    },
    /// One message per connection.
    None,
}

/// Compiled framing rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Framing {
    /// Messages between `start` (optional) and `end`.
    Delimited {
        /// Start sequence.
        start: Option<Vec<u8>>,
        /// End sequence.
        end: Vec<u8>,
    },
    /// Big-endian length prefix of 2 or 4 bytes.
    LengthPrefix(u8),
    /// One message per connection.
    Whole,
}

impl Framing {
    /// Validates and compiles framing settings.
    pub fn from_settings(settings: &FramingSettings) -> Result<Self, EngineError> {
        let bytes = |text: &str| parse_bytes(text).map_err(EngineError::Config);
        Ok(match settings {
            FramingSettings::Delimited { start, end } => Self::Delimited {
                start: start.as_deref().map(bytes).transpose()?,
                end: bytes(end)?,
            },
            FramingSettings::LengthPrefix { length_bytes } => match length_bytes {
                2 | 4 => Self::LengthPrefix(*length_bytes),
                other => {
                    return Err(EngineError::Config(format!(
                        "length_bytes must be 2 or 4, not {other}"
                    )));
                }
            },
            FramingSettings::None => Self::Whole,
        })
    }

    /// Frames one message for sending.
    pub fn encode(&self, payload: &[u8]) -> Result<Vec<u8>, String> {
        Ok(match self {
            Self::Delimited { start, end } => {
                if memmem::find(payload, end).is_some() {
                    return Err("the message contains the end sequence".into());
                }
                let mut out = Vec::with_capacity(payload.len() + end.len() + 4);
                if let Some(start) = start {
                    out.extend_from_slice(start);
                }
                out.extend_from_slice(payload);
                out.extend_from_slice(end);
                out
            }
            Self::LengthPrefix(2) => {
                let len = u16::try_from(payload.len())
                    .map_err(|_| "the message is too long for a 2-byte length prefix".to_owned())?;
                [len.to_be_bytes().as_slice(), payload].concat()
            }
            Self::LengthPrefix(_) => {
                let len = u32::try_from(payload.len())
                    .map_err(|_| "the message is too long for a 4-byte length prefix".to_owned())?;
                [len.to_be_bytes().as_slice(), payload].concat()
            }
            Self::Whole => payload.to_vec(),
        })
    }
}

/// Something the [`Framer`] found in a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameEvent {
    /// A complete message.
    Frame(Vec<u8>),
    /// Bytes that were dropped: outside a frame, or part of an oversized
    /// message.
    Discarded(usize),
    /// The stream cannot continue, for example because a declared length is
    /// too large; the connection should be closed.
    Fatal(String),
}

/// Splits a byte stream into messages according to a [`Framing`].
#[derive(Debug, Clone)]
pub struct Framer {
    framing: Framing,
    max_len: usize,
    buffer: Vec<u8>,
    inside: bool,
    skipping: bool,
}

impl Framer {
    /// Creates a framer.
    pub fn new(framing: Framing, max_len: usize) -> Self {
        Self {
            framing,
            max_len,
            buffer: Vec::new(),
            inside: false,
            skipping: false,
        }
    }

    /// Appends received bytes.
    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// The next event, or `None` when more bytes are needed.
    pub fn next_event(&mut self) -> Option<FrameEvent> {
        match self.framing.clone() {
            Framing::Delimited { start, end } => self.next_delimited(start.as_deref(), &end),
            Framing::LengthPrefix(size) => self.next_prefixed(usize::from(size)),
            Framing::Whole => {
                if self.buffer.len() > self.max_len {
                    let len = self.buffer.len();
                    self.buffer.clear();
                    return Some(FrameEvent::Fatal(format!(
                        "the message exceeds {} bytes ({len} buffered)",
                        self.max_len
                    )));
                }
                None
            }
        }
    }

    /// Signals the end of the stream and returns what is left.
    pub fn finish(&mut self) -> Option<FrameEvent> {
        let rest = std::mem::take(&mut self.buffer);
        self.inside = false;
        self.skipping = false;
        if rest.is_empty() {
            return None;
        }
        Some(match self.framing {
            Framing::Whole => FrameEvent::Frame(rest),
            _ => FrameEvent::Discarded(rest.len()),
        })
    }

    fn discard(&mut self, len: usize) -> FrameEvent {
        self.buffer.drain(..len);
        FrameEvent::Discarded(len)
    }

    fn next_delimited(&mut self, start: Option<&[u8]>, end: &[u8]) -> Option<FrameEvent> {
        if self.skipping {
            // Drop the rest of an oversized message, up to its end.
            return match memmem::find(&self.buffer, end) {
                Some(at) => {
                    self.skipping = false;
                    Some(self.discard(at + end.len()))
                }
                None if self.buffer.len() > end.len() => {
                    let keep = end.len() - 1;
                    Some(self.discard(self.buffer.len() - keep))
                }
                None => None,
            };
        }
        if let Some(start) = start
            && !self.inside
        {
            match memmem::find(&self.buffer, start) {
                Some(0) => {
                    self.buffer.drain(..start.len());
                    self.inside = true;
                }
                Some(at) => return Some(self.discard(at)),
                None if self.buffer.len() >= start.len() => {
                    let keep = start.len() - 1;
                    return Some(self.discard(self.buffer.len() - keep));
                }
                None => return None,
            }
        }
        match memmem::find(&self.buffer, end) {
            Some(at) => {
                let rest = self.buffer.split_off(at + end.len());
                let mut payload = std::mem::replace(&mut self.buffer, rest);
                payload.truncate(at);
                self.inside = false;
                if payload.len() > self.max_len {
                    return Some(FrameEvent::Discarded(at + end.len()));
                }
                Some(FrameEvent::Frame(payload))
            }
            None if self.buffer.len() > self.max_len + end.len() => {
                self.inside = false;
                self.skipping = true;
                let keep = end.len() - 1;
                Some(self.discard(self.buffer.len() - keep))
            }
            None => None,
        }
    }

    fn next_prefixed(&mut self, size: usize) -> Option<FrameEvent> {
        let header = self.buffer.get(..size)?;
        let len = header.iter().fold(0usize, |acc, &b| {
            acc.saturating_mul(256).saturating_add(usize::from(b))
        });
        if len > self.max_len {
            self.buffer.clear();
            return Some(FrameEvent::Fatal(format!(
                "declared message length {len} exceeds {} bytes",
                self.max_len
            )));
        }
        if self.buffer.len() < size + len {
            return None;
        }
        let rest = self.buffer.split_off(size + len);
        let mut payload = std::mem::replace(&mut self.buffer, rest);
        payload.drain(..size);
        Some(FrameEvent::Frame(payload))
    }
}

/// Settings of the `tcp` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcpSourceSettings {
    /// Address to listen on.
    pub listen: String,
    /// How messages are delimited.
    pub framing: FramingSettings,
    /// Bytes sent after each stored message.
    #[serde(default)]
    pub response: Option<String>,
    /// Bytes sent when a message could not be stored.
    #[serde(default)]
    pub error_response: Option<String>,
    /// Concurrent connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Largest accepted message.
    #[serde(default = "default_max_message_len")]
    pub max_message_len: usize,
    /// TLS, optionally with client certificates (mutual TLS).
    #[serde(default)]
    pub tls: Option<ServerTlsSettings>,
}

#[derive(Debug)]
struct SourceSetup {
    framing: Framing,
    response: Option<Vec<u8>>,
    error_response: Option<Vec<u8>>,
    max_message_len: usize,
}

/// Receives framed messages over raw TCP.
#[derive(Debug, Clone)]
pub struct TcpSource {
    listen: String,
    max_connections: usize,
    tls: Option<ListenerTls>,
    setup: Arc<SourceSetup>,
}

impl TcpSource {
    /// Validates the settings and creates the source.
    pub fn new(settings: TcpSourceSettings) -> Result<Self, EngineError> {
        let bytes = |text: &Option<String>| {
            text.as_deref()
                .map(|text| parse_bytes(text).map_err(EngineError::Config))
                .transpose()
        };
        Ok(Self {
            listen: settings.listen.clone(),
            max_connections: settings.max_connections,
            tls: ListenerTls::from_settings(settings.tls.as_ref())?,
            setup: Arc::new(SourceSetup {
                framing: Framing::from_settings(&settings.framing)?,
                response: bytes(&settings.response)?,
                error_response: bytes(&settings.error_response)?,
                max_message_len: settings.max_message_len,
            }),
        })
    }
}

#[async_trait]
impl SourceConnector for TcpSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let setup = self.setup.clone();
        let handler_context = context.clone();
        serve(
            &context,
            &self.listen,
            self.max_connections,
            self.tls.clone(),
            move |stream, peer| {
                source_connection(handler_context.clone(), setup.clone(), stream, peer)
            },
        )
        .await
    }
}

async fn source_connection(
    context: SourceContext,
    setup: Arc<SourceSetup>,
    mut stream: Stream,
    peer: SocketAddr,
) {
    let mut framer = Framer::new(setup.framing.clone(), setup.max_message_len);
    let mut buffer = vec![0; READ_BUFFER];
    loop {
        let read = tokio::select! {
            () = context.cancelled() => return,
            read = stream.read(&mut buffer) => read,
        };
        let closed = match read {
            Ok(0) => true,
            Ok(n) => {
                framer.push(&buffer[..n]);
                false
            }
            Err(e) => {
                debug!(channel = %context.channel(), %peer, error = %e, "connection closed");
                return;
            }
        };
        loop {
            let event = if closed {
                framer.finish()
            } else {
                framer.next_event()
            };
            let Some(event) = event else { break };
            match event {
                FrameEvent::Frame(payload) => {
                    let info = SubmitInfo {
                        peer: Some(peer.to_string()),
                        ..SubmitInfo::default()
                    };
                    let response = match context.submit(payload, info).await {
                        Ok(_) => setup.response.as_deref(),
                        Err(e) => {
                            warn!(channel = %context.channel(), %peer, error = %e, "message could not be stored");
                            setup.error_response.as_deref()
                        }
                    };
                    if let Some(response) = response
                        && stream.write_all(response).await.is_err()
                    {
                        return;
                    }
                }
                FrameEvent::Discarded(bytes) => {
                    debug!(channel = %context.channel(), %peer, bytes, "discarded bytes");
                }
                FrameEvent::Fatal(reason) => {
                    warn!(channel = %context.channel(), %peer, %reason, "closing connection");
                    return;
                }
            }
            if closed {
                break;
            }
        }
        if closed {
            let _ = stream.shutdown().await;
            return;
        }
    }
}

/// Settings of the `tcp` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcpDestinationSettings {
    /// Receiver address.
    pub target: String,
    /// How messages are delimited.
    pub framing: FramingSettings,
    /// Time to establish a connection.
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: DurationText,
    /// Time to wait for a response.
    #[serde(default = "default_response_timeout")]
    pub response_timeout: DurationText,
    /// Whether to read a response after each message.
    #[serde(default)]
    pub wait_for_response: bool,
    /// The response that confirms delivery.
    #[serde(default)]
    pub expected_response: Option<String>,
    /// Largest accepted response.
    #[serde(default = "default_max_message_len")]
    pub max_message_len: usize,
    /// TLS, optionally with a client certificate (mutual TLS).
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
}

/// Sends framed messages over raw TCP.
#[derive(Debug)]
pub struct TcpDestination {
    settings: TcpDestinationSettings,
    framing: Framing,
    expected: Option<Vec<u8>>,
    dialer: Dialer,
    connection: Mutex<Option<(Stream, Framer)>>,
}

impl TcpDestination {
    /// Validates the settings and creates the destination.
    pub fn new(settings: TcpDestinationSettings) -> Result<Self, EngineError> {
        let framing = Framing::from_settings(&settings.framing)?;
        let expected = settings
            .expected_response
            .as_deref()
            .map(|text| parse_bytes(text).map_err(EngineError::Config))
            .transpose()?;
        let dialer = Dialer::new(
            &settings.target,
            settings.connect_timeout.0,
            settings.tls.as_ref(),
        )?;
        Ok(Self {
            settings,
            framing,
            expected,
            dialer,
            connection: Mutex::new(None),
        })
    }

    async fn connect(&self) -> Result<Stream, SendError> {
        self.dialer.connect().await.map_err(SendError::temporary)
    }

    fn check_response(&self, response: Vec<u8>) -> Result<Option<Vec<u8>>, SendError> {
        match &self.expected {
            Some(expected) if *expected != response => Err(SendError::temporary(format!(
                "unexpected response {:?}",
                String::from_utf8_lossy(&response)
            ))),
            _ => Ok(Some(response)),
        }
    }

    /// One message on its own connection; the response is everything the
    /// receiver sends before closing.
    async fn send_whole(&self, frame: &[u8]) -> Result<Option<Vec<u8>>, SendError> {
        let mut stream = self.connect().await?;
        let timeout = self.settings.response_timeout.0;
        tokio::time::timeout(timeout, async {
            stream.write_all(frame).await?;
            stream.shutdown().await
        })
        .await
        .map_err(|_| SendError::temporary("sending timed out"))?
        .map_err(|e| SendError::temporary(format!("sending failed: {e}")))?;
        if !self.settings.wait_for_response {
            return Ok(None);
        }
        let mut response = Vec::new();
        let limit = u64::try_from(self.settings.max_message_len).unwrap_or(u64::MAX);
        tokio::time::timeout(
            timeout,
            (&mut stream).take(limit).read_to_end(&mut response),
        )
        .await
        .map_err(|_| SendError::temporary("no response before the timeout"))?
        .map_err(|e| SendError::temporary(format!("reading the response failed: {e}")))?;
        self.check_response(response)
    }

    async fn exchange(
        &self,
        slot: &mut Option<(Stream, Framer)>,
        frame: &[u8],
    ) -> Result<Option<Vec<u8>>, SendError> {
        if slot.is_none() {
            let stream = self.connect().await?;
            *slot = Some((
                stream,
                Framer::new(self.framing.clone(), self.settings.max_message_len),
            ));
        }
        let Some((stream, framer)) = slot.as_mut() else {
            return Err(SendError::temporary("no connection"));
        };
        let timeout = self.settings.response_timeout.0;
        tokio::time::timeout(timeout, stream.write_all(frame))
            .await
            .map_err(|_| SendError::temporary("sending timed out"))?
            .map_err(|e| SendError::temporary(format!("sending failed: {e}")))?;
        if !self.settings.wait_for_response {
            return Ok(None);
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let mut buffer = vec![0; READ_BUFFER];
        loop {
            while let Some(event) = framer.next_event() {
                match event {
                    FrameEvent::Frame(response) => return self.check_response(response),
                    FrameEvent::Discarded(_) => {}
                    FrameEvent::Fatal(reason) => return Err(SendError::temporary(reason)),
                }
            }
            match tokio::time::timeout_at(deadline, stream.read(&mut buffer)).await {
                Err(_) => return Err(SendError::temporary("no response before the timeout")),
                Ok(Ok(0)) => {
                    return Err(SendError::temporary(
                        "the receiver closed the connection before responding",
                    ));
                }
                Ok(Ok(n)) => framer.push(&buffer[..n]),
                Ok(Err(e)) => {
                    return Err(SendError::temporary(format!(
                        "reading the response failed: {e}"
                    )));
                }
            }
        }
    }
}

#[async_trait]
impl DestinationConnector for TcpDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let frame = self
            .framing
            .encode(&delivery.payload)
            .map_err(SendError::permanent)?;
        if self.framing == Framing::Whole {
            return self.send_whole(&frame).await;
        }
        let mut slot = self.connection.lock().await;
        let result = self.exchange(&mut slot, &frame).await;
        if result.is_err() {
            // The connection state is unknown after a failure.
            *slot = None;
        }
        result
    }
}

/// Registers the `tcp` source and destination.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("tcp", |config: &SourceConfig| {
            let settings: TcpSourceSettings = settings(&config.settings, "tcp source")?;
            Ok(Arc::new(TcpSource::new(settings)?) as Arc<dyn SourceConnector>)
        })
        .add_destination("tcp", |config: &DestinationConfig| {
            let settings: TcpDestinationSettings = settings(&config.settings, "tcp destination")?;
            Ok(Arc::new(TcpDestination::new(settings)?) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(framer: &mut Framer, input: &[u8]) -> Vec<FrameEvent> {
        framer.push(input);
        std::iter::from_fn(|| framer.next_event()).collect()
    }

    #[test]
    fn splits_delimited_messages() {
        let framing = Framing::Delimited {
            start: Some(vec![0x02]),
            end: vec![0x03],
        };
        let mut framer = Framer::new(framing.clone(), 100);
        assert_eq!(
            frames(&mut framer, b"xx\x02one\x03\x02tw"),
            [FrameEvent::Discarded(2), FrameEvent::Frame(b"one".to_vec())]
        );
        assert_eq!(
            frames(&mut framer, b"o\x03"),
            [FrameEvent::Frame(b"two".to_vec())]
        );
        assert_eq!(framing.encode(b"abc").unwrap(), b"\x02abc\x03");
        assert!(framing.encode(b"a\x03b").is_err());

        let mut end_only = Framer::new(
            Framing::Delimited {
                start: None,
                end: b"\r\n".to_vec(),
            },
            100,
        );
        assert_eq!(
            frames(&mut end_only, b"a\r\nb\r"),
            [FrameEvent::Frame(b"a".to_vec())]
        );
        assert_eq!(
            frames(&mut end_only, b"\n"),
            [FrameEvent::Frame(b"b".to_vec())]
        );
    }

    #[test]
    fn drops_oversized_delimited_messages() {
        let mut framer = Framer::new(
            Framing::Delimited {
                start: None,
                end: vec![b'|'],
            },
            4,
        );
        let events = frames(&mut framer, b"0123456789");
        assert!(matches!(events[..], [FrameEvent::Discarded(_)]));
        assert_eq!(
            frames(&mut framer, b"abc|ok|"),
            [FrameEvent::Discarded(4), FrameEvent::Frame(b"ok".to_vec())]
        );
    }

    #[test]
    fn splits_length_prefixed_messages() {
        let framing = Framing::LengthPrefix(2);
        let encoded = [
            framing.encode(b"hello").unwrap(),
            framing.encode(b"x").unwrap(),
        ]
        .concat();
        let mut framer = Framer::new(framing, 100);
        assert_eq!(frames(&mut framer, &encoded[..4]), []);
        assert_eq!(
            frames(&mut framer, &encoded[4..]),
            [
                FrameEvent::Frame(b"hello".to_vec()),
                FrameEvent::Frame(b"x".to_vec())
            ]
        );
        let mut limited = Framer::new(Framing::LengthPrefix(4), 10);
        assert!(matches!(
            frames(&mut limited, &[0, 0, 1, 0]).as_slice(),
            [FrameEvent::Fatal(_)]
        ));
    }

    #[test]
    fn whole_connection_is_one_message() {
        let mut framer = Framer::new(Framing::Whole, 10);
        assert_eq!(frames(&mut framer, b"abc"), []);
        assert_eq!(framer.finish(), Some(FrameEvent::Frame(b"abc".to_vec())));
        let mut limited = Framer::new(Framing::Whole, 2);
        assert!(matches!(
            frames(&mut limited, b"abc").as_slice(),
            [FrameEvent::Fatal(_)]
        ));
    }

    #[test]
    fn validates_framing_settings() {
        assert!(
            Framing::from_settings(&FramingSettings::LengthPrefix { length_bytes: 3 }).is_err()
        );
        assert!(
            Framing::from_settings(&FramingSettings::Delimited {
                start: None,
                end: String::new()
            })
            .is_err()
        );
    }
}
