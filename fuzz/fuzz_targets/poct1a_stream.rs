#![no_main]

//! Splits arbitrary bytes, delivered in arbitrary chunks, into POCT1-A
//! documents and checks that no byte is invented and every document parses
//! or is rejected without panicking.

use libfuzzer_sys::fuzz_target;
use oxim_poct1a::{Message, SplitEvent, Splitter, SplitterOptions};

fuzz_target!(|input: (Vec<u8>, Vec<u16>, u16)| {
    let (stream, cuts, limit) = input;
    let mut options = SplitterOptions::default();
    options.max_document_len = 16 + usize::from(limit % 4096);
    let mut splitter = Splitter::new(options);
    let mut cuts: Vec<usize> = cuts
        .into_iter()
        .map(|cut| usize::from(cut) % (stream.len() + 1))
        .collect();
    cuts.sort_unstable();

    let mut events = Vec::new();
    let mut start = 0;
    for cut in cuts.into_iter().chain([stream.len()]) {
        splitter.push(&stream[start..cut]);
        start = cut;
        events.extend(std::iter::from_fn(|| splitter.next_event()));
    }
    events.extend(splitter.finish());

    let mut accounted = 0;
    for event in &events {
        match event {
            SplitEvent::Document(bytes) => {
                assert!(bytes.len() <= options.max_document_len);
                accounted += bytes.len();
                let _ = Message::parse(bytes);
            }
            SplitEvent::Discarded { bytes, .. } => accounted += bytes,
        }
    }
    assert!(accounted <= stream.len());
});
