//! Property tests: framing survives arbitrary chunking and never panics.

use oxim_mllp::{Decoder, Event, encode};
use proptest::prelude::*;

fn payload() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        any::<u8>().prop_filter("no block bytes", |b| *b != 0x0B && *b != 0x1C),
        2..300,
    )
}

fn decode_in_chunks(stream: &[u8], cuts: &[usize]) -> Vec<Event> {
    let mut decoder = Decoder::default();
    let mut events = Vec::new();
    let mut start = 0;
    let mut cuts: Vec<usize> = cuts.iter().map(|c| c % (stream.len() + 1)).collect();
    cuts.sort_unstable();
    for cut in cuts.into_iter().chain([stream.len()]) {
        decoder.push(&stream[start..cut]);
        start = cut;
        events.extend(std::iter::from_fn(|| decoder.next_event()));
    }
    events.extend(decoder.finish());
    events
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn frames_survive_any_chunking(
        payloads in prop::collection::vec(payload(), 1..8),
        cuts in prop::collection::vec(any::<usize>(), 0..20),
    ) {
        let mut stream = Vec::new();
        for payload in &payloads {
            stream.extend(encode(payload).unwrap());
        }
        let expected: Vec<Event> = payloads.into_iter().map(Event::Frame).collect();
        prop_assert_eq!(decode_in_chunks(&stream, &cuts), expected);
    }

    #[test]
    fn arbitrary_bytes_never_panic_and_are_accounted_for(
        stream in prop::collection::vec(prop_oneof![
            3 => prop::sample::select(vec![0x0B, 0x1C, 0x0D, 0x06, 0x15, b'M']),
            1 => any::<u8>(),
        ], 0..500),
        cuts in prop::collection::vec(any::<usize>(), 0..20),
    ) {
        let events = decode_in_chunks(&stream, &cuts);
        let accounted: usize = events
            .iter()
            .map(|event| match event {
                Event::Frame(payload) => payload.len() + 2,
                Event::CommitAck | Event::CommitNak => 3,
                Event::Discarded { bytes, .. } => *bytes,
            })
            .sum();
        let trailers = events
            .iter()
            .filter(|event| !matches!(event, Event::Discarded { .. }))
            .count();
        // Every byte is either framing, payload or discarded. Frames end with
        // one or two trailer bytes, so allow for the optional carriage return.
        prop_assert!(accounted <= stream.len() && stream.len() <= accounted + trailers);
    }
}
