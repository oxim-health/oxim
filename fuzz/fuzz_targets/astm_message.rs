#![no_main]

//! Parses arbitrary bytes as an ASTM E1394 message, checks losslessness and
//! walks every value and the record hierarchy; also splits the same bytes as
//! an unframed stream.

use libfuzzer_sys::fuzz_target;
use oxim_astm::Message;
use oxim_astm::raw::{RawEvent, RawSplitter};

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = Message::parse(data) {
        assert_eq!(message.to_bytes(), data, "parsing must be lossless");
        for record in message.records() {
            for n in 1..=record.field_count() + 1 {
                let Some(field) = record.field(n) else { continue };
                for repetition in field.repetitions() {
                    for component in repetition.components() {
                        let _ = component.to_string_lossy();
                    }
                }
            }
        }
        let _ = message.patients();
        let _ = message.is_terminated();
    }

    let mut splitter = RawSplitter::default();
    splitter.push(data);
    let mut accounted = 0;
    while let Some(event) = splitter.next_event() {
        accounted += match event {
            RawEvent::Message(bytes) => bytes.len(),
            RawEvent::Discarded { bytes, .. } => bytes,
        };
    }
    accounted += match splitter.finish() {
        Some(RawEvent::Message(bytes)) => bytes.len(),
        Some(RawEvent::Discarded { bytes, .. }) => bytes,
        None => 0,
    };
    assert_eq!(accounted, data.len());
});
