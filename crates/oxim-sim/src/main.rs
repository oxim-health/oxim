//! `oxim-sim`: simulators for analyzers, LIS systems and POCT devices.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use oxim_hl7::AckCode;
use oxim_sim::{astm, generate::Generator, mllp, poct};
use tokio::net::{TcpListener, TcpStream};

type SimResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(
    name = "oxim-sim",
    version,
    about = "Simulated analyzers, LIS systems and POCT devices for testing OXIM"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print or write synthetic messages.
    Generate {
        /// What to generate.
        kind: Kind,
        #[command(flatten)]
        source: Synthetic,
        /// Write one file per message into this directory instead of printing.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// HL7 over MLLP.
    Mllp {
        #[command(subcommand)]
        command: MllpCommand,
    },
    /// ASTM over LIS01 (TCP).
    Astm {
        #[command(subcommand)]
        command: AstmCommand,
    },
    /// POCT1-A device.
    Poct {
        #[command(subcommand)]
        command: PoctCommand,
    },
    /// Replay the device side of a capture (.oximcap) against a host.
    Replay {
        /// The capture file.
        capture: PathBuf,
        #[command(flatten)]
        endpoint: Endpoint,
        /// Seconds to wait for each answer the capture shows.
        #[arg(long, default_value_t = 10)]
        timeout: u64,
        /// Milliseconds of host silence that end an answer.
        #[arg(long, default_value_t = 300)]
        idle_ms: u64,
        /// Keep the recorded pauses between transmissions.
        #[arg(long)]
        realtime: bool,
        /// Print the exchange.
        #[arg(long)]
        verbose: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Kind {
    /// HL7 v2.5.1 ORU^R01 results.
    Hl7Oru,
    /// ASTM E1394 result messages.
    Astm,
}

#[derive(Debug, Args)]
struct Synthetic {
    /// Number of messages.
    #[arg(long, default_value_t = 1)]
    count: usize,
    /// Results per message.
    #[arg(long, default_value_t = 4)]
    results: usize,
    /// Random seed for reproducible data.
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

#[derive(Debug, Args)]
struct Messages {
    /// Send these files instead of synthetic messages (LF or CRLF line
    /// endings are converted to CR).
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    #[command(flatten)]
    synthetic: Synthetic,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Ack {
    /// Application accept.
    Aa,
    /// Application error.
    Ae,
    /// Application reject.
    Ar,
}

#[derive(Debug, Subcommand)]
enum MllpCommand {
    /// Send messages and wait for each acknowledgment.
    Send {
        /// host:port of the receiver.
        #[arg(long)]
        to: String,
        #[command(flatten)]
        messages: Messages,
        /// Messages per second (unlimited by default).
        #[arg(long)]
        rate: Option<f64>,
        /// Seconds to wait for each acknowledgment.
        #[arg(long, default_value_t = 10)]
        timeout: u64,
        /// Print every acknowledgment.
        #[arg(long)]
        verbose: bool,
    },
    /// Act as a LIS: accept connections and acknowledge every message.
    Receive {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:2575")]
        listen: String,
        /// Acknowledgment code.
        #[arg(long, value_enum, default_value_t = Ack::Aa)]
        ack: Ack,
        /// Answer every n-th message with AE.
        #[arg(long)]
        fail_every: Option<u64>,
        /// Milliseconds to wait before answering.
        #[arg(long, default_value_t = 0)]
        delay_ms: u64,
        /// Only count messages instead of printing them.
        #[arg(long)]
        quiet: bool,
    },
}

#[derive(Debug, Args)]
struct Endpoint {
    /// Connect to host:port.
    #[arg(long, conflicts_with = "listen")]
    to: Option<String>,
    /// Listen on this address and wait for one connection.
    #[arg(long)]
    listen: Option<String>,
}

impl Endpoint {
    async fn open(&self) -> SimResult<TcpStream> {
        match (&self.to, &self.listen) {
            (Some(to), None) => Ok(TcpStream::connect(to).await?),
            (None, Some(listen)) => {
                let listener = TcpListener::bind(listen).await?;
                eprintln!("waiting for a connection on {listen}");
                Ok(listener.accept().await?.0)
            }
            _ => Err("use --to or --listen".into()),
        }
    }
}

#[derive(Debug, Subcommand)]
enum AstmCommand {
    /// Act as an analyzer and send result messages.
    Send {
        #[command(flatten)]
        endpoint: Endpoint,
        #[command(flatten)]
        messages: Messages,
    },
    /// Act as a host and print received messages.
    Receive {
        #[command(flatten)]
        endpoint: Endpoint,
    },
}

#[derive(Debug, Subcommand)]
enum PoctCommand {
    /// Run a device conversation: HEL, DST, observations, EOT, END.
    Send {
        /// host:port of the data manager.
        #[arg(long)]
        to: String,
        /// Number of observations.
        #[arg(long, default_value_t = 3)]
        observations: usize,
        /// Seconds to wait for each answer.
        #[arg(long, default_value_t = 10)]
        timeout: u64,
    },
}

fn read_messages(files: &[PathBuf], synthetic: &Synthetic, kind: Kind) -> SimResult<Vec<Vec<u8>>> {
    if !files.is_empty() {
        return files
            .iter()
            .map(|path| -> SimResult<Vec<u8>> {
                let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(normalize_line_endings(&text))
            })
            .collect();
    }
    let mut generator = Generator::new(synthetic.seed);
    Ok((0..synthetic.count)
        .map(|_| match kind {
            Kind::Hl7Oru => generator.hl7_oru(synthetic.results),
            Kind::Astm => generator.astm_results(synthetic.results),
        })
        .collect())
}

/// Converts LF and CRLF line endings to CR, the HL7 and ASTM separator.
fn normalize_line_endings(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut iter = text.iter().peekable();
    while let Some(&b) = iter.next() {
        match b {
            b'\r' => {
                if iter.peek() == Some(&&b'\n') {
                    iter.next();
                }
                out.push(b'\r');
            }
            b'\n' => out.push(b'\r'),
            other => out.push(other),
        }
    }
    out
}

fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace('\r', "\n")
        .trim_end()
        .to_owned()
}

fn write_files(dir: &Path, messages: &[Vec<u8>], extension: &str) -> SimResult<()> {
    std::fs::create_dir_all(dir)?;
    for (index, message) in messages.iter().enumerate() {
        let path = dir.join(format!("message-{:05}.{extension}", index + 1));
        std::fs::write(&path, message)?;
        println!("{}", path.display());
    }
    Ok(())
}

async fn run(cli: Cli) -> SimResult<()> {
    match cli.command {
        Command::Generate { kind, source, out } => {
            let messages = read_messages(&[], &source, kind)?;
            match out {
                Some(dir) => write_files(
                    &dir,
                    &messages,
                    match kind {
                        Kind::Hl7Oru => "hl7",
                        Kind::Astm => "astm",
                    },
                )?,
                None => {
                    for message in &messages {
                        println!("{}\n", show(message));
                    }
                }
            }
        }
        Command::Mllp {
            command:
                MllpCommand::Send {
                    to,
                    messages,
                    rate,
                    timeout,
                    verbose,
                },
        } => {
            let batch = read_messages(&messages.files, &messages.synthetic, Kind::Hl7Oru)?;
            let report = mllp::send(&to, batch, rate, Duration::from_secs(timeout), |_, ack| {
                if verbose {
                    match ack {
                        Some(ack) => println!("{}\n", show(ack)),
                        None => println!("(no acknowledgment)\n"),
                    }
                }
            })
            .await?;
            println!("{}", report.summary());
        }
        Command::Mllp {
            command:
                MllpCommand::Receive {
                    listen,
                    ack,
                    fail_every,
                    delay_ms,
                    quiet,
                },
        } => {
            let code = match ack {
                Ack::Aa => AckCode::ApplicationAccept,
                Ack::Ae => AckCode::ApplicationError,
                Ack::Ar => AckCode::ApplicationReject,
            };
            eprintln!("receiving HL7 on {listen}; Ctrl+C to stop");
            let options = mllp::ReceiveOptions {
                ack: code,
                fail_every,
                delay: Duration::from_millis(delay_ms),
            };
            let count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let seen = count.clone();
            tokio::select! {
                result = mllp::receive(&listen, options, move |peer, message| {
                    let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if !quiet {
                        println!("--- #{n} from {peer}\n{}", show(message));
                    }
                }) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
            println!(
                "received {}",
                count.load(std::sync::atomic::Ordering::SeqCst)
            );
        }
        Command::Astm {
            command: AstmCommand::Send { endpoint, messages },
        } => {
            let batch = read_messages(&messages.files, &messages.synthetic, Kind::Astm)?;
            let stream = endpoint.open().await?;
            let report = astm::send(stream, batch, |message| {
                println!("received:\n{}", show(message))
            })
            .await?;
            println!(
                "queued={} delivered={} aborted={} received={}",
                report.queued, report.delivered, report.aborted, report.received
            );
        }
        Command::Astm {
            command: AstmCommand::Receive { endpoint },
        } => {
            let stream = endpoint.open().await?;
            let report =
                astm::receive(stream, |message| println!("---\n{}", show(message))).await?;
            println!("received {}", report.received);
        }
        Command::Poct {
            command:
                PoctCommand::Send {
                    to,
                    observations,
                    timeout,
                },
        } => {
            let received = poct::send(
                &to,
                observations,
                Duration::from_secs(timeout),
                |document| {
                    println!("host: {}", String::from_utf8_lossy(document));
                },
            )
            .await?;
            println!("conversation finished; host sent {received} documents");
        }
        Command::Replay {
            capture,
            endpoint,
            timeout,
            idle_ms,
            realtime,
            verbose,
        } => {
            let capture = oxim_capture::Capture::load(&capture)
                .map_err(|e| format!("{}: {e}", capture.display()))?;
            let target = match (endpoint.to, endpoint.listen) {
                (Some(to), None) => oxim_capture::Endpoint::Connect(to),
                (None, Some(listen)) => {
                    eprintln!("waiting for the host to connect to {listen}");
                    oxim_capture::Endpoint::Listen(listen)
                }
                _ => return Err("use --to or --listen".into()),
            };
            let options = oxim_capture::ReplayOptions {
                answer_timeout: Duration::from_secs(timeout),
                idle: Duration::from_millis(idle_ms),
                realtime,
                connect_timeout: Duration::from_secs(timeout),
            };
            let report = oxim_capture::replay(&capture, &target, &options).await?;
            if verbose {
                for (direction, bytes) in &report.transcript {
                    let arrow = match direction {
                        oxim_capture::Direction::DeviceToHost => "device >",
                        oxim_capture::Direction::HostToDevice => "< host  ",
                    };
                    println!("{arrow} {}", oxim_capture::describe(bytes));
                }
            }
            println!(
                "connections={} bytes_sent={} answers={} unanswered={}",
                report.connections, report.bytes_sent, report.answers, report.unanswered
            );
            if report.unanswered > 0 {
                return Err(
                    format!("{} expected answer(s) did not arrive", report.unanswered).into(),
                );
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_line_endings() {
        assert_eq!(normalize_line_endings(b"a\r\nb\nc\rd"), b"a\rb\rc\rd");
    }
}
