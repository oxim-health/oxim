//! TCP stream reassembly: segments become the byte streams of each
//! connection, in order, without retransmitted duplicates.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;

use oxim_model::Timestamp;

use crate::pcap::Segment;

/// Bytes that arrived in one segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// When the segment was captured.
    pub timestamp: Timestamp,
    /// Its new bytes, in stream order.
    pub data: Vec<u8>,
}

/// One direction of one TCP connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// The sender.
    pub source: SocketAddr,
    /// The receiver.
    pub destination: SocketAddr,
    /// The bytes, in stream order.
    pub chunks: Vec<Chunk>,
}

impl Stream {
    /// All bytes of the stream.
    pub fn bytes(&self) -> Vec<u8> {
        self.chunks
            .iter()
            .flat_map(|c| c.data.iter().copied())
            .collect()
    }
}

#[derive(Default)]
struct Assembly {
    /// The sequence number of the next expected byte.
    next: Option<u32>,
    /// Segments that arrived ahead of `next`.
    pending: BTreeMap<u64, (Timestamp, Vec<u8>)>,
    /// Bytes accepted so far, used to order pending segments.
    base: u32,
    chunks: Vec<Chunk>,
}

impl Assembly {
    fn offset(&self, sequence: u32) -> u64 {
        u64::from(sequence.wrapping_sub(self.base))
    }

    fn push(&mut self, segment: &Segment) {
        if segment.syn {
            self.next = Some(segment.sequence.wrapping_add(1));
            self.base = segment.sequence.wrapping_add(1);
            return;
        }
        if segment.payload.is_empty() {
            return;
        }
        let next = *self.next.get_or_insert_with(|| {
            self.base = segment.sequence;
            segment.sequence
        });
        let start = self.offset(segment.sequence);
        let expected = self.offset(next);
        // Sequence numbers far "ahead" by more than 2 GiB are old segments
        // wrapped around: treat them as retransmissions.
        if start > u64::from(u32::MAX / 2) {
            return;
        }
        self.pending
            .entry(start)
            .or_insert_with(|| (segment.timestamp, segment.payload.clone()));
        let mut position = expected;
        while let Some((&start, _)) = self.pending.iter().next() {
            if start > position {
                break;
            }
            let Some((timestamp, data)) = self.pending.remove(&start) else {
                break;
            };
            let end = start + data.len() as u64;
            if end > position {
                let skip = usize::try_from(position - start).unwrap_or(usize::MAX);
                self.chunks.push(Chunk {
                    timestamp,
                    data: data[skip..].to_vec(),
                });
                position = end;
            }
        }
        let advanced = u32::try_from(position - expected).unwrap_or(u32::MAX);
        self.next = Some(next.wrapping_add(advanced));
    }
}

/// Reassembles the streams of all connections in `segments`, in order of
/// their first segment.
pub fn reassemble(segments: &[Segment]) -> Vec<Stream> {
    let mut order: Vec<(SocketAddr, SocketAddr)> = Vec::new();
    let mut assemblies: HashMap<(SocketAddr, SocketAddr), Assembly> = HashMap::new();
    for segment in segments {
        let key = (segment.source, segment.destination);
        let assembly = assemblies.entry(key).or_insert_with(|| {
            order.push(key);
            Assembly::default()
        });
        assembly.push(segment);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let assembly = assemblies.remove(&key)?;
            (!assembly.chunks.is_empty()).then_some(Stream {
                source: key.0,
                destination: key.1,
                chunks: assembly.chunks,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(sequence: u32, payload: &[u8], syn: bool) -> Segment {
        Segment {
            timestamp: Timestamp::from_unix_nanos(i64::from(sequence)),
            source: "10.0.0.5:40000".parse().unwrap(),
            destination: "10.0.0.1:6661".parse().unwrap(),
            sequence,
            syn,
            fin: false,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn orders_segments_and_drops_retransmissions() {
        let segments = vec![
            segment(99, b"", true),
            segment(100, b"abc", false),
            segment(106, b"ghi", false), // out of order
            segment(103, b"def", false),
            segment(100, b"abc", false),  // retransmission
            segment(104, b"efgh", false), // overlaps
            segment(109, b"jk", false),
        ];
        let streams = reassemble(&segments);
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].bytes(), b"abcdefghijk");
    }

    #[test]
    fn handles_captures_that_start_mid_stream() {
        let segments = vec![segment(5000, b"xy", false), segment(5002, b"z", false)];
        assert_eq!(reassemble(&segments)[0].bytes(), b"xyz");
    }
}
