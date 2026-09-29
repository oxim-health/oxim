//! Reading PCAP and PCAPNG files (as written by netKit, tcpdump or
//! Wireshark) down to TCP segments.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use oxim_model::Timestamp;

use crate::ShadowError;

/// One TCP segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// When it was captured.
    pub timestamp: Timestamp,
    /// The sender.
    pub source: SocketAddr,
    /// The receiver.
    pub destination: SocketAddr,
    /// The sequence number of the first payload byte.
    pub sequence: u32,
    /// Whether SYN was set.
    pub syn: bool,
    /// Whether FIN or RST was set.
    pub fin: bool,
    /// The payload.
    pub payload: Vec<u8>,
}

/// A captured frame and its link type.
struct Frame<'a> {
    timestamp: Timestamp,
    link: u32,
    data: &'a [u8],
}

fn invalid(detail: impl Into<String>) -> ShadowError {
    ShadowError::Capture(detail.into())
}

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, bytes: &[u8], at: usize) -> Result<u16, ShadowError> {
        let raw: [u8; 2] = bytes
            .get(at..at + 2)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| invalid("truncated capture"))?;
        Ok(match self {
            Self::Little => u16::from_le_bytes(raw),
            Self::Big => u16::from_be_bytes(raw),
        })
    }

    fn u32(self, bytes: &[u8], at: usize) -> Result<u32, ShadowError> {
        let raw: [u8; 4] = bytes
            .get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| invalid("truncated capture"))?;
        Ok(match self {
            Self::Little => u32::from_le_bytes(raw),
            Self::Big => u32::from_be_bytes(raw),
        })
    }
}

fn timestamp(seconds: u64, fraction: u64, per_second: u64) -> Timestamp {
    let nanos = seconds
        .saturating_mul(1_000_000_000)
        .saturating_add(fraction.saturating_mul(1_000_000_000) / per_second.max(1));
    Timestamp::from_unix_nanos(i64::try_from(nanos).unwrap_or(i64::MAX))
}

/// Whether `bytes` start like a PCAP or PCAPNG file.
pub fn is_pcap(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some(
            [0xd4, 0xc3, 0xb2, 0xa1]
                | [0xa1, 0xb2, 0xc3, 0xd4]
                | [0x4d, 0x3c, 0xb2, 0xa1]
                | [0xa1, 0xb2, 0x3c, 0x4d]
                | [0x0a, 0x0d, 0x0d, 0x0a]
        )
    )
}

fn classic(bytes: &[u8]) -> Result<Vec<Frame<'_>>, ShadowError> {
    let (endian, per_second) = match bytes.get(..4) {
        Some([0xd4, 0xc3, 0xb2, 0xa1]) => (Endian::Little, 1_000_000),
        Some([0xa1, 0xb2, 0xc3, 0xd4]) => (Endian::Big, 1_000_000),
        Some([0x4d, 0x3c, 0xb2, 0xa1]) => (Endian::Little, 1_000_000_000),
        Some([0xa1, 0xb2, 0x3c, 0x4d]) => (Endian::Big, 1_000_000_000),
        _ => return Err(invalid("not a PCAP file")),
    };
    let link = endian.u32(bytes, 20)? & 0x0fff_ffff;
    let mut frames = Vec::new();
    let mut at = 24;
    while at + 16 <= bytes.len() {
        let seconds = u64::from(endian.u32(bytes, at)?);
        let fraction = u64::from(endian.u32(bytes, at + 4)?);
        let length = endian.u32(bytes, at + 8)? as usize;
        let data = bytes
            .get(at + 16..at + 16 + length)
            .ok_or_else(|| invalid("truncated PCAP record"))?;
        frames.push(Frame {
            timestamp: timestamp(seconds, fraction, per_second),
            link,
            data,
        });
        at += 16 + length;
    }
    Ok(frames)
}

fn next_generation(bytes: &[u8]) -> Result<Vec<Frame<'_>>, ShadowError> {
    let mut frames = Vec::new();
    let mut endian = Endian::Little;
    // Link type and timestamp resolution per interface of the section.
    let mut interfaces: Vec<(u32, u64)> = Vec::new();
    let mut at = 0;
    while at + 12 <= bytes.len() {
        let kind_raw = &bytes[at..at + 4];
        if kind_raw == [0x0a, 0x0d, 0x0d, 0x0a] {
            endian = match bytes.get(at + 8..at + 12) {
                Some([0x4d, 0x3c, 0x2b, 0x1a]) => Endian::Little,
                Some([0x1a, 0x2b, 0x3c, 0x4d]) => Endian::Big,
                _ => return Err(invalid("invalid PCAPNG byte order")),
            };
            interfaces.clear();
        }
        let kind = endian.u32(bytes, at)?;
        let length = endian.u32(bytes, at + 4)? as usize;
        if length < 12 || at + length > bytes.len() {
            return Err(invalid("truncated PCAPNG block"));
        }
        let body = &bytes[at + 8..at + length - 4];
        match kind {
            // Interface description: link type and options.
            1 => {
                let link = u32::from(endian.u16(body, 0)?);
                let mut per_second = 1_000_000u64;
                let mut option = 8;
                while option + 4 <= body.len() {
                    let code = endian.u16(body, option)?;
                    let size = endian.u16(body, option + 2)? as usize;
                    if code == 0 {
                        break;
                    }
                    if code == 9
                        && let Some(&resolution) = body.get(option + 4)
                    {
                        per_second = if resolution & 0x80 == 0 {
                            10u64.saturating_pow(u32::from(resolution))
                        } else {
                            1u64 << u32::from(resolution & 0x7f).min(63)
                        };
                    }
                    option += 4 + size.div_ceil(4) * 4;
                }
                interfaces.push((link, per_second));
            }
            // Enhanced packet.
            6 => {
                let interface = endian.u32(body, 0)? as usize;
                let (link, per_second) = *interfaces
                    .get(interface)
                    .ok_or_else(|| invalid("packet of an undeclared interface"))?;
                let high = u64::from(endian.u32(body, 4)?);
                let low = u64::from(endian.u32(body, 8)?);
                let captured = endian.u32(body, 12)? as usize;
                let data = body
                    .get(20..20 + captured)
                    .ok_or_else(|| invalid("truncated PCAPNG packet"))?;
                let ticks = (high << 32) | low;
                frames.push(Frame {
                    timestamp: timestamp(ticks / per_second, ticks % per_second, per_second),
                    link,
                    data,
                });
            }
            // Simple packet: no timestamp, first interface.
            3 => {
                let (link, _) = *interfaces
                    .first()
                    .ok_or_else(|| invalid("packet of an undeclared interface"))?;
                let original = endian.u32(body, 0)? as usize;
                let data = &body[4..body.len().min(4 + original)];
                frames.push(Frame {
                    timestamp: Timestamp::from_unix_nanos(0),
                    link,
                    data,
                });
            }
            _ => {}
        }
        at += length;
    }
    Ok(frames)
}

/// The IP packet inside a link-layer frame.
fn ip_packet(link: u32, data: &[u8]) -> Option<&[u8]> {
    match link {
        // Ethernet, with optional 802.1Q tags.
        1 => {
            let mut at = 12;
            let mut ethertype = u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?);
            while ethertype == 0x8100 || ethertype == 0x88a8 {
                at += 4;
                ethertype = u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?);
            }
            matches!(ethertype, 0x0800 | 0x86dd)
                .then(|| data.get(at + 2..))
                .flatten()
        }
        // Linux cooked capture v1 and v2.
        113 => data.get(16..),
        276 => data.get(20..),
        // Raw IP.
        12 | 14 | 101 | 228 | 229 => Some(data),
        // BSD loopback (host order family) and OpenBSD loopback.
        0 | 108 => data.get(4..),
        _ => None,
    }
}

fn tcp(timestamp: Timestamp, packet: &[u8]) -> Option<Segment> {
    let version = packet.first()? >> 4;
    let (source, destination, segment) = match version {
        4 => {
            let header = usize::from(packet.first()? & 0x0f) * 4;
            let total = usize::from(u16::from_be_bytes(packet.get(2..4)?.try_into().ok()?));
            let flags = u16::from_be_bytes(packet.get(6..8)?.try_into().ok()?);
            // Fragments are not reassembled.
            if flags & 0x3fff != 0 || *packet.get(9)? != 6 {
                return None;
            }
            let source = Ipv4Addr::from(<[u8; 4]>::try_from(packet.get(12..16)?).ok()?);
            let destination = Ipv4Addr::from(<[u8; 4]>::try_from(packet.get(16..20)?).ok()?);
            (
                IpAddr::V4(source),
                IpAddr::V4(destination),
                packet.get(header..total.min(packet.len()))?,
            )
        }
        6 => {
            let payload = usize::from(u16::from_be_bytes(packet.get(4..6)?.try_into().ok()?));
            let mut next = *packet.get(6)?;
            let source = Ipv6Addr::from(<[u8; 16]>::try_from(packet.get(8..24)?).ok()?);
            let destination = Ipv6Addr::from(<[u8; 16]>::try_from(packet.get(24..40)?).ok()?);
            let end = (40 + payload).min(packet.len());
            let mut at = 40;
            while matches!(next, 0 | 43 | 60) {
                let length = (usize::from(*packet.get(at + 1)?) + 1) * 8;
                next = *packet.get(at)?;
                at += length;
            }
            if next != 6 {
                return None;
            }
            (
                IpAddr::V6(source),
                IpAddr::V6(destination),
                packet.get(at..end)?,
            )
        }
        _ => return None,
    };
    let source_port = u16::from_be_bytes(segment.get(0..2)?.try_into().ok()?);
    let destination_port = u16::from_be_bytes(segment.get(2..4)?.try_into().ok()?);
    let sequence = u32::from_be_bytes(segment.get(4..8)?.try_into().ok()?);
    let offset = usize::from(segment.get(12)? >> 4) * 4;
    let flags = *segment.get(13)?;
    Some(Segment {
        timestamp,
        source: SocketAddr::new(source, source_port),
        destination: SocketAddr::new(destination, destination_port),
        sequence,
        syn: flags & 0x02 != 0,
        fin: flags & 0x05 != 0,
        payload: segment.get(offset..)?.to_vec(),
    })
}

/// The TCP segments of a PCAP or PCAPNG file, in capture order. Other
/// traffic is skipped.
pub fn segments(bytes: &[u8]) -> Result<Vec<Segment>, ShadowError> {
    let frames = if bytes.starts_with(&[0x0a, 0x0d, 0x0d, 0x0a]) {
        next_generation(bytes)?
    } else {
        classic(bytes)?
    };
    Ok(frames
        .iter()
        .filter_map(|frame| tcp(frame.timestamp, ip_packet(frame.link, frame.data)?))
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An Ethernet/IPv4/TCP frame.
    pub(crate) fn frame(
        source: ([u8; 4], u16),
        destination: ([u8; 4], u16),
        sequence: u32,
        flags: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out = vec![0u8; 12];
        out.extend_from_slice(&[0x08, 0x00]);
        let total = u16::try_from(20 + 20 + payload.len()).unwrap();
        out.extend_from_slice(&[
            0x45,
            0,
            (total >> 8) as u8,
            total as u8,
            0,
            0,
            0x40,
            0,
            64,
            6,
            0,
            0,
        ]);
        out.extend_from_slice(&source.0);
        out.extend_from_slice(&destination.0);
        out.extend_from_slice(&source.1.to_be_bytes());
        out.extend_from_slice(&destination.1.to_be_bytes());
        out.extend_from_slice(&sequence.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0x50, flags, 0xff, 0xff, 0, 0, 0, 0]);
        out.extend_from_slice(payload);
        out
    }

    /// A classic PCAP file (microseconds, little endian, Ethernet).
    pub(crate) fn pcap(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0];
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&65535u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        for (i, frame) in frames.iter().enumerate() {
            out.extend_from_slice(&1_790_000_000u32.to_le_bytes());
            out.extend_from_slice(&(u32::try_from(i).unwrap() * 1000).to_le_bytes());
            let length = u32::try_from(frame.len()).unwrap();
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(frame);
        }
        out
    }

    /// A PCAPNG file with one Ethernet interface in microseconds.
    pub(crate) fn pcapng(frames: &[Vec<u8>]) -> Vec<u8> {
        let block = |kind: u32, body: Vec<u8>| {
            let mut body = body;
            while body.len() % 4 != 0 {
                body.push(0);
            }
            let length = u32::try_from(body.len() + 12).unwrap();
            let mut out = kind.to_le_bytes().to_vec();
            out.extend_from_slice(&length.to_le_bytes());
            out.extend(body);
            out.extend_from_slice(&length.to_le_bytes());
            out
        };
        let mut shb = vec![0x4d, 0x3c, 0x2b, 0x1a, 1, 0, 0, 0];
        shb.extend_from_slice(&u64::MAX.to_le_bytes());
        let mut out = block(0x0a0d_0d0a, shb);
        let mut idb = 1u16.to_le_bytes().to_vec();
        idb.extend_from_slice(&[0, 0]);
        idb.extend_from_slice(&65535u32.to_le_bytes());
        out.extend(block(1, idb));
        for (i, frame) in frames.iter().enumerate() {
            let ticks = 1_790_000_000u64 * 1_000_000 + u64::try_from(i).unwrap();
            let mut epb = 0u32.to_le_bytes().to_vec();
            epb.extend_from_slice(&u32::try_from(ticks >> 32).unwrap().to_le_bytes());
            epb.extend_from_slice(&u32::try_from(ticks & 0xffff_ffff).unwrap().to_le_bytes());
            let length = u32::try_from(frame.len()).unwrap();
            epb.extend_from_slice(&length.to_le_bytes());
            epb.extend_from_slice(&length.to_le_bytes());
            epb.extend_from_slice(frame);
            out.extend(block(6, epb));
        }
        out
    }

    #[test]
    fn reads_both_file_formats() {
        let frames = vec![
            frame(
                ([10, 0, 0, 5], 40000),
                ([10, 0, 0, 1], 6661),
                100,
                0x02,
                b"",
            ),
            frame(
                ([10, 0, 0, 5], 40000),
                ([10, 0, 0, 1], 6661),
                101,
                0x18,
                b"hello",
            ),
        ];
        for bytes in [pcap(&frames), pcapng(&frames)] {
            assert!(is_pcap(&bytes));
            let segments = segments(&bytes).unwrap();
            assert_eq!(segments.len(), 2);
            assert!(segments[0].syn);
            assert_eq!(segments[1].payload, b"hello");
            assert_eq!(segments[1].destination.port(), 6661);
            assert_eq!(segments[1].sequence, 101);
        }
        assert!(segments(b"\xd4\xc3\xb2\xa1").is_err());
        assert!(!is_pcap(b"{\"format\":1}"));
    }
}
