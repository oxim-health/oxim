//! One LIS01 link per endpoint, shared by the ASTM source and destination
//! connectors that name the same endpoint.
//!
//! An analyzer connected over one serial port or one TCP connection both
//! sends results and receives worklists on the same half-duplex LIS01 link,
//! so the connectors cannot each open their own. The first connector that
//! needs an endpoint starts a link task; later connectors with the same
//! endpoint key (`tcp-listen:<address>`, `tcp-connect:<address>` or
//! `serial:<port>`) share it. The task stops when the last connector using
//! it is dropped, for example when the channels are undeployed.
//!
//! The link task keeps the transport open (accepting, reconnecting or
//! reopening with backoff) and runs an [`oxim_astm::session::Session`] with
//! deferred final acknowledgment: a received message is handed to the
//! attached source, and the frame that completes it is acknowledged only
//! after the source stored it durably (ADR 0004).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};

use oxim_astm::session::{self, Output, Session, SessionConfig};
use oxim_core::EngineError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::serial::PortSettings;

/// Where the bytes of a link come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Transport {
    /// Accept connections on this address; a new connection replaces the
    /// current one.
    TcpListen(String),
    /// Connect to this address.
    TcpConnect(String),
    /// Open this serial port.
    Serial(PortSettings),
}

impl Transport {
    /// The key under which connectors share the link.
    pub(crate) fn key(&self) -> String {
        match self {
            Self::TcpListen(address) => format!("tcp-listen:{address}"),
            Self::TcpConnect(address) => format!("tcp-connect:{address}"),
            Self::Serial(port) => format!("serial:{}", port.port),
        }
    }
}

/// Settings of the link itself, which every connector sharing it must agree
/// on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LinkOptions {
    pub(crate) session: SessionConfig,
    pub(crate) reconnect_delay: Duration,
    pub(crate) max_reconnect_delay: Duration,
}

/// A message received from the device, for the attached source.
#[derive(Debug)]
pub(crate) struct Inbound {
    pub(crate) text: Vec<u8>,
    pub(crate) peer: String,
    /// Answer `true` once the message is stored durably, `false` otherwise.
    /// `None` when the link already acknowledged the message.
    pub(crate) confirm: Option<oneshot::Sender<bool>>,
}

/// Why a message could not be sent to the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SendFailure {
    /// The message cannot be sent over LIS01; retrying cannot help.
    Invalid(String),
    /// The link failed; the message may be sent again later.
    Failed(String),
}

struct Outbound {
    text: Vec<u8>,
    reply: oneshot::Sender<Result<(), SendFailure>>,
}

type Receiver = Arc<Mutex<Option<mpsc::Sender<Inbound>>>>;

/// A running link. Dropping the last handle stops it.
pub(crate) struct Link {
    key: String,
    transport: Transport,
    options: LinkOptions,
    receiver: Receiver,
    commands: mpsc::Sender<Outbound>,
    task: tokio::task::JoinHandle<()>,
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.task.abort();
    }
}

static LINKS: LazyLock<Mutex<HashMap<String, Weak<Link>>>> = LazyLock::new(Mutex::default);

/// Returns the running link for `transport`, starting it if needed.
pub(crate) fn acquire(
    transport: Transport,
    options: LinkOptions,
) -> Result<Arc<Link>, EngineError> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| EngineError::Config("ASTM connectors need a Tokio runtime".into()))?;
    let key = transport.key();
    let mut links = LINKS
        .lock()
        .map_err(|_| EngineError::Config("the ASTM link registry is unavailable".into()))?;
    links.retain(|_, link| link.strong_count() > 0);
    if let Some(link) = links.get(&key).and_then(Weak::upgrade) {
        if link.transport != transport || link.options != options {
            return Err(EngineError::Config(format!(
                "ASTM link {key} is already in use with different settings; connectors \
                 sharing a link must use the same link settings"
            )));
        }
        return Ok(link);
    }
    let receiver: Receiver = Arc::new(Mutex::new(None));
    let (commands, queue) = mpsc::channel(64);
    let task = runtime.spawn(run_link(
        transport.clone(),
        options,
        receiver.clone(),
        queue,
    ));
    let link = Arc::new(Link {
        key: key.clone(),
        transport,
        options,
        receiver,
        commands,
        task,
    });
    links.insert(key, Arc::downgrade(&link));
    Ok(link)
}

/// Keeps a source attached to a link; detaches it when dropped.
pub(crate) struct Attachment {
    receiver: Receiver,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.receiver.lock() {
            *slot = None;
        }
    }
}

impl Link {
    /// The endpoint key.
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// Routes received messages to `sender` until the attachment is
    /// dropped. Only one source can be attached at a time.
    pub(crate) fn attach(&self, sender: mpsc::Sender<Inbound>) -> Result<Attachment, String> {
        let mut slot = self
            .receiver
            .lock()
            .map_err(|_| format!("ASTM link {} is unavailable", self.key))?;
        if slot.as_ref().is_some_and(|current| !current.is_closed()) {
            return Err(format!(
                "ASTM link {} already delivers to another source",
                self.key
            ));
        }
        *slot = Some(sender);
        Ok(Attachment {
            receiver: self.receiver.clone(),
        })
    }

    /// Sends a message to the device and waits until every frame is
    /// acknowledged, the link gives up, or `timeout` passes.
    pub(crate) async fn send(&self, text: Vec<u8>, timeout: Duration) -> Result<(), SendFailure> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Outbound { text, reply })
            .await
            .map_err(|_| SendFailure::Failed(format!("ASTM link {} stopped", self.key)))?;
        match tokio::time::timeout(timeout, response).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(SendFailure::Failed(format!(
                "ASTM link {} stopped",
                self.key
            ))),
            Err(_) => Err(SendFailure::Failed(format!(
                "not delivered within {} s over ASTM link {}",
                timeout.as_secs(),
                self.key
            ))),
        }
    }
}

/// A byte stream the link can run over.
pub(crate) trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

type Stream = Box<dyn Duplex>;

async fn open(
    transport: &Transport,
    listener: &mut Option<TcpListener>,
) -> std::io::Result<(Stream, String)> {
    match transport {
        Transport::TcpListen(address) => {
            let bound = match listener {
                Some(bound) => bound,
                None => listener.insert(TcpListener::bind(address.as_str()).await?),
            };
            let (stream, peer) = bound.accept().await?;
            let _ = stream.set_nodelay(true);
            Ok((Box::new(stream), peer.to_string()))
        }
        Transport::TcpConnect(address) => {
            let stream = TcpStream::connect(address.as_str()).await?;
            let _ = stream.set_nodelay(true);
            Ok((Box::new(stream), address.clone()))
        }
        Transport::Serial(port) => Ok((Box::new(port.open()?), port.port.clone())),
    }
}

/// Waits `duration` while queueing send requests. Returns `false` when the
/// link was dropped.
async fn pause(
    duration: Duration,
    commands: &mut mpsc::Receiver<Outbound>,
    waiting: &mut VecDeque<Outbound>,
) -> bool {
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            () = &mut sleep => return true,
            command = commands.recv() => match command {
                Some(request) => {
                    waiting.retain(|queued| !queued.reply.is_closed());
                    waiting.push_back(request);
                }
                None => return false,
            },
        }
    }
}

async fn run_link(
    transport: Transport,
    options: LinkOptions,
    receiver: Receiver,
    mut commands: mpsc::Receiver<Outbound>,
) {
    let key = transport.key();
    let mut waiting = VecDeque::new();
    let mut listener: Option<TcpListener> = None;
    let mut delay = options.reconnect_delay;
    let mut next: Option<(Stream, String)> = None;
    loop {
        let opened = match next.take() {
            Some(replacement) => Ok(replacement),
            None => tokio::select! {
                opened = open(&transport, &mut listener) => opened,
                command = commands.recv() => match command {
                    Some(request) => {
                        waiting.push_back(request);
                        continue;
                    }
                    None => return,
                },
            },
        };
        match opened {
            Ok((stream, peer)) => {
                delay = options.reconnect_delay;
                info!(link = %key, %peer, "ASTM link connected");
                let end = run_session(
                    stream,
                    &peer,
                    &options,
                    &receiver,
                    &mut commands,
                    &mut waiting,
                    listener.as_ref(),
                )
                .await;
                info!(link = %key, %peer, "ASTM link disconnected");
                match end {
                    SessionEnd::LinkDropped => return,
                    SessionEnd::Replaced(stream, peer) => {
                        next = Some((stream, peer));
                        continue;
                    }
                    SessionEnd::Closed if listener.is_some() => continue,
                    SessionEnd::Closed => {}
                }
            }
            Err(error) => {
                warn!(link = %key, %error, retry_in_ms = delay.as_millis(), "cannot open ASTM link");
            }
        }
        if !pause(delay, &mut commands, &mut waiting).await {
            return;
        }
        delay = delay.saturating_mul(2).min(options.max_reconnect_delay);
    }
}

/// Why [`run_session`] returned.
enum SessionEnd {
    /// The stream closed or failed.
    Closed,
    /// Every handle to the link was dropped.
    LinkDropped,
    /// A new connection arrived on the listener and replaces this one.
    Replaced(Stream, String),
}

/// Hands a received message to the attached source.
async fn deliver(
    receiver: &Receiver,
    text: Vec<u8>,
    peer: &str,
    confirm: bool,
) -> Option<oneshot::Receiver<bool>> {
    let sender = receiver.lock().ok().and_then(|slot| slot.clone());
    let Some(sender) = sender else {
        warn!(%peer, "no ASTM source is attached; the message is refused so the device keeps it");
        return None;
    };
    let (tx, rx) = if confirm {
        let (tx, rx) = oneshot::channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let inbound = Inbound {
        text,
        peer: peer.to_owned(),
        confirm: tx,
    };
    if sender.send(inbound).await.is_err() {
        warn!(%peer, "the ASTM source stopped; the message is refused so the device keeps it");
        return None;
    }
    rx
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending().await,
    }
}

async fn accept(
    listener: Option<&TcpListener>,
) -> std::io::Result<(TcpStream, std::net::SocketAddr)> {
    match listener {
        Some(listener) => listener.accept().await,
        None => std::future::pending().await,
    }
}

/// Runs one LIS01 session over `stream` until it closes.
async fn run_session<S>(
    stream: S,
    peer: &str,
    options: &LinkOptions,
    receiver: &Receiver,
    commands: &mut mpsc::Receiver<Outbound>,
    waiting: &mut VecDeque<Outbound>,
    listener: Option<&TcpListener>,
) -> SessionEnd
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut session = Session::new(options.session);
    let mut in_flight: HashMap<session::MessageId, oneshot::Sender<Result<(), SendFailure>>> =
        HashMap::new();
    let mut confirmation: Option<oneshot::Receiver<bool>> = None;
    let mut buffer = vec![0u8; 4096];
    let end = loop {
        let now = Instant::now();
        while let Some(request) = waiting.pop_front() {
            if request.reply.is_closed() {
                continue;
            }
            match session.send(request.text, now) {
                Ok(id) => {
                    in_flight.insert(id, request.reply);
                }
                Err(error) => {
                    let _ = request
                        .reply
                        .send(Err(SendFailure::Invalid(error.to_string())));
                }
            }
        }

        let mut failed = false;
        while let Some(output) = session.poll_output() {
            match output {
                Output::Transmit(bytes) => {
                    if writer.write_all(&bytes).await.is_err() {
                        failed = true;
                        break;
                    }
                }
                Output::ReceivedPending(text) => match deliver(receiver, text, peer, true).await {
                    Some(stored) => confirmation = Some(stored),
                    None => {
                        session.reject_received(Instant::now());
                    }
                },
                Output::Received(text) => {
                    // Already acknowledged by the link (for example a message
                    // without a terminator record): store it if possible.
                    let _ = deliver(receiver, text, peer, false).await;
                }
                Output::Delivered(id) => {
                    if let Some(reply) = in_flight.remove(&id)
                        && reply.send(Ok(())).is_err()
                    {
                        warn!(%peer, "a message was delivered after its sender gave up; it may be delivered twice");
                    }
                }
                Output::Aborted { id, reason } => {
                    if let Some(reply) = in_flight.remove(&id) {
                        let _ = reply.send(Err(SendFailure::Failed(format!(
                            "the device did not accept the message ({reason:?})"
                        ))));
                    }
                }
                Output::Event(event) => debug!(%peer, ?event, "ASTM link event"),
                other => debug!(%peer, ?other, "ASTM link output"),
            }
        }
        if failed || writer.flush().await.is_err() {
            break SessionEnd::Closed;
        }

        let deadline = session.poll_timeout();
        tokio::select! {
            read = reader.read(&mut buffer) => match read {
                Ok(0) | Err(_) => break SessionEnd::Closed,
                Ok(n) => session.handle_input(&buffer[..n], Instant::now()),
            },
            command = commands.recv() => match command {
                Some(request) => waiting.push_back(request),
                None => break SessionEnd::LinkDropped,
            },
            stored = async {
                match confirmation.as_mut() {
                    Some(stored) => stored.await,
                    None => std::future::pending().await,
                }
            }, if confirmation.is_some() => {
                confirmation = None;
                let now = Instant::now();
                if stored == Ok(true) {
                    session.confirm_received(now);
                } else {
                    session.reject_received(now);
                }
            }
            () = sleep_until(deadline) => session.handle_timeout(Instant::now()),
            accepted = accept(listener) => match accepted {
                Ok((stream, address)) => {
                    let _ = stream.set_nodelay(true);
                    info!(%peer, replacement = %address, "a new connection replaces the current ASTM link");
                    break SessionEnd::Replaced(Box::new(stream), address.to_string());
                }
                Err(error) => debug!(%error, "cannot accept a replacement ASTM connection"),
            },
        }
    };
    for (_, reply) in in_flight {
        let _ = reply.send(Err(SendFailure::Failed(
            "the link closed before the message was delivered".into(),
        )));
    }
    end
}

#[cfg(test)]
mod tests {
    use oxim_astm::frame::{ACK, EOT};
    use oxim_astm::session::Role;

    use super::*;

    const MESSAGE: &[u8] =
        b"H|\\^&|||ANALYZER\rP|1\rO|1|SMP001||^^^GLU\rR|1|^^^GLU|5.4|mmol/L\rL|1|N\r";

    fn options() -> LinkOptions {
        let mut session = SessionConfig::default();
        session.defer_final_ack = true;
        LinkOptions {
            session,
            reconnect_delay: Duration::from_millis(10),
            max_reconnect_delay: Duration::from_millis(100),
        }
    }

    /// Drives an instrument-side session over `stream` until `done`
    /// returns true for its outputs, returning everything the host sent.
    async fn instrument<S: AsyncRead + AsyncWrite + Unpin>(
        stream: S,
        send: Option<&[u8]>,
        mut done: impl FnMut(&[Output]) -> bool,
    ) -> (Vec<Output>, Vec<u8>) {
        let mut config = SessionConfig::default();
        config.role = Role::Instrument;
        let mut session = Session::new(config);
        if let Some(message) = send {
            session.send(message.to_vec(), Instant::now()).unwrap();
        }
        let (mut reader, mut writer) = tokio::io::split(stream);
        let mut log = Vec::new();
        let mut from_host = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            while let Some(output) = session.poll_output() {
                if let Output::Transmit(bytes) = &output {
                    writer.write_all(bytes).await.unwrap();
                }
                log.push(output);
            }
            if done(&log) {
                return (log, from_host);
            }
            let n = tokio::time::timeout(Duration::from_secs(5), reader.read(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            from_host.extend_from_slice(&buffer[..n]);
            session.handle_input(&buffer[..n], Instant::now());
        }
    }

    #[tokio::test]
    async fn stores_before_the_final_acknowledgment() {
        let (host_side, device_side) = tokio::io::duplex(4096);
        let (sender, mut inbox) = mpsc::channel(4);
        let receiver: Receiver = Arc::new(Mutex::new(Some(sender)));
        let (_commands, mut command_queue) = mpsc::channel(4);
        let options = options();
        let link = tokio::spawn({
            let receiver = receiver.clone();
            async move {
                let mut waiting = VecDeque::new();
                run_session(
                    host_side,
                    "duplex",
                    &options,
                    &receiver,
                    &mut command_queue,
                    &mut waiting,
                    None,
                )
                .await;
            }
        });
        let device = tokio::spawn(instrument(device_side, Some(MESSAGE), |log| {
            log.iter().any(|o| matches!(o, Output::Delivered(_)))
        }));

        let inbound = tokio::time::timeout(Duration::from_secs(5), inbox.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(inbound.text, MESSAGE);
        assert_eq!(inbound.peer, "duplex");
        // The device is still waiting for the final ACK while we "store".
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!device.is_finished());
        inbound.confirm.unwrap().send(true).unwrap();

        let (log, from_host) = device.await.unwrap();
        assert!(log.iter().any(|o| matches!(o, Output::Delivered(_))));
        assert_eq!(from_host.iter().filter(|&&b| b == ACK).count(), 6);
        link.abort();
    }

    #[tokio::test]
    async fn refused_messages_are_retransmitted() {
        let (host_side, device_side) = tokio::io::duplex(4096);
        let (sender, mut inbox) = mpsc::channel(4);
        let receiver: Receiver = Arc::new(Mutex::new(Some(sender)));
        let (_commands, mut command_queue) = mpsc::channel(4);
        let options = options();
        let link = tokio::spawn({
            let receiver = receiver.clone();
            async move {
                let mut waiting = VecDeque::new();
                run_session(
                    host_side,
                    "duplex",
                    &options,
                    &receiver,
                    &mut command_queue,
                    &mut waiting,
                    None,
                )
                .await;
            }
        });
        let device = tokio::spawn(instrument(device_side, Some(MESSAGE), |log| {
            log.iter().any(|o| matches!(o, Output::Delivered(_)))
        }));

        let first = inbox.recv().await.unwrap();
        first.confirm.unwrap().send(false).unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), inbox.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.text, MESSAGE);
        second.confirm.unwrap().send(true).unwrap();
        let (log, _) = device.await.unwrap();
        assert!(
            log.iter()
                .any(|o| matches!(o, Output::Event(session::Event::Retransmission { .. })))
        );
        link.abort();
    }

    #[tokio::test]
    async fn sends_queued_messages_to_the_device() {
        let (host_side, device_side) = tokio::io::duplex(4096);
        let receiver: Receiver = Arc::new(Mutex::new(None));
        let (commands, mut command_queue) = mpsc::channel(4);
        let options = options();
        let link = tokio::spawn({
            let receiver = receiver.clone();
            async move {
                let mut waiting = VecDeque::new();
                run_session(
                    host_side,
                    "duplex",
                    &options,
                    &receiver,
                    &mut command_queue,
                    &mut waiting,
                    None,
                )
                .await;
            }
        });
        let device = tokio::spawn(instrument(device_side, None, |log| {
            log.iter().any(|o| matches!(o, Output::Received(_)))
        }));

        // Text LIS01 cannot carry is refused without touching the link.
        let (reply, response) = oneshot::channel();
        commands
            .send(Outbound {
                text: b"H|\\^&\r\nL|1\r".to_vec(),
                reply,
            })
            .await
            .unwrap();
        assert!(matches!(
            response.await.unwrap(),
            Err(SendFailure::Invalid(_))
        ));

        let (reply, response) = oneshot::channel();
        let worklist = b"H|\\^&\rP|1\rO|1|SMP002||^^^HGB\rL|1|N\r".to_vec();
        commands
            .send(Outbound {
                text: worklist.clone(),
                reply,
            })
            .await
            .unwrap();
        assert_eq!(response.await.unwrap(), Ok(()));
        let (log, from_host) = device.await.unwrap();
        assert!(log.contains(&Output::Received(worklist)));
        assert_eq!(from_host.last(), Some(&EOT));
        link.abort();
    }

    #[tokio::test]
    async fn refuses_messages_without_a_source() {
        let (host_side, device_side) = tokio::io::duplex(4096);
        let receiver: Receiver = Arc::new(Mutex::new(None));
        let (_commands, mut command_queue) = mpsc::channel(4);
        let mut options = options();
        options.session.receive_timeout = Duration::from_millis(200);
        let link = tokio::spawn({
            let receiver = receiver.clone();
            async move {
                let mut waiting = VecDeque::new();
                run_session(
                    host_side,
                    "duplex",
                    &options,
                    &receiver,
                    &mut command_queue,
                    &mut waiting,
                    None,
                )
                .await;
            }
        });
        // The instrument retransmits the refused frame; stop at the first
        // retransmission.
        let (log, _) = instrument(device_side, Some(MESSAGE), |log| {
            log.iter()
                .any(|o| matches!(o, Output::Event(session::Event::Retransmission { .. })))
        })
        .await;
        assert!(!log.iter().any(|o| matches!(o, Output::Delivered(_))));
        link.abort();
    }
}
