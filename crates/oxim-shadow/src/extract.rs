//! Messages inside captured byte streams: MLLP frames, ASTM LIS01
//! transmissions, or ASTM records without framing.

use oxim_astm::frame::{Decoded, decode_frame};
use oxim_mllp::{Decoder, Event};
use oxim_model::Timestamp;
use serde::{Deserialize, Serialize};

use crate::flows::Chunk;

/// How messages are framed on a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Framing {
    /// HL7 v2 over MLLP.
    #[default]
    Mllp,
    /// ASTM E1394 records in CLSI LIS01 (E1381) frames.
    Astm,
    /// ASTM E1394 records without LIS01 framing, one message per `L`
    /// record.
    AstmRaw,
}

/// One message and when its last byte was captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// When the message was complete.
    pub timestamp: Timestamp,
    /// The message, without framing.
    pub data: Vec<u8>,
}

const ENQ: u8 = 0x05;
const EOT: u8 = 0x04;
const STX: u8 = 0x02;

/// Splits ASTM records at each `L` (terminator) record.
struct AstmRecords {
    current: Vec<u8>,
}

impl AstmRecords {
    fn push_record(&mut self, text: &[u8], timestamp: Timestamp, out: &mut Vec<Captured>) {
        self.current.extend_from_slice(text);
        if text.first() == Some(&b'L') || text.windows(2).any(|w| w == b"\rL") {
            self.flush(timestamp, out);
        }
    }

    fn flush(&mut self, timestamp: Timestamp, out: &mut Vec<Captured>) {
        if !self.current.is_empty() {
            out.push(Captured {
                timestamp,
                data: std::mem::take(&mut self.current),
            });
        }
    }
}

/// The messages in one direction of a connection.
pub fn messages(chunks: &[Chunk], framing: Framing) -> Vec<Captured> {
    let mut out = Vec::new();
    match framing {
        Framing::Mllp => {
            let mut decoder = Decoder::default();
            for chunk in chunks {
                decoder.push(&chunk.data);
                while let Some(event) = decoder.next_event() {
                    if let Event::Frame(data) = event {
                        out.push(Captured {
                            timestamp: chunk.timestamp,
                            data,
                        });
                    }
                }
            }
        }
        Framing::Astm => {
            let mut buffer: Vec<u8> = Vec::new();
            let mut records = AstmRecords {
                current: Vec::new(),
            };
            let mut record = Vec::new();
            let mut last = Timestamp::from_unix_nanos(0);
            for chunk in chunks {
                last = chunk.timestamp;
                buffer.extend_from_slice(&chunk.data);
                while let Some(&first) = buffer.first() {
                    if first != STX {
                        if first == EOT || first == ENQ {
                            // A transmission ends or starts: flush what is
                            // pending.
                            if !record.is_empty() {
                                records.push_record(
                                    &std::mem::take(&mut record),
                                    chunk.timestamp,
                                    &mut out,
                                );
                            }
                            records.flush(chunk.timestamp, &mut out);
                        }
                        buffer.remove(0);
                        continue;
                    }
                    match decode_frame(&buffer, 64 * 1024) {
                        Decoded::Incomplete => break,
                        Decoded::Frame { frame, len } => {
                            buffer.drain(..len);
                            record.extend_from_slice(frame.text());
                            if frame.is_last() {
                                records.push_record(
                                    &std::mem::take(&mut record),
                                    chunk.timestamp,
                                    &mut out,
                                );
                            }
                        }
                        Decoded::Invalid { len, .. } => {
                            buffer.drain(..len.max(1));
                        }
                    }
                }
            }
            if !record.is_empty() {
                records.push_record(&record, last, &mut out);
            }
            records.flush(last, &mut out);
        }
        Framing::AstmRaw => {
            let mut records = AstmRecords {
                current: Vec::new(),
            };
            let mut line = Vec::new();
            let mut last = Timestamp::from_unix_nanos(0);
            for chunk in chunks {
                last = chunk.timestamp;
                for &byte in &chunk.data {
                    line.push(byte);
                    if byte == b'\r' {
                        records.push_record(&std::mem::take(&mut line), chunk.timestamp, &mut out);
                    }
                }
            }
            if !line.is_empty() {
                records.push_record(&line, last, &mut out);
            }
            records.flush(last, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use oxim_astm::frame::Frame;

    use super::*;

    fn chunk(data: &[u8]) -> Chunk {
        Chunk {
            timestamp: Timestamp::from_unix_nanos(1),
            data: data.to_vec(),
        }
    }

    #[test]
    fn splits_mllp_frames_across_chunks() {
        let mut stream = oxim_mllp::encode(b"MSH|^~\\&|A\r").unwrap();
        stream.extend(oxim_mllp::encode(b"MSH|^~\\&|B\r").unwrap());
        let (a, b) = stream.split_at(7);
        let found = messages(&[chunk(a), chunk(b)], Framing::Mllp);
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].data, b"MSH|^~\\&|B\r");
    }

    #[test]
    fn joins_astm_frames_into_messages() {
        let mut stream = vec![ENQ];
        stream.extend(Frame::new(1, b"H|\\^&\r".to_vec(), true).unwrap().encode());
        stream.push(0x06);
        stream.extend(
            Frame::new(2, b"P|1\rO|1|S1||^^^GLU\rR|1|^^^GL".to_vec(), false)
                .unwrap()
                .encode(),
        );
        stream.extend(Frame::new(3, b"U|5.4\r".to_vec(), true).unwrap().encode());
        stream.extend(Frame::new(4, b"L|1|N\r".to_vec(), true).unwrap().encode());
        stream.push(EOT);
        let found = messages(&[chunk(&stream)], Framing::Astm);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].data,
            b"H|\\^&\rP|1\rO|1|S1||^^^GLU\rR|1|^^^GLU|5.4\rL|1|N\r"
        );
        let raw = messages(
            &[chunk(b"H|\\^&\rL|1|N\rH|\\^&\rQ|1\rL|1|N\r")],
            Framing::AstmRaw,
        );
        assert_eq!(raw.len(), 2);
    }
}
