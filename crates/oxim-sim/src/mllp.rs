//! An HL7 sender and an HL7 receiver (LIS) over MLLP.

use std::io;
use std::time::{Duration, Instant};

use oxim_hl7::{AckCode, AckOptions, Message, build_ack};
use oxim_mllp::{Decoder, Event, encode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What happened to messages sent with [`send`].
#[derive(Debug, Clone, Default)]
pub struct SendReport {
    /// Messages written.
    pub sent: u64,
    /// Answered with AA or CA.
    pub accepted: u64,
    /// Answered with AE or CE.
    pub errors: u64,
    /// Answered with AR or CR.
    pub rejected: u64,
    /// Not answered in time, or answered with something unreadable.
    pub unanswered: u64,
    /// Round-trip times of answered messages.
    pub latencies: Vec<Duration>,
}

impl SendReport {
    /// A one-line summary with latency percentiles.
    pub fn summary(&self) -> String {
        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();
        let percentile = |p: f64| -> String {
            if sorted.is_empty() {
                return "-".into();
            }
            let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
            format!(
                "{:.1}ms",
                sorted[index.min(sorted.len() - 1)].as_secs_f64() * 1000.0
            )
        };
        format!(
            "sent={} accepted={} errors={} rejected={} unanswered={} latency p50={} p95={} p99={} max={}",
            self.sent,
            self.accepted,
            self.errors,
            self.rejected,
            self.unanswered,
            percentile(0.5),
            percentile(0.95),
            percentile(0.99),
            percentile(1.0)
        )
    }
}

/// Reads the next MLLP frame, or `None` on timeout or end of stream.
async fn read_frame(
    stream: &mut TcpStream,
    decoder: &mut Decoder,
    deadline: Instant,
) -> io::Result<Option<Vec<u8>>> {
    let mut buffer = [0u8; 8192];
    loop {
        while let Some(event) = decoder.next_event() {
            match event {
                Event::Frame(frame) => return Ok(Some(frame)),
                Event::CommitAck | Event::CommitNak | Event::Discarded { .. } => {}
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        match tokio::time::timeout(remaining, stream.read(&mut buffer)).await {
            Ok(Ok(0)) => return Ok(None),
            Ok(Ok(n)) => decoder.push(&buffer[..n]),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Ok(None),
        }
    }
}

/// Sends `messages` to `target` over one connection, waiting for each
/// acknowledgment. `rate` limits messages per second.
pub async fn send(
    target: &str,
    messages: impl IntoIterator<Item = Vec<u8>>,
    rate: Option<f64>,
    ack_timeout: Duration,
    mut on_answer: impl FnMut(&[u8], Option<&[u8]>),
) -> io::Result<SendReport> {
    let mut stream = TcpStream::connect(target).await?;
    stream.set_nodelay(true)?;
    let mut decoder = Decoder::default();
    let mut report = SendReport::default();
    let interval = rate
        .filter(|r| *r > 0.0)
        .map(|r| Duration::from_secs_f64(1.0 / r));
    for message in messages {
        let started = Instant::now();
        let frame = encode(&message).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        stream.write_all(&frame).await?;
        report.sent += 1;
        let answer = read_frame(&mut stream, &mut decoder, started + ack_timeout).await?;
        match answer.as_deref().and_then(|ack| Message::parse(ack).ok()) {
            Some(ack) => {
                report.latencies.push(started.elapsed());
                match ack.get("MSA-1").map(|code| code.raw().to_vec()).as_deref() {
                    Some(b"AA" | b"CA") => report.accepted += 1,
                    Some(b"AE" | b"CE") => report.errors += 1,
                    Some(b"AR" | b"CR") => report.rejected += 1,
                    _ => report.unanswered += 1,
                }
            }
            None => report.unanswered += 1,
        }
        on_answer(&message, answer.as_deref());
        if let Some(interval) = interval {
            let elapsed = started.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
    }
    Ok(report)
}

/// How the simulated receiver answers.
#[derive(Debug, Clone, Copy)]
pub struct ReceiveOptions {
    /// The acknowledgment code for normal answers.
    pub ack: AckCode,
    /// Answer every n-th message with AE instead.
    pub fail_every: Option<u64>,
    /// Wait this long before answering.
    pub delay: Duration,
}

/// Accepts connections on `listen` and acknowledges every message, calling
/// `on_message` for each. Runs until the task is cancelled.
pub async fn receive(
    listen: &str,
    options: ReceiveOptions,
    on_message: impl Fn(&str, &[u8]) + Send + Sync + Clone + 'static,
) -> io::Result<()> {
    let listener = TcpListener::bind(listen).await?;
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let on_message = on_message.clone();
        let counter = counter.clone();
        tokio::spawn(async move {
            let peer = peer.to_string();
            let mut decoder = Decoder::default();
            let mut buffer = [0u8; 8192];
            loop {
                let n = match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                decoder.push(&buffer[..n]);
                while let Some(event) = decoder.next_event() {
                    let Event::Frame(frame) = event else { continue };
                    on_message(&peer, &frame);
                    let number = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let code = match options.fail_every {
                        Some(every) if every > 0 && number.is_multiple_of(every) => {
                            AckCode::ApplicationError
                        }
                        _ => options.ack,
                    };
                    if !options.delay.is_zero() {
                        tokio::time::sleep(options.delay).await;
                    }
                    let Ok(message) = Message::parse(&frame) else {
                        continue;
                    };
                    let control = format!("SIMACK{number}");
                    let Ok(ack) = build_ack(
                        &message,
                        &AckOptions {
                            code,
                            control_id: &control,
                            timestamp: "20260929120000",
                            text: None,
                            error: None,
                        },
                    ) else {
                        continue;
                    };
                    let Ok(frame) = encode(&ack.to_bytes()) else {
                        continue;
                    };
                    if stream.write_all(&frame).await.is_err() {
                        return;
                    }
                }
            }
        });
    }
}
