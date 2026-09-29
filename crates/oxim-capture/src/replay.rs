//! Replaying the device side of a capture against a host.
//!
//! Every connection of the capture is replayed in turn. The device's bytes
//! are sent as recorded; where the capture shows the host answering, the
//! replay waits for the host to answer before it continues, so protocols
//! with strict turn-taking (ASTM LIS01, POCT1-A) keep their rhythm. The
//! host's answers are collected, not compared: timestamps and control
//! identifiers legitimately differ from the recording.
//!
//! An answer is complete when the protocol says so (a LIS01 control
//! character or frame end, an MLLP end block, a closed POCT1-A document),
//! when as many bytes arrived as were recorded (other protocols), or when
//! the host stays silent for [`ReplayOptions::idle`] after it started
//! answering.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use crate::format::{Capture, Direction, Event, Protocol, Record};

/// Where the replayed device meets the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The device connects to the host at this address (the usual case).
    Connect(String),
    /// The device listens on this address and the host connects to it.
    Listen(String),
}

/// Replay timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayOptions {
    /// How long to wait for the host to start an answer.
    pub answer_timeout: Duration,
    /// How long the host may pause within an answer before it is taken as
    /// complete.
    pub idle: Duration,
    /// Keep the recorded pauses between the device's transmissions (at
    /// most 10 seconds each) instead of sending as fast as the host answers.
    pub realtime: bool,
    /// How long to keep retrying to connect (the host may still be starting)
    /// or to wait for the host to connect in listen mode.
    pub connect_timeout: Duration,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            answer_timeout: Duration::from_secs(10),
            idle: Duration::from_millis(300),
            realtime: false,
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// What a replay did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayReport {
    /// Connections replayed.
    pub connections: usize,
    /// Device bytes sent.
    pub bytes_sent: usize,
    /// Host answers received where the capture shows one.
    pub answers: usize,
    /// Answers the capture shows but the host did not give in time.
    pub unanswered: usize,
    /// Everything that was exchanged, in order.
    pub transcript: Vec<(Direction, Vec<u8>)>,
}

impl ReplayReport {
    /// All bytes the host sent.
    pub fn host_bytes(&self) -> Vec<u8> {
        self.transcript
            .iter()
            .filter(|(direction, _)| *direction == Direction::HostToDevice)
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect()
    }
}

const ENQ: u8 = 0x05;
const ACK: u8 = 0x06;
const EOT: u8 = 0x04;
const NAK: u8 = 0x15;
const LF: u8 = 0x0A;

/// Whether `received` holds a complete answer in `protocol`, compared with
/// the recorded `expected` answer.
fn complete(protocol: Option<Protocol>, expected: &[u8], received: &[u8]) -> Option<bool> {
    let last = *received.last()?;
    match protocol? {
        Protocol::AstmLis01 => Some(matches!(last, ACK | NAK | EOT | ENQ | LF)),
        Protocol::Hl7v2Mllp => Some(received.ends_with(&[0x1C, 0x0D])),
        Protocol::Poct1a => {
            let documents = |bytes: &[u8]| {
                bytes
                    .windows(5)
                    .filter(|window| *window == b"<?xml")
                    .count()
            };
            let trimmed = received.trim_ascii_end();
            Some(trimmed.ends_with(b">") && documents(received) >= documents(expected).max(1))
        }
        Protocol::AstmRaw | Protocol::Raw => None,
    }
}

async fn open(
    endpoint: &Endpoint,
    listener: Option<&TcpListener>,
    options: &ReplayOptions,
) -> io::Result<TcpStream> {
    match endpoint {
        Endpoint::Connect(address) => {
            let deadline = Instant::now() + options.connect_timeout;
            loop {
                match TcpStream::connect(address).await {
                    Ok(stream) => return Ok(stream),
                    Err(e) if Instant::now() >= deadline => return Err(e),
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }
        Endpoint::Listen(address) => {
            let listener = listener
                .ok_or_else(|| io::Error::other(format!("no listener bound on {address}")))?;
            match tokio::time::timeout(options.connect_timeout, listener.accept()).await {
                Ok(accepted) => Ok(accepted?.0),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("the host did not connect to {address}"),
                )),
            }
        }
    }
}

/// Reads the host's answer. Returns the bytes read.
async fn read_answer(
    stream: &mut TcpStream,
    protocol: Option<Protocol>,
    expected: &[u8],
    options: &ReplayOptions,
) -> io::Result<Vec<u8>> {
    let mut received = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let first_deadline = Instant::now() + options.answer_timeout;
    loop {
        let wait = if received.is_empty() {
            first_deadline.saturating_duration_since(Instant::now())
        } else {
            options.idle
        };
        match tokio::time::timeout(wait, stream.read(&mut buffer)).await {
            Ok(Ok(0)) | Err(_) => return Ok(received),
            Ok(Ok(n)) => {
                received.extend_from_slice(&buffer[..n]);
                let done = complete(protocol, expected, &received)
                    .unwrap_or(received.len() >= expected.len());
                if done {
                    return Ok(received);
                }
            }
            Ok(Err(e)) => return Err(e),
        }
    }
}

/// Replays one connection's records.
async fn replay_connection(
    stream: &mut TcpStream,
    protocol: Option<Protocol>,
    records: &[&Record],
    options: &ReplayOptions,
    report: &mut ReplayReport,
) -> io::Result<()> {
    let mut expected: Vec<u8> = Vec::new();
    let mut last_sent: Option<oxim_model::Timestamp> = None;
    for record in records {
        match record.direction {
            Some(Direction::HostToDevice) => expected.extend_from_slice(&record.data),
            Some(Direction::DeviceToHost) => {
                if !expected.is_empty() {
                    answer(stream, protocol, &expected, options, report).await?;
                    expected.clear();
                }
                if options.realtime
                    && let Some(previous) = last_sent
                {
                    let gap = record
                        .timestamp
                        .unix_nanos()
                        .saturating_sub(previous.unix_nanos());
                    let gap = Duration::from_nanos(u64::try_from(gap).unwrap_or(0));
                    tokio::time::sleep(gap.min(Duration::from_secs(10))).await;
                }
                stream.write_all(&record.data).await?;
                stream.flush().await?;
                report.bytes_sent += record.data.len();
                report
                    .transcript
                    .push((Direction::DeviceToHost, record.data.clone()));
                last_sent = Some(record.timestamp);
            }
            None => {}
        }
    }
    if !expected.is_empty() {
        answer(stream, protocol, &expected, options, report).await?;
    }
    // Collect anything the host still sends, such as a late reply.
    let trailing = read_answer(
        stream,
        None,
        &[],
        &ReplayOptions {
            answer_timeout: options.idle,
            ..*options
        },
    )
    .await
    .unwrap_or_default();
    if !trailing.is_empty() {
        report.transcript.push((Direction::HostToDevice, trailing));
    }
    let _ = stream.shutdown().await;
    Ok(())
}

async fn answer(
    stream: &mut TcpStream,
    protocol: Option<Protocol>,
    expected: &[u8],
    options: &ReplayOptions,
    report: &mut ReplayReport,
) -> io::Result<()> {
    let received = read_answer(stream, protocol, expected, options).await?;
    if received.is_empty() {
        report.unanswered += 1;
    } else {
        report.answers += 1;
        report.transcript.push((Direction::HostToDevice, received));
    }
    Ok(())
}

/// Replays the device side of `capture` against a host.
pub async fn replay(
    capture: &Capture,
    endpoint: &Endpoint,
    options: &ReplayOptions,
) -> io::Result<ReplayReport> {
    let listener = match endpoint {
        Endpoint::Listen(address) => Some(TcpListener::bind(address).await?),
        Endpoint::Connect(_) => None,
    };
    replay_inner(capture, endpoint, listener, options).await
}

/// Like [`replay`] in listen mode, with a listener the caller already
/// bound (for example to port 0, to learn the port first).
pub async fn replay_listening(
    capture: &Capture,
    listener: TcpListener,
    options: &ReplayOptions,
) -> io::Result<ReplayReport> {
    let address: SocketAddr = listener.local_addr()?;
    replay_inner(
        capture,
        &Endpoint::Listen(address.to_string()),
        Some(listener),
        options,
    )
    .await
}

async fn replay_inner(
    capture: &Capture,
    endpoint: &Endpoint,
    listener: Option<TcpListener>,
    options: &ReplayOptions,
) -> io::Result<ReplayReport> {
    let mut report = ReplayReport::default();
    for connection in capture.connections() {
        let records: Vec<&Record> = capture
            .records
            .iter()
            .filter(|r| r.connection == connection && r.event == Event::Data)
            .collect();
        if records.is_empty() {
            continue;
        }
        let mut stream = open(endpoint, listener.as_ref(), options).await?;
        report.connections += 1;
        replay_connection(
            &mut stream,
            capture.header.protocol,
            &records,
            options,
            &mut report,
        )
        .await?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_complete_answers() {
        let astm = Some(Protocol::AstmLis01);
        assert_eq!(complete(astm, b"\x06", b"\x06"), Some(true));
        assert_eq!(complete(astm, b"\x02", b"\x021H|"), Some(false));
        assert_eq!(complete(astm, b"", b"\x021H|\x03AB\r\n"), Some(true));
        let mllp = Some(Protocol::Hl7v2Mllp);
        assert_eq!(complete(mllp, b"", b"\x0bMSH|\x1c"), Some(false));
        assert_eq!(complete(mllp, b"", b"\x0bMSH|\x1c\r"), Some(true));
        let poct = Some(Protocol::Poct1a);
        assert_eq!(complete(poct, b"<?xml?><A/>", b"<?xml?><ACK"), Some(false));
        assert_eq!(
            complete(poct, b"<?xml?><A/>", b"<?xml?><ACK/>\n"),
            Some(true)
        );
        assert_eq!(complete(None, b"ab", b"a"), None);
        assert_eq!(complete(astm, b"", b""), None);
    }
}
