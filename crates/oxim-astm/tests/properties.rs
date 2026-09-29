//! Property tests: losslessness, panic freedom, framing and splitting under
//! arbitrary chunking.

use std::time::Instant;

use oxim_astm::frame::{Decoded, ENQ, EOT, decode_frame, split_message};
use oxim_astm::raw::{RawEvent, RawOptions, RawSplitter};
use oxim_astm::session::{Output, Session, SessionConfig};
use oxim_astm::{Delimiters, Encoding, Message, escape, unescape};
use proptest::prelude::*;

fn utf8() -> &'static Encoding {
    encoding_rs::UTF_8
}

/// Bytes biased towards the characters that matter to the parsers.
fn astm_bytes() -> impl Strategy<Value = Vec<u8>> {
    let interesting = prop_oneof![
        4 => prop::sample::select(b"|\\^&\r\nHPORCLQ0123456789 \x02\x03\x04\x05\x06\x15\x17".to_vec()),
        1 => any::<u8>(),
    ];
    prop::collection::vec(interesting, 0..400)
}

/// Valid messages: records made of printable text, terminated by CR.
fn messages() -> impl Strategy<Value = Vec<u8>> {
    let record = ("[POCRQMS]", "[ -~&&[^|\\\\^&]]{0,300}");
    prop::collection::vec(record, 0..6).prop_map(|records| {
        let mut message = b"H|\\^&|||SENDER\r".to_vec();
        for (kind, text) in records {
            message.extend_from_slice(kind.as_bytes());
            message.push(b'|');
            message.extend_from_slice(text.as_bytes());
            message.push(b'\r');
        }
        message.extend_from_slice(b"L|1|N\r");
        message
    })
}

fn apply_cuts(stream: &[u8], cuts: &[usize]) -> Vec<(usize, usize)> {
    let mut cuts: Vec<usize> = cuts.iter().map(|c| c % (stream.len() + 1)).collect();
    cuts.sort_unstable();
    let mut ranges = Vec::new();
    let mut start = 0;
    for cut in cuts.into_iter().chain([stream.len()]) {
        ranges.push((start, cut));
        start = cut;
    }
    ranges
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn escaping_is_reversible(text in prop::collection::vec(any::<u8>(), 0..200)) {
        let delimiters = Delimiters::default();
        let escaped = escape(&text, &delimiters);
        prop_assert!(!escaped.iter().any(|&b| b == b'|' || b == b'\r' || b == b'\n'));
        let restored = unescape(&escaped, &delimiters);
        prop_assert_eq!(restored.as_ref(), text.as_slice());
    }

    #[test]
    fn every_parsed_message_round_trips(suffix in astm_bytes()) {
        let mut input = b"H|\\^&|".to_vec();
        input.extend_from_slice(&suffix);
        if let Ok(message) = Message::parse(&input) {
            prop_assert_eq!(message.to_bytes(), input);
            let _ = message.patients();
            for record in message.records() {
                for n in 1..=record.field_count() + 1 {
                    if let Some(field) = record.field(n) {
                        for repetition in field.repetitions() {
                            for component in repetition.components() {
                                let _ = component.to_string_lossy();
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn parsing_arbitrary_bytes_never_panics(input in astm_bytes()) {
        let _ = Message::parse(&input);
    }

    #[test]
    fn set_then_get_returns_the_text(
        text in any::<String>(),
        record in prop::sample::select(vec!["P", "O", "R"]),
        field in 2usize..14,
        repetition in prop::option::of(1usize..4),
        component in prop::option::of(1usize..6),
    ) {
        let mut message = Message::parse(
            b"H|\\^&|||SENDER\rP|1||PID||Doe^Jane\rO|1|S1||^^^GLU\\^^^HGB\rR|1|^^^GLU|5.4\rL|1\r",
        )
        .unwrap();
        let mut path = format!("{record}-{field}");
        if let Some(repetition) = repetition {
            path.push_str(&format!("[{repetition}]"));
        }
        if let Some(component) = component {
            path.push_str(&format!(".{component}"));
        }
        message.set_text(&path, &text, utf8()).unwrap();
        prop_assert_eq!(message.get(&path).unwrap().to_string_lossy(), text.as_str());
        let reparsed = Message::parse(&message.to_bytes()).unwrap();
        prop_assert_eq!(reparsed, message);
    }

    #[test]
    fn framing_round_trips(message in messages(), first in 0u8..8, max_text in 1usize..300) {
        let frames = split_message(&message, first, max_text).unwrap();
        let mut stream = Vec::new();
        for frame in &frames {
            stream.extend(frame.encode());
        }
        let mut text = Vec::new();
        let mut rest = stream.as_slice();
        while !rest.is_empty() {
            match decode_frame(rest, max_text) {
                Decoded::Frame { frame, len } => {
                    text.extend_from_slice(frame.text());
                    rest = &rest[len..];
                }
                other => prop_assert!(false, "unexpected {:?}", other),
            }
        }
        prop_assert_eq!(text, message);
    }

    #[test]
    fn decode_frame_never_panics(input in astm_bytes(), max_text in 0usize..300) {
        let mut input = input;
        input.insert(0, 0x02);
        match decode_frame(&input, max_text) {
            Decoded::Incomplete => {}
            Decoded::Frame { len, .. } | Decoded::Invalid { len, .. } => {
                prop_assert!(len >= 1 && len <= input.len());
            }
        }
    }

    #[test]
    fn sessions_receive_under_any_chunking(message in messages(), cuts in prop::collection::vec(any::<usize>(), 0..30)) {
        let now = Instant::now();
        let mut stream = vec![ENQ];
        for frame in split_message(&message, 1, 240).unwrap() {
            stream.extend(frame.encode());
        }
        stream.push(EOT);
        let mut session = Session::new(SessionConfig::default());
        let mut received = Vec::new();
        for (start, end) in apply_cuts(&stream, &cuts) {
            session.handle_input(&stream[start..end], now);
            while let Some(output) = session.poll_output() {
                if let Output::Received(text) = output {
                    received.push(text);
                }
            }
        }
        prop_assert_eq!(received, vec![message]);
    }

    #[test]
    fn sessions_survive_arbitrary_input(input in astm_bytes(), cuts in prop::collection::vec(any::<usize>(), 0..20)) {
        let now = Instant::now();
        let mut session = Session::new(SessionConfig::default());
        for (start, end) in apply_cuts(&input, &cuts) {
            session.handle_input(&input[start..end], now);
            while session.poll_output().is_some() {}
        }
        session.handle_timeout(now + std::time::Duration::from_secs(60));
        while session.poll_output().is_some() {}
        prop_assert!(session.is_idle());
    }

    #[test]
    fn raw_splitting_survives_any_chunking(
        batch in prop::collection::vec(messages(), 1..5),
        cuts in prop::collection::vec(any::<usize>(), 0..30),
    ) {
        let stream: Vec<u8> = batch.concat();
        let mut splitter = RawSplitter::default();
        let mut found = Vec::new();
        for (start, end) in apply_cuts(&stream, &cuts) {
            splitter.push(&stream[start..end]);
            while let Some(event) = splitter.next_event() {
                if let RawEvent::Message(message) = event {
                    found.push(message);
                }
            }
        }
        prop_assert_eq!(splitter.finish(), None);
        prop_assert_eq!(found, batch);
    }

    #[test]
    fn raw_splitting_accounts_for_every_byte(
        input in astm_bytes(),
        cuts in prop::collection::vec(any::<usize>(), 0..20),
        max in 1usize..200,
    ) {
        let mut options = RawOptions::default();
        options.max_message_len = max;
        let mut splitter = RawSplitter::new(options);
        let mut accounted = 0;
        let count = |event: RawEvent| match event {
            RawEvent::Message(bytes) => bytes.len(),
            RawEvent::Discarded { bytes, .. } => bytes,
        };
        for (start, end) in apply_cuts(&input, &cuts) {
            splitter.push(&input[start..end]);
            while let Some(event) = splitter.next_event() {
                accounted += count(event);
            }
        }
        if let Some(event) = splitter.finish() {
            accounted += count(event);
        }
        prop_assert_eq!(accounted, input.len());
    }
}
