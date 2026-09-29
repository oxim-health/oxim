#![no_main]

//! Feeds arbitrary bytes to the MLLP decoder in arbitrary chunks and checks
//! that every byte is accounted for.

use libfuzzer_sys::fuzz_target;
use oxim_mllp::{Decoder, Event, encode};

fuzz_target!(|input: (Vec<u8>, Vec<u16>)| {
    let (stream, cuts) = input;
    let mut decoder = Decoder::default();
    let mut events = Vec::new();
    let mut cuts: Vec<usize> = cuts
        .into_iter()
        .map(|cut| usize::from(cut) % (stream.len() + 1))
        .collect();
    cuts.sort_unstable();
    let mut start = 0;
    for cut in cuts.into_iter().chain([stream.len()]) {
        decoder.push(&stream[start..cut]);
        start = cut;
        events.extend(std::iter::from_fn(|| decoder.next_event()));
    }
    events.extend(decoder.finish());

    let mut accounted = 0;
    let mut frames = 0;
    for event in &events {
        match event {
            Event::Frame(payload) => {
                accounted += payload.len() + 2;
                frames += 1;
                // Every decoded payload can be framed again.
                assert!(encode(payload).is_ok());
            }
            Event::CommitAck | Event::CommitNak => {
                accounted += 3;
                frames += 1;
            }
            Event::Discarded { bytes, .. } => accounted += bytes,
        }
    }
    assert!(accounted <= stream.len() && stream.len() <= accounted + frames);
});
