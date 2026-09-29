//! `oxim-capture`: record device conversations to `.oximcap` files.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use oxim_capture::{
    Capture, CaptureSink, DeviceSide, Direction, Event, Header, Protocol, SerialLine, TcpProxy,
    Transport, describe, now, record_serial,
};

type CliResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(
    name = "oxim-capture",
    version,
    about = "Record device conversations routed through a TCP proxy or a serial bridge"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Side {
    /// The device connects to --listen; the host is --target.
    Client,
    /// The device is --target; the host connects to --listen.
    Target,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Forward TCP connections from --listen to --target and record both
    /// directions.
    Tcp {
        /// Address the proxy listens on.
        #[arg(long)]
        listen: String,
        /// Address the proxy forwards to.
        #[arg(long)]
        target: String,
        /// Which side is the device.
        #[arg(long, value_enum, default_value_t = Side::Client)]
        device: Side,
        /// The capture file to write.
        #[arg(long)]
        out: PathBuf,
        /// hl7v2-mllp, astm-lis01, astm-raw, poct1a or raw.
        #[arg(long)]
        protocol: Option<Protocol>,
        /// A note stored in the capture header.
        #[arg(long)]
        description: Option<String>,
    },
    /// Record a serial line; with --bridge, forward between the device port
    /// and the host port.
    Serial {
        /// The device's port, for example COM3 or /dev/ttyUSB0.
        #[arg(long)]
        port: String,
        /// Baud rate.
        #[arg(long, default_value_t = 9600)]
        baud: u32,
        /// Data bits.
        #[arg(long, default_value_t = 8)]
        data_bits: u8,
        /// none, odd or even.
        #[arg(long, default_value = "none")]
        parity: String,
        /// Stop bits.
        #[arg(long, default_value_t = 1)]
        stop_bits: u8,
        /// The host's port; without it the device port is only listened to.
        #[arg(long)]
        bridge: Option<String>,
        /// The capture file to write.
        #[arg(long)]
        out: PathBuf,
        /// hl7v2-mllp, astm-lis01, astm-raw, poct1a or raw.
        #[arg(long)]
        protocol: Option<Protocol>,
        /// A note stored in the capture header.
        #[arg(long)]
        description: Option<String>,
    },
    /// Print a capture as a readable transcript.
    Show {
        /// The capture file.
        file: PathBuf,
    },
}

fn create(out: &Path) -> CliResult<std::io::BufWriter<std::fs::File>> {
    if out.exists() {
        return Err(format!("{} exists; choose another file", out.display()).into());
    }
    Ok(std::io::BufWriter::new(std::fs::File::create(out)?))
}

async fn ctrl_c() {
    let _ = tokio::signal::ctrl_c().await;
}

fn header(transport: Transport, protocol: Option<Protocol>, description: Option<String>) -> Header {
    let mut header = Header::new(transport, now());
    header.tool = Some(format!("oxim-capture {}", env!("CARGO_PKG_VERSION")));
    header.protocol = protocol;
    header.description = description;
    header
}

async fn run(command: Command) -> CliResult<()> {
    match command {
        Command::Tcp {
            listen,
            target,
            device,
            out,
            protocol,
            description,
        } => {
            let side = match device {
                Side::Client => DeviceSide::Client,
                Side::Target => DeviceSide::Target,
            };
            let mut header = header(Transport::Tcp, protocol, description);
            match side {
                DeviceSide::Client => {
                    header.device = Some(format!("client of {listen}"));
                    header.host = Some(target.clone());
                }
                DeviceSide::Target => {
                    header.device = Some(target.clone());
                    header.host = Some(format!("client of {listen}"));
                }
            }
            let sink = CaptureSink::new(create(&out)?, &header)?;
            let proxy = TcpProxy::bind(&listen, target.clone(), side).await?;
            eprintln!(
                "recording {} <-> {target} to {}; Ctrl+C stops",
                proxy.local_addr()?,
                out.display()
            );
            proxy.run(sink.clone(), ctrl_c()).await?;
            eprintln!(
                "stopped: {} connection(s), {} record(s)",
                sink.connections(),
                sink.records()
            );
            Ok(())
        }
        Command::Serial {
            port,
            baud,
            data_bits,
            parity,
            stop_bits,
            bridge,
            out,
            protocol,
            description,
        } => {
            let line = |port: String| SerialLine {
                port,
                baud_rate: baud,
                data_bits,
                parity: parity.clone(),
                stop_bits,
            };
            let device = line(port.clone());
            let bridge = bridge.map(line);
            let mut header = header(Transport::Serial, protocol, description);
            header.device = Some(port);
            header.host = bridge.as_ref().map(|b| b.port.clone());
            let sink = CaptureSink::new(create(&out)?, &header)?;
            eprintln!(
                "recording {} to {}; Ctrl+C stops",
                device.port,
                out.display()
            );
            record_serial(&device, bridge.as_ref(), sink.clone(), ctrl_c()).await?;
            eprintln!("stopped: {} record(s)", sink.records());
            Ok(())
        }
        Command::Show { file } => {
            let capture = Capture::load(&file)?;
            let h = &capture.header;
            println!(
                "{} {:?} {} records, created {}{}",
                file.display(),
                h.transport,
                capture.records.len(),
                h.created_at,
                h.protocol
                    .map(|p| format!(
                        ", protocol {}",
                        serde_json::to_string(&p).unwrap_or_default()
                    ))
                    .unwrap_or_default()
            );
            for record in &capture.records {
                let what = match (record.event, record.direction) {
                    (Event::Data, Some(Direction::DeviceToHost)) => "device >".to_owned(),
                    (Event::Data, Some(Direction::HostToDevice)) => "< host  ".to_owned(),
                    (event, _) => format!("{event:?}").to_lowercase(),
                };
                println!(
                    "{} #{} {what} {}",
                    record.timestamp,
                    record.connection,
                    describe(&record.data)
                );
            }
            Ok(())
        }
    }
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
    match runtime.block_on(run(cli.command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
