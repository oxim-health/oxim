//! The `.oximcap` capture format: JSON Lines, one header line followed by
//! one line per event.
//!
//! ```text
//! {"format":"oximcap","version":1,"created_at":"2026-09-29T09:00:00Z","transport":"tcp","protocol":"astm-lis01"}
//! {"timestamp":"2026-09-29T09:00:00.01Z","connection":1,"event":"open","transport":"tcp","peer":"127.0.0.1:50211"}
//! {"timestamp":"2026-09-29T09:00:00.02Z","connection":1,"event":"data","direction":"device-to-host","transport":"tcp","peer":"127.0.0.1:50211","data":"BQ=="}
//! {"timestamp":"2026-09-29T09:00:00.03Z","connection":1,"event":"data","direction":"host-to-device","transport":"tcp","peer":"127.0.0.1:50211","data":"Bg=="}
//! {"timestamp":"2026-09-29T09:00:01Z","connection":1,"event":"close","transport":"tcp","peer":"127.0.0.1:50211"}
//! ```
//!
//! | Header field | Meaning |
//! |---|---|
//! | `format` | always `oximcap` |
//! | `version` | format version, `1` |
//! | `created_at` | RFC 3339 time the capture started |
//! | `tool` | the program that wrote it, optional |
//! | `transport` | `tcp`, `serial` or `other` |
//! | `protocol` | `hl7v2-mllp`, `astm-lis01`, `astm-raw`, `poct1a` or `raw`; optional |
//! | `device`, `host` | where each side was (address or port name), optional |
//! | `description` | free text, optional |
//! | `anonymized` | `true` once protected health information was removed |
//!
//! | Record field | Meaning |
//! |---|---|
//! | `timestamp` | RFC 3339 time the bytes were seen |
//! | `connection` | connection number, starting at 1; one capture may hold several |
//! | `event` | `open`, `data` or `close` |
//! | `direction` | `device-to-host` or `host-to-device`; required for `data` |
//! | `transport` | as in the header |
//! | `peer` | the remote address or port name, optional |
//! | `data` | the bytes exactly as seen, standard base64; `data` events only |
//!
//! Readers ignore unknown fields, so later versions can add fields without
//! breaking older tools. Byte chunks are kept as they were read from the
//! transport: the boundaries carry timing information, not meaning.

use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::str::FromStr;

use oxim_model::Timestamp;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::base64;

/// The value of the header's `format` field.
pub const FORMAT_NAME: &str = "oximcap";

/// The format version this crate writes and reads.
pub const FORMAT_VERSION: u32 = 1;

/// Which way bytes travelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// From the device (analyzer, point-of-care device) to its host.
    DeviceToHost,
    /// From the host (OXIM or a LIS) to the device.
    HostToDevice,
}

impl Direction {
    /// The other direction.
    pub fn reverse(self) -> Self {
        match self {
            Self::DeviceToHost => Self::HostToDevice,
            Self::HostToDevice => Self::DeviceToHost,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DeviceToHost => "device-to-host",
            Self::HostToDevice => "host-to-device",
        })
    }
}

/// The transport the bytes were captured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// A TCP connection.
    Tcp,
    /// A serial line.
    Serial,
    /// Anything else, such as bytes assembled from files.
    Other,
}

/// The protocol spoken on the connection, when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    /// HL7 v2 over MLLP.
    #[serde(rename = "hl7v2-mllp")]
    Hl7v2Mllp,
    /// ASTM E1394 records in CLSI LIS01 (E1381) frames.
    #[serde(rename = "astm-lis01")]
    AstmLis01,
    /// ASTM E1394 records without LIS01 framing.
    #[serde(rename = "astm-raw")]
    AstmRaw,
    /// CLSI POCT1-A XML.
    #[serde(rename = "poct1a")]
    Poct1a,
    /// Anything else.
    #[serde(rename = "raw")]
    Raw,
}

impl FromStr for Protocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "hl7v2-mllp" | "mllp" | "hl7v2" => Self::Hl7v2Mllp,
            "astm-lis01" | "astm" => Self::AstmLis01,
            "astm-raw" => Self::AstmRaw,
            "poct1a" => Self::Poct1a,
            "raw" => Self::Raw,
            other => {
                return Err(format!(
                    "unknown protocol {other:?}; use hl7v2-mllp, astm-lis01, astm-raw, poct1a or raw"
                ));
            }
        })
    }
}

/// What a record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// A connection opened (or a serial port was opened).
    Open,
    /// Bytes were seen.
    #[default]
    Data,
    /// A connection closed.
    Close,
}

mod timestamp_text {
    use std::str::FromStr;

    use oxim_model::Timestamp;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Timestamp,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Timestamp, D::Error> {
        let text = String::deserialize(deserializer)?;
        Timestamp::from_str(&text).map_err(|_| {
            serde::de::Error::custom(format!("{text:?} is not an RFC 3339 time with offset"))
        })
    }
}

/// The first line of a capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Always [`FORMAT_NAME`].
    pub format: String,
    /// The format version, [`FORMAT_VERSION`].
    pub version: u32,
    /// When the capture started.
    #[serde(with = "timestamp_text")]
    pub created_at: Timestamp,
    /// The program that wrote the capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The transport.
    pub transport: Transport,
    /// The protocol, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
    /// Where the device was, for example `client of 0.0.0.0:5100` or `COM3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Where the host was, for example `10.0.0.5:5100`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether protected health information was removed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub anonymized: bool,
}

impl Header {
    /// A header for a capture that starts at `created_at`.
    pub fn new(transport: Transport, created_at: Timestamp) -> Self {
        Self {
            format: FORMAT_NAME.to_owned(),
            version: FORMAT_VERSION,
            created_at,
            tool: None,
            transport,
            protocol: None,
            device: None,
            host: None,
            description: None,
            anonymized: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RecordLine {
    #[serde(with = "timestamp_text")]
    timestamp: Timestamp,
    #[serde(default = "first_connection")]
    connection: u32,
    #[serde(default)]
    event: Event,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    direction: Option<Direction>,
    transport: Transport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    peer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<String>,
}

fn first_connection() -> u32 {
    1
}

/// One event of a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// When it happened.
    pub timestamp: Timestamp,
    /// The connection number, from 1.
    pub connection: u32,
    /// What happened.
    pub event: Event,
    /// Which way the bytes went; required for [`Event::Data`].
    pub direction: Option<Direction>,
    /// The transport.
    pub transport: Transport,
    /// The remote address or port name.
    pub peer: Option<String>,
    /// The bytes, for [`Event::Data`].
    pub data: Vec<u8>,
}

impl Record {
    /// A data record.
    pub fn data(
        timestamp: Timestamp,
        connection: u32,
        direction: Direction,
        transport: Transport,
        data: Vec<u8>,
    ) -> Self {
        Self {
            timestamp,
            connection,
            event: Event::Data,
            direction: Some(direction),
            transport,
            peer: None,
            data,
        }
    }

    /// An open or close record.
    pub fn event(
        timestamp: Timestamp,
        connection: u32,
        event: Event,
        transport: Transport,
    ) -> Self {
        Self {
            timestamp,
            connection,
            event,
            direction: None,
            transport,
            peer: None,
            data: Vec::new(),
        }
    }

    /// Sets the peer.
    pub fn with_peer(mut self, peer: impl Into<String>) -> Self {
        self.peer = Some(peer.into());
        self
    }

    fn to_line(&self) -> RecordLine {
        RecordLine {
            timestamp: self.timestamp,
            connection: self.connection,
            event: self.event,
            direction: self.direction,
            transport: self.transport,
            peer: self.peer.clone(),
            data: (self.event == Event::Data).then(|| base64::encode(&self.data)),
        }
    }

    fn from_line(line: RecordLine) -> Result<Self, String> {
        let data = match (&line.event, line.data) {
            (Event::Data, Some(text)) => {
                base64::decode(&text).ok_or("\"data\" is not valid base64")?
            }
            (Event::Data, None) => return Err("a data record needs \"data\"".into()),
            (_, Some(_)) => return Err("only data records carry \"data\"".into()),
            (_, None) => Vec::new(),
        };
        if line.event == Event::Data && line.direction.is_none() {
            return Err("a data record needs \"direction\"".into());
        }
        if line.connection == 0 {
            return Err("\"connection\" starts at 1".into());
        }
        Ok(Self {
            timestamp: line.timestamp,
            connection: line.connection,
            event: line.event,
            direction: line.direction,
            transport: line.transport,
            peer: line.peer,
            data,
        })
    }
}

/// Errors reading a capture.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CaptureError {
    /// Reading or writing failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The file has no header line.
    #[error("the capture is empty")]
    Empty,
    /// A line is not valid.
    #[error("line {line}: {message}")]
    Line {
        /// The 1-based line number.
        line: usize,
        /// What is wrong.
        message: String,
    },
}

/// A complete capture in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The header.
    pub header: Header,
    /// The records, in the order they were written.
    pub records: Vec<Record>,
}

impl Capture {
    /// An empty capture.
    pub fn new(header: Header) -> Self {
        Self {
            header,
            records: Vec::new(),
        }
    }

    /// Reads a capture. Blank lines are ignored.
    pub fn read(reader: impl BufRead) -> Result<Self, CaptureError> {
        let mut header = None;
        let mut records = Vec::new();
        for (index, line) in reader.lines().enumerate() {
            let line = line?;
            let number = index + 1;
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            let error = |message: String| CaptureError::Line {
                line: number,
                message,
            };
            if header.is_none() {
                let parsed: Header = serde_json::from_str(text)
                    .map_err(|e| error(format!("invalid header: {e}")))?;
                if parsed.format != FORMAT_NAME {
                    return Err(error(format!(
                        "not an {FORMAT_NAME} capture (format {:?})",
                        parsed.format
                    )));
                }
                if parsed.version != FORMAT_VERSION {
                    return Err(error(format!(
                        "unsupported {FORMAT_NAME} version {}; this tool reads version {FORMAT_VERSION}",
                        parsed.version
                    )));
                }
                header = Some(parsed);
                continue;
            }
            let parsed: RecordLine =
                serde_json::from_str(text).map_err(|e| error(format!("invalid record: {e}")))?;
            records.push(Record::from_line(parsed).map_err(error)?);
        }
        Ok(Self {
            header: header.ok_or(CaptureError::Empty)?,
            records,
        })
    }

    /// Reads a capture from bytes.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, CaptureError> {
        Self::read(bytes)
    }

    /// Reads a capture file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CaptureError> {
        let file = std::fs::File::open(path)?;
        Self::read(io::BufReader::new(file))
    }

    /// Writes the capture.
    pub fn write_to(&self, writer: impl Write) -> io::Result<()> {
        let mut out = CaptureWriter::new(writer, &self.header)?;
        for record in &self.records {
            out.write(record)?;
        }
        out.flush()
    }

    /// The capture as bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        // Writing to a vector cannot fail.
        let _ = self.write_to(&mut out);
        out
    }

    /// Writes the capture to a file.
    pub fn save(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        self.write_to(io::BufWriter::new(file))
    }

    /// The connection numbers in order of first appearance.
    pub fn connections(&self) -> Vec<u32> {
        let mut seen = Vec::new();
        for record in &self.records {
            if !seen.contains(&record.connection) {
                seen.push(record.connection);
            }
        }
        seen
    }

    /// All bytes sent one way on one connection.
    pub fn stream(&self, connection: u32, direction: Direction) -> Vec<u8> {
        self.records
            .iter()
            .filter(|r| {
                r.connection == connection
                    && r.event == Event::Data
                    && r.direction == Some(direction)
            })
            .flat_map(|r| r.data.iter().copied())
            .collect()
    }
}

/// Writes a capture line by line, so a capture survives a crash of the
/// writing program up to the last flushed record.
#[derive(Debug)]
pub struct CaptureWriter<W: Write> {
    inner: W,
}

impl<W: Write> CaptureWriter<W> {
    /// Writes the header line.
    pub fn new(mut inner: W, header: &Header) -> io::Result<Self> {
        serde_json::to_writer(&mut inner, header).map_err(io::Error::other)?;
        inner.write_all(b"\n")?;
        Ok(Self { inner })
    }

    /// Writes one record line.
    pub fn write(&mut self, record: &Record) -> io::Result<()> {
        serde_json::to_writer(&mut self.inner, &record.to_line()).map_err(io::Error::other)?;
        self.inner.write_all(b"\n")
    }

    /// Flushes the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    /// The underlying writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_unix_millis(1_790_000_000_000 + millis)
            .unwrap_or(Timestamp::from_unix_nanos(0))
    }

    #[test]
    fn writes_and_reads_captures() {
        let mut header = Header::new(Transport::Tcp, at(0));
        header.protocol = Some(Protocol::AstmLis01);
        header.tool = Some("test".into());
        let mut capture = Capture::new(header);
        capture.records = vec![
            Record::event(at(1), 1, Event::Open, Transport::Tcp).with_peer("127.0.0.1:5000"),
            Record::data(
                at(2),
                1,
                Direction::DeviceToHost,
                Transport::Tcp,
                vec![0x05],
            ),
            Record::data(
                at(3),
                1,
                Direction::HostToDevice,
                Transport::Tcp,
                vec![0x06],
            ),
            Record::data(
                at(4),
                2,
                Direction::DeviceToHost,
                Transport::Tcp,
                b"x\x00y".to_vec(),
            ),
            Record::event(at(5), 1, Event::Close, Transport::Tcp),
        ];
        let bytes = capture.to_bytes();
        let text = String::from_utf8(bytes.clone()).unwrap_or_default();
        assert!(
            text.starts_with(r#"{"format":"oximcap","version":1,"created_at":"#),
            "{text}"
        );
        assert!(
            text.contains(r#""direction":"device-to-host","transport":"tcp","data":"BQ=="}"#),
            "{text}"
        );
        let back = Capture::from_slice(&bytes).ok();
        assert_eq!(back.as_ref(), Some(&capture));
        assert_eq!(capture.connections(), [1, 2]);
        assert_eq!(capture.stream(1, Direction::DeviceToHost), [0x05]);
    }

    #[test]
    fn reports_the_offending_line() {
        let header = r#"{"format":"oximcap","version":1,"created_at":"2026-09-29T09:00:00Z","transport":"tcp"}"#;
        for (text, expected) in [
            (String::new(), "the capture is empty".to_owned()),
            (
                r#"{"format":"pcap","version":1,"created_at":"2026-09-29T09:00:00Z","transport":"tcp"}"#.to_owned(),
                "line 1: not an oximcap capture".to_owned(),
            ),
            (
                header.replace("\"version\":1", "\"version\":9"),
                "line 1: unsupported oximcap version 9".to_owned(),
            ),
            (
                format!("{header}\n\n{{\"timestamp\":\"2026-09-29T09:00:00Z\",\"transport\":\"tcp\",\"direction\":\"device-to-host\",\"data\":\"*\"}}"),
                "line 3: \"data\" is not valid base64".to_owned(),
            ),
            (
                format!("{header}\n{{\"timestamp\":\"2026-09-29T09:00:00Z\",\"transport\":\"tcp\",\"data\":\"BQ==\"}}"),
                "line 2: a data record needs \"direction\"".to_owned(),
            ),
            (
                format!("{header}\n{{\"timestamp\":\"yesterday\",\"transport\":\"tcp\"}}"),
                "line 2: invalid record".to_owned(),
            ),
        ] {
            let error = Capture::from_slice(text.as_bytes()).err().map(|e| e.to_string()).unwrap_or_default();
            assert!(error.starts_with(&expected), "{error} / {expected}");
        }
    }
}
