//! Anonymizing `.oximcap` captures.
//!
//! Each direction of each connection is treated as one byte stream in the
//! capture's protocol (from the header, or guessed from the first byte):
//!
//! | Protocol | Unit | Handling |
//! |---|---|---|
//! | HL7 v2 over MLLP | MLLP frame | the message is anonymized and framed again |
//! | ASTM in LIS01 frames | the frames of one record or message | the records are anonymized and split over the same number of frames with the original frame numbers and terminators; checksums are recomputed |
//! | unframed ASTM | record | anonymized |
//! | POCT1-A | `V` attribute | rewritten in place |
//!
//! Link control characters (ENQ, ACK, NAK, EOT) and other bytes between
//! units are kept. A unit's new bytes are placed in the record where the
//! original unit ended, so the other side's answers stay after what they
//! answer and the capture still replays. Frames that cannot be decoded and
//! incomplete MLLP frames are dropped with a warning rather than kept with
//! patient data in them; streams of an unknown protocol are kept unchanged
//! with a warning.

use std::collections::{BTreeMap, HashMap};

use oxim_astm::frame::{Decoded, Frame, decode_frame};
use oxim_capture::{Capture, Direction, Event, Protocol};

use crate::astm::{self, Delimiters};
use crate::{Anonymizer, Report, hl7, poct1a};

const STX: u8 = 0x02;
const VT: u8 = 0x0B;
const FS: u8 = 0x1C;
const CR: u8 = 0x0D;

/// A span of the stream and what replaces it.
struct Unit {
    start: usize,
    end: usize,
    replacement: Vec<u8>,
}

fn guess(stream: &[u8]) -> Option<Protocol> {
    match stream.first()? {
        &VT => Some(Protocol::Hl7v2Mllp),
        0x02 | 0x04 | 0x05 | 0x06 | 0x15 => Some(Protocol::AstmLis01),
        b'H' => Some(Protocol::AstmRaw),
        b'<' => Some(Protocol::Poct1a),
        _ => None,
    }
}

fn mllp_units(
    anonymizer: &Anonymizer,
    stream: &[u8],
    place: &str,
    report: &mut Report,
) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut position = 0;
    while let Some(offset) = stream[position..].iter().position(|&b| b == VT) {
        let start = position + offset;
        let Some(length) = stream[start + 1..].iter().position(|&b| b == FS) else {
            report.warn(format!(
                "{place}: an incomplete MLLP frame at the end was dropped"
            ));
            units.push(Unit {
                start,
                end: stream.len(),
                replacement: Vec::new(),
            });
            return units;
        };
        let fs = start + 1 + length;
        let end = if stream.get(fs + 1) == Some(&CR) {
            fs + 2
        } else {
            fs + 1
        };
        let content = &stream[start + 1..fs];
        let replacement = match hl7::anonymize_message(anonymizer, content, report) {
            Ok(message) => {
                let mut framed = Vec::with_capacity(message.len() + 3);
                framed.push(VT);
                framed.extend_from_slice(&message);
                framed.extend_from_slice(&stream[fs..end]);
                framed
            }
            Err(error) => {
                report.warn(format!(
                    "{place}: an MLLP frame that is not valid HL7 was dropped ({error})"
                ));
                Vec::new()
            }
        };
        units.push(Unit {
            start,
            end,
            replacement,
        });
        position = end;
    }
    units
}

/// Splits `text` into `count` chunks (the first ones one byte longer when
/// it does not divide evenly).
fn split_evenly(text: &[u8], count: usize) -> Vec<&[u8]> {
    let count = count.max(1);
    let base = text.len() / count;
    let extra = text.len() % count;
    let mut chunks = Vec::with_capacity(count);
    let mut position = 0;
    for index in 0..count {
        let length = base + usize::from(index < extra);
        chunks.push(&text[position..position + length]);
        position += length;
    }
    chunks
}

fn lis01_units(
    anonymizer: &Anonymizer,
    stream: &[u8],
    place: &str,
    report: &mut Report,
) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut delimiters = Delimiters::default();
    let mut group: Vec<(usize, usize, Frame)> = Vec::new();
    let mut position = 0;
    while position < stream.len() {
        if stream[position] != STX {
            position += 1;
            continue;
        }
        match decode_frame(&stream[position..], 64 * 1024) {
            Decoded::Frame { frame, len } => {
                let last = frame.is_last();
                group.push((position, position + len, frame));
                position += len;
                if last {
                    finish_group(
                        anonymizer,
                        &mut group,
                        &mut delimiters,
                        &mut units,
                        place,
                        report,
                    );
                }
            }
            Decoded::Incomplete => {
                report.warn(format!(
                    "{place}: an incomplete LIS01 frame at the end was dropped"
                ));
                units.push(Unit {
                    start: position,
                    end: stream.len(),
                    replacement: Vec::new(),
                });
                position = stream.len();
            }
            Decoded::Invalid { error, len } => {
                report.warn(format!(
                    "{place}: an invalid LIS01 frame was dropped ({error})"
                ));
                units.push(Unit {
                    start: position,
                    end: position + len.max(1),
                    replacement: Vec::new(),
                });
                position += len.max(1);
            }
        }
    }
    finish_group(
        anonymizer,
        &mut group,
        &mut delimiters,
        &mut units,
        place,
        report,
    );
    units.sort_by_key(|unit| unit.start);
    units
}

/// Anonymizes the frames of one group and adds a unit per frame.
fn finish_group(
    anonymizer: &Anonymizer,
    group: &mut Vec<(usize, usize, Frame)>,
    delimiters: &mut Delimiters,
    units: &mut Vec<Unit>,
    place: &str,
    report: &mut Report,
) {
    if group.is_empty() {
        return;
    }
    let text: Vec<u8> = group
        .iter()
        .flat_map(|(_, _, frame)| frame.text().iter().copied())
        .collect();
    if text.first() == Some(&b'H') {
        report.messages += 1;
    }
    let new = astm::anonymize_records(anonymizer, &text, delimiters, report);
    let chunks = split_evenly(&new, group.len());
    for ((start, end, frame), chunk) in group.drain(..).zip(chunks) {
        if chunk.len() > 240 {
            report.warn(format!(
                "{place}: an anonymized frame is longer than 240 characters"
            ));
        }
        let replacement = match Frame::new(frame.number(), chunk.to_vec(), frame.is_last()) {
            Ok(frame) => frame.encode(),
            Err(error) => {
                report.warn(format!(
                    "{place}: a frame could not be rebuilt and was dropped ({error})"
                ));
                Vec::new()
            }
        };
        units.push(Unit {
            start,
            end,
            replacement,
        });
    }
}

fn raw_astm_units(anonymizer: &Anonymizer, stream: &[u8], report: &mut Report) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut delimiters = Delimiters::default();
    let mut position = 0;
    for record in stream.split_inclusive(|&b| b == CR || b == b'\n') {
        let start = position;
        position += record.len();
        if record.first() == Some(&b'H') {
            report.messages += 1;
        }
        let replacement = astm::anonymize_records(anonymizer, record, &mut delimiters, report);
        if replacement != record {
            units.push(Unit {
                start,
                end: position,
                replacement,
            });
        }
    }
    units
}

fn poct_units(anonymizer: &Anonymizer, stream: &[u8], report: &mut Report) -> Vec<Unit> {
    report.messages += stream.windows(5).filter(|w| *w == b"<?xml").count() as u64;
    poct1a::spans(anonymizer, stream, report)
        .into_iter()
        .map(|(start, end, replacement)| Unit {
            start,
            end,
            replacement,
        })
        .collect()
}

/// Anonymizes a capture. The result is marked as anonymized.
pub fn anonymize_capture(
    anonymizer: &Anonymizer,
    capture: &Capture,
    report: &mut Report,
) -> Capture {
    let records = &capture.records;
    let mut streams: BTreeMap<(u32, Direction), Vec<usize>> = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        if record.event == Event::Data
            && let Some(direction) = record.direction
        {
            streams
                .entry((record.connection, direction))
                .or_default()
                .push(index);
        }
    }
    let mut replaced: HashMap<usize, Vec<u8>> = HashMap::new();
    for ((connection, direction), indices) in streams {
        let mut stream = Vec::new();
        let mut owner = Vec::new();
        for &index in &indices {
            stream.extend_from_slice(&records[index].data);
            owner.extend(std::iter::repeat_n(index, records[index].data.len()));
        }
        let place = format!("connection {connection} {direction}");
        let protocol = match capture.header.protocol {
            Some(Protocol::Raw) | None => guess(&stream),
            known => known,
        };
        let mut units = match protocol {
            Some(Protocol::Hl7v2Mllp) => mllp_units(anonymizer, &stream, &place, report),
            Some(Protocol::AstmLis01) => lis01_units(anonymizer, &stream, &place, report),
            Some(Protocol::AstmRaw) => raw_astm_units(anonymizer, &stream, report),
            Some(Protocol::Poct1a) => poct_units(anonymizer, &stream, report),
            Some(Protocol::Raw) | None => {
                if !stream.is_empty() {
                    report.warn(format!(
                        "{place}: unknown protocol; the bytes were kept unchanged and may contain protected health information"
                    ));
                }
                Vec::new()
            }
        };
        units.sort_by_key(|unit| unit.start);
        let mut buffers: HashMap<usize, Vec<u8>> =
            indices.iter().map(|&index| (index, Vec::new())).collect();
        let mut position = 0;
        let keep = |from: usize, to: usize, buffers: &mut HashMap<usize, Vec<u8>>| {
            for offset in from..to {
                if let Some(buffer) = buffers.get_mut(&owner[offset]) {
                    buffer.push(stream[offset]);
                }
            }
        };
        for unit in &units {
            if unit.start < position {
                continue;
            }
            keep(position, unit.start, &mut buffers);
            if unit.end > unit.start
                && let Some(buffer) = buffers.get_mut(&owner[unit.end - 1])
            {
                buffer.extend_from_slice(&unit.replacement);
            }
            position = unit.end;
        }
        keep(position, stream.len(), &mut buffers);
        replaced.extend(buffers);
    }
    let mut out = capture.clone();
    out.header.anonymized = true;
    out.records = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| match replaced.remove(&index) {
            Some(data) if data.is_empty() && !record.data.is_empty() => None,
            Some(data) => {
                let mut record = record.clone();
                record.data = data;
                Some(record)
            }
            None => Some(record.clone()),
        })
        .collect();
    out
}
