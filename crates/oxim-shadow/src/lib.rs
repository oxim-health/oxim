//! Shadow mode: check an OXIM channel against a running Mirth Connect
//! installation without touching a single message.
//!
//! Traffic of the Mirth channel is captured passively — with the
//! companion netKit project, tcpdump or Wireshark (PCAP/PCAPNG), or with
//! `oxim-capture` (`.oximcap`). Shadow mode takes the messages the devices
//! sent to Mirth, runs them through the OXIM channel in an isolated
//! in-process engine (its destinations only record what they would send),
//! and compares OXIM's output with what Mirth sent to each destination,
//! field by field:
//!
//! ```text
//! analyzer ──▶ Mirth :6661 ──▶ LIS :6662        (captured)
//!                │                   │
//!   inbound messages           Mirth's output
//!                ▼                   │
//!        OXIM channel (replay) ──▶ compare ──▶ report
//! ```
//!
//! HL7 v2 and ASTM messages are compared field by field (`PID-5`,
//! `OBX[2]-5`, `R-4`), other data line by line. Paths that always differ,
//! such as timestamps and control identifiers, are ignored with
//! [`CompareOptions::ignore`]. Messages are paired in order, or by a key
//! path such as `OBR-3`. Reports show which fields differ; their values are
//! hidden unless [`CompareOptions::show_values`] is set, because they may
//! be patient data.

mod compare;
mod extract;
mod flows;
mod pcap;
mod replay;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use oxim_capture::{Capture, Direction};
use oxim_core::{ChannelConfig, Registry};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use compare::{
    CompareOptions, DestinationReport, Difference, FieldDifference, compare, differences,
};
pub use extract::{Captured, Framing, messages};
pub use flows::{Chunk, Stream, reassemble};
pub use pcap::{Segment, is_pcap, segments};
pub use replay::{Replay, run as replay};

/// Errors of shadow mode.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ShadowError {
    /// The capture cannot be read.
    #[error("capture: {0}")]
    Capture(String),
    /// The replay engine failed.
    #[error("replay: {0}")]
    Engine(String),
    /// The options do not fit the channel.
    #[error("{0}")]
    Options(String),
}

/// A destination of the channel and where Mirth's traffic to it was
/// captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationCapture {
    /// The OXIM destination identifier.
    pub destination: String,
    /// The port Mirth sent to (for PCAP captures).
    pub port: u16,
    /// How messages are framed on that connection.
    pub framing: Framing,
}

/// What to replay and compare.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowOptions {
    /// The port Mirth's channel listens on (for PCAP captures).
    pub inbound_port: u16,
    /// How inbound messages are framed.
    pub inbound_framing: Framing,
    /// The compared destinations.
    pub destinations: Vec<DestinationCapture>,
    /// How messages are compared.
    pub compare: CompareOptions,
    /// How long one message may take to process in the replay.
    pub timeout: Duration,
}

/// The captured traffic of one Mirth channel.
#[derive(Debug, Clone, Default)]
pub struct Traffic {
    /// What the devices sent to Mirth.
    pub inbound: Vec<Captured>,
    /// What Mirth answered the devices.
    pub replies: Vec<Captured>,
    /// What Mirth sent, per OXIM destination.
    pub outbound: BTreeMap<String, Vec<Captured>>,
}

fn sorted(mut messages: Vec<Captured>) -> Vec<Captured> {
    messages.sort_by_key(|m| m.timestamp);
    messages
}

impl Traffic {
    /// The traffic of a PCAP or PCAPNG capture, selected by port.
    pub fn from_pcap(bytes: &[u8], options: &ShadowOptions) -> Result<Self, ShadowError> {
        let streams = reassemble(&segments(bytes)?);
        let collect = |matches: &dyn Fn(&Stream) -> bool, framing: Framing| {
            sorted(
                streams
                    .iter()
                    .filter(|s| matches(s))
                    .flat_map(|s| messages(&s.chunks, framing))
                    .collect(),
            )
        };
        let inbound_port = options.inbound_port;
        let mut traffic = Self {
            inbound: collect(
                &|s| s.destination.port() == inbound_port,
                options.inbound_framing,
            ),
            replies: collect(
                &|s| s.source.port() == inbound_port,
                options.inbound_framing,
            ),
            outbound: BTreeMap::new(),
        };
        for destination in &options.destinations {
            let port = destination.port;
            traffic.outbound.insert(
                destination.destination.clone(),
                collect(&|s| s.destination.port() == port, destination.framing),
            );
        }
        Ok(traffic)
    }

    /// The traffic of `.oximcap` captures: `inbound` recorded between the
    /// devices and Mirth, and one capture per destination recorded between
    /// Mirth and the receiver (Mirth being the connecting side).
    pub fn from_captures(
        inbound: &Capture,
        outbound: &[(String, Capture)],
        options: &ShadowOptions,
    ) -> Self {
        let side = |capture: &Capture, direction: Direction, framing: Framing| {
            let mut found = Vec::new();
            for connection in capture.connections() {
                let chunks: Vec<Chunk> = capture
                    .records
                    .iter()
                    .filter(|r| {
                        r.connection == connection
                            && r.event == oxim_capture::Event::Data
                            && r.direction == Some(direction)
                    })
                    .map(|r| Chunk {
                        timestamp: r.timestamp,
                        data: r.data.clone(),
                    })
                    .collect();
                found.extend(messages(&chunks, framing));
            }
            sorted(found)
        };
        let framing_of = |destination: &str| {
            options
                .destinations
                .iter()
                .find(|d| d.destination == destination)
                .map_or(Framing::Mllp, |d| d.framing)
        };
        Self {
            inbound: side(inbound, Direction::DeviceToHost, options.inbound_framing),
            replies: side(inbound, Direction::HostToDevice, options.inbound_framing),
            outbound: outbound
                .iter()
                .map(|(destination, capture)| {
                    (
                        destination.clone(),
                        side(capture, Direction::DeviceToHost, framing_of(destination)),
                    )
                })
                .collect(),
        }
    }
}

/// The result of a shadow run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowReport {
    /// The OXIM channel.
    pub channel: String,
    /// Messages replayed.
    pub inbound_messages: usize,
    /// Inbound messages the OXIM channel filtered.
    pub filtered: usize,
    /// Inbound messages that ended in error in OXIM, with the reason.
    pub errors: Vec<(usize, String)>,
    /// The comparison per destination.
    pub destinations: Vec<DestinationReport>,
    /// The comparison of the replies to the devices, when the channel
    /// answers requests.
    pub replies: Option<DestinationReport>,
}

impl ShadowReport {
    /// Whether OXIM produced exactly what Mirth sent.
    pub fn is_identical(&self) -> bool {
        self.errors.is_empty()
            && self
                .destinations
                .iter()
                .chain(self.replies.iter())
                .all(|d| d.different.is_empty() && d.missing_in_oxim == 0 && d.extra_in_oxim == 0)
    }

    /// The report as JSON.
    pub fn to_json(&self) -> Result<String, ShadowError> {
        serde_json::to_string_pretty(self).map_err(|e| ShadowError::Options(e.to_string()))
    }

    /// The report as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let verdict = if self.is_identical() {
            "OXIM produced the same messages as Mirth Connect"
        } else {
            "OXIM's output differs from Mirth Connect's"
        };
        let _ = writeln!(out, "# Shadow comparison: channel `{}`\n", self.channel);
        let _ = writeln!(out, "{verdict}.\n");
        let _ = writeln!(
            out,
            "{} inbound messages replayed, {} filtered, {} in error.\n",
            self.inbound_messages,
            self.filtered,
            self.errors.len()
        );
        let _ = writeln!(
            out,
            "| Destination | Mirth | OXIM | Identical | Different | Missing in OXIM | Extra in OXIM |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");
        for d in self.destinations.iter().chain(self.replies.iter()) {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} |",
                d.destination,
                d.mirth_messages,
                d.oxim_messages,
                d.identical,
                d.different.len(),
                d.missing_in_oxim,
                d.extra_in_oxim
            );
        }
        for d in self.destinations.iter().chain(self.replies.iter()) {
            if d.different.is_empty() {
                continue;
            }
            let _ = writeln!(out, "\n## Differences: {}\n", d.destination);
            for difference in d.different.iter().take(50) {
                let key = difference
                    .key
                    .as_deref()
                    .map(|k| format!(" (key {k})"))
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "- Mirth message {}{key}: {}",
                    difference.mirth_index + 1,
                    difference
                        .fields
                        .iter()
                        .map(|f| format!("`{}` Mirth {} / OXIM {}", f.path, f.mirth, f.oxim))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
            if d.different.len() > 50 {
                let _ = writeln!(out, "- … and {} more", d.different.len() - 50);
            }
        }
        if !self.errors.is_empty() {
            let _ = writeln!(out, "\n## Errors in OXIM\n");
            for (index, error) in &self.errors {
                let _ = writeln!(out, "- inbound message {}: {error}", index + 1);
            }
        }
        out
    }
}

/// Replays `traffic` through `channel` and compares OXIM's output with
/// Mirth's.
pub async fn shadow(
    channel: &ChannelConfig,
    registry: Registry,
    traffic: &Traffic,
    options: &ShadowOptions,
) -> Result<ShadowReport, ShadowError> {
    for destination in &options.destinations {
        if !channel
            .destinations
            .iter()
            .any(|d| d.id.as_str() == destination.destination)
        {
            return Err(ShadowError::Options(format!(
                "channel {} has no destination {}",
                channel.id, destination.destination
            )));
        }
    }
    let inbound: Vec<Vec<u8>> = traffic.inbound.iter().map(|m| m.data.clone()).collect();
    let replay = replay::run(channel, registry, &inbound, options.timeout).await?;
    let mut report = ShadowReport {
        channel: channel.id.to_string(),
        inbound_messages: inbound.len(),
        filtered: replay.filtered.len(),
        errors: replay.errors.clone(),
        ..ShadowReport::default()
    };
    for destination in &options.destinations {
        let mirth: Vec<Vec<u8>> = traffic
            .outbound
            .get(&destination.destination)
            .map(|found| found.iter().map(|m| m.data.clone()).collect())
            .unwrap_or_default();
        let oxim = replay
            .outputs
            .iter()
            .find(|(id, _)| id.as_str() == destination.destination)
            .map(|(_, outputs)| outputs.clone())
            .unwrap_or_default();
        report.destinations.push(compare(
            &destination.destination,
            &mirth,
            &oxim,
            &options.compare,
        ));
    }
    if channel.source.response.is_some() {
        let mirth: Vec<Vec<u8>> = traffic.replies.iter().map(|m| m.data.clone()).collect();
        let oxim: Vec<(usize, Vec<u8>)> = replay
            .replies
            .iter()
            .enumerate()
            .filter_map(|(i, reply)| reply.clone().map(|r| (i, r)))
            .collect();
        report.replies = Some(compare("replies", &mirth, &oxim, &options.compare));
    }
    Ok(report)
}

/// Default paths ignored when comparing HL7 v2 messages: the message time
/// and control identifier, which differ between any two engines.
pub const DEFAULT_IGNORE: [&str; 2] = ["MSH-7", "MSH-10"];

/// The default replay timeout per message.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
