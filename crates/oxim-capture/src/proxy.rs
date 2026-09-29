//! Recording: a transparent TCP proxy and a serial line recorder that write
//! everything passing through them to a capture.
//!
//! Only traffic routed through the proxy is recorded. Watching a network
//! without rerouting it is the job of the companion netKit project.

use std::future::Future;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use oxim_model::Timestamp;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::format::{CaptureWriter, Direction, Event, Header, Record, Transport};

/// The current time as a capture timestamp.
pub fn now() -> Timestamp {
    Timestamp::from_system_time(SystemTime::now()).unwrap_or(Timestamp::from_unix_nanos(0))
}

type Writer = CaptureWriter<Box<dyn Write + Send>>;

/// A capture being written, shared by the tasks that record into it. Every
/// record is flushed, so the file is complete up to the last record even if
/// the program is killed.
#[derive(Clone)]
pub struct CaptureSink {
    writer: Arc<Mutex<Writer>>,
    connections: Arc<AtomicU32>,
    records: Arc<AtomicU32>,
}

impl std::fmt::Debug for CaptureSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureSink")
            .field("connections", &self.connections)
            .field("records", &self.records)
            .finish_non_exhaustive()
    }
}

impl CaptureSink {
    /// Writes the header and returns a sink for the records.
    pub fn new(writer: impl Write + Send + 'static, header: &Header) -> io::Result<Self> {
        let mut writer = CaptureWriter::new(Box::new(writer) as Box<dyn Write + Send>, header)?;
        writer.flush()?;
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            connections: Arc::new(AtomicU32::new(0)),
            records: Arc::new(AtomicU32::new(0)),
        })
    }

    /// Allocates the next connection number.
    pub fn next_connection(&self) -> u32 {
        self.connections.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Writes and flushes one record.
    pub fn record(&self, record: &Record) -> io::Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| io::Error::other("the capture writer failed earlier"))?;
        writer.write(record)?;
        writer.flush()?;
        self.records.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Records written so far.
    pub fn records(&self) -> u32 {
        self.records.load(Ordering::Relaxed)
    }

    /// Connections seen so far.
    pub fn connections(&self) -> u32 {
        self.connections.load(Ordering::SeqCst)
    }
}

/// Which side of the proxy the device is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceSide {
    /// The device connects to the proxy's listening address (the host is the
    /// target).
    #[default]
    Client,
    /// The device is the target the proxy connects to (the host connects to
    /// the proxy).
    Target,
}

/// Copies one direction of a connection and records every chunk.
async fn pump(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    sink: CaptureSink,
    connection: u32,
    direction: Direction,
    transport: Transport,
    peer: String,
) -> io::Result<()> {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = from.read(&mut buffer).await?;
        if n == 0 {
            let _ = to.shutdown().await;
            return Ok(());
        }
        let chunk = buffer[..n].to_vec();
        // Record before forwarding, so the capture never shows an answer
        // before the bytes it answers.
        sink.record(
            &Record::data(now(), connection, direction, transport, chunk.clone())
                .with_peer(peer.clone()),
        )?;
        to.write_all(&chunk).await?;
        to.flush().await?;
    }
}

/// A transparent TCP proxy that records both directions of every
/// connection.
#[derive(Debug)]
pub struct TcpProxy {
    listener: TcpListener,
    target: String,
    device: DeviceSide,
}

impl TcpProxy {
    /// Listens on `listen`; connections are forwarded to `target`.
    pub async fn bind(
        listen: &str,
        target: impl Into<String>,
        device: DeviceSide,
    ) -> io::Result<Self> {
        Ok(Self {
            listener: TcpListener::bind(listen).await?,
            target: target.into(),
            device,
        })
    }

    /// The address the proxy listens on.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts and forwards connections until `shutdown` completes.
    pub async fn run(
        self,
        sink: CaptureSink,
        shutdown: impl Future<Output = ()>,
    ) -> io::Result<()> {
        tokio::pin!(shutdown);
        loop {
            let (client, peer) = tokio::select! {
                () = &mut shutdown => return Ok(()),
                accepted = self.listener.accept() => accepted?,
            };
            let target = match TcpStream::connect(&self.target).await {
                Ok(target) => target,
                Err(e) => {
                    eprintln!("cannot connect to {}: {e}; closing {peer}", self.target);
                    continue;
                }
            };
            let _ = client.set_nodelay(true);
            let _ = target.set_nodelay(true);
            let connection = sink.next_connection();
            let peer = peer.to_string();
            sink.record(
                &Record::event(now(), connection, Event::Open, Transport::Tcp)
                    .with_peer(peer.clone()),
            )?;
            let (client_read, client_write) = client.into_split();
            let (target_read, target_write) = target.into_split();
            let outbound = match self.device {
                DeviceSide::Client => Direction::DeviceToHost,
                DeviceSide::Target => Direction::HostToDevice,
            };
            let sink = sink.clone();
            tokio::spawn(async move {
                let forward = pump(
                    client_read,
                    target_write,
                    sink.clone(),
                    connection,
                    outbound,
                    Transport::Tcp,
                    peer.clone(),
                );
                let backward = pump(
                    target_read,
                    client_write,
                    sink.clone(),
                    connection,
                    outbound.reverse(),
                    Transport::Tcp,
                    peer.clone(),
                );
                let (a, b) = tokio::join!(forward, backward);
                for result in [a, b] {
                    if let Err(e) = result
                        && e.kind() != io::ErrorKind::ConnectionReset
                    {
                        eprintln!("connection {connection} ({peer}): {e}");
                    }
                }
                let _ = sink.record(
                    &Record::event(now(), connection, Event::Close, Transport::Tcp).with_peer(peer),
                );
            });
        }
    }
}

/// Serial line settings for [`record_serial`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialLine {
    /// Port name, for example `COM3` or `/dev/ttyUSB0`.
    pub port: String,
    /// Baud rate.
    pub baud_rate: u32,
    /// Data bits: 5 to 8.
    pub data_bits: u8,
    /// Parity: `none`, `odd` or `even`.
    pub parity: String,
    /// Stop bits: 1 or 2.
    pub stop_bits: u8,
}

impl SerialLine {
    /// `port` at `baud_rate`, 8N1.
    pub fn new(port: impl Into<String>, baud_rate: u32) -> Self {
        Self {
            port: port.into(),
            baud_rate,
            data_bits: 8,
            parity: "none".into(),
            stop_bits: 1,
        }
    }

    fn open(&self) -> io::Result<tokio_serial::SerialStream> {
        use tokio_serial::SerialPortBuilderExt;
        let data_bits = match self.data_bits {
            5 => tokio_serial::DataBits::Five,
            6 => tokio_serial::DataBits::Six,
            7 => tokio_serial::DataBits::Seven,
            8 => tokio_serial::DataBits::Eight,
            other => {
                return Err(io::Error::other(format!(
                    "data bits must be 5 to 8, not {other}"
                )));
            }
        };
        let parity = match self.parity.as_str() {
            "none" => tokio_serial::Parity::None,
            "odd" => tokio_serial::Parity::Odd,
            "even" => tokio_serial::Parity::Even,
            other => {
                return Err(io::Error::other(format!(
                    "parity must be none, odd or even, not {other:?}"
                )));
            }
        };
        let stop_bits = match self.stop_bits {
            1 => tokio_serial::StopBits::One,
            2 => tokio_serial::StopBits::Two,
            other => {
                return Err(io::Error::other(format!(
                    "stop bits must be 1 or 2, not {other}"
                )));
            }
        };
        tokio_serial::new(&self.port, self.baud_rate)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .open_native_async()
            .map_err(io::Error::other)
    }
}

/// Records a serial line until `shutdown` completes.
///
/// With a `bridge` port, OXIM sits between the device (on `device`) and the
/// host (on `bridge`) and forwards both directions. Without one, the port
/// only listens, as on a monitoring cable, and everything read is recorded
/// as sent by the device.
pub async fn record_serial(
    device: &SerialLine,
    bridge: Option<&SerialLine>,
    sink: CaptureSink,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    let device_port = device.open()?;
    let connection = sink.next_connection();
    sink.record(
        &Record::event(now(), connection, Event::Open, Transport::Serial)
            .with_peer(device.port.clone()),
    )?;
    let result = match bridge {
        None => {
            let (device_read, _device_write) = tokio::io::split(device_port);
            tokio::select! {
                () = shutdown => Ok(()),
                result = pump(device_read, tokio::io::sink(), sink.clone(), connection, Direction::DeviceToHost, Transport::Serial, device.port.clone()) => result,
            }
        }
        Some(bridge) => {
            let host_port = bridge.open()?;
            let (device_read, device_write) = tokio::io::split(device_port);
            let (host_read, host_write) = tokio::io::split(host_port);
            let forward = pump(
                device_read,
                host_write,
                sink.clone(),
                connection,
                Direction::DeviceToHost,
                Transport::Serial,
                device.port.clone(),
            );
            let backward = pump(
                host_read,
                device_write,
                sink.clone(),
                connection,
                Direction::HostToDevice,
                Transport::Serial,
                bridge.port.clone(),
            );
            tokio::select! {
                () = shutdown => Ok(()),
                result = forward => result,
                result = backward => result,
            }
        }
    };
    sink.record(
        &Record::event(now(), connection, Event::Close, Transport::Serial)
            .with_peer(device.port.clone()),
    )?;
    result
}
