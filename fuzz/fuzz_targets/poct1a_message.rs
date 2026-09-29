#![no_main]

//! Parses arbitrary bytes as a POCT1-A message, reads every typed view and
//! checks that the parsed tree serializes to an equivalent message.

use libfuzzer_sys::fuzz_target;
use oxim_poct1a::{Message, ParseOptions};

fuzz_target!(|data: &[u8]| {
    let Ok(message) = Message::parse(data) else {
        return;
    };
    assert_eq!(message.as_bytes(), data, "parsing keeps the original bytes");

    let _ = (
        message.message_type(),
        message.topic(),
        message.trigger(),
        message.kind(),
    );
    let _ = (
        message.control_id(),
        message.version_id(),
        message.creation_dttm(),
    );
    let _ = (
        message.device_info(),
        message.device_status(),
        message.ack_info(),
    );
    let _ = message.end_of_topic();
    for observation in message.observations() {
        let _ = (
            observation.kind(),
            observation.observation_id(),
            observation.value(),
            observation.unit(),
            observation.observed_at(),
            observation.patient_id(),
            observation.operator_id(),
            observation.specimen(),
        );
    }

    // Everything the parser accepts can be written back and parses to the
    // same tree. Escaping can make the written message longer than the
    // input, so the length limit is lifted for the second parse.
    let rebuilt =
        Message::from_element(message.root().clone()).expect("a parsed tree is always valid");
    let mut options = ParseOptions::default();
    options.max_len = usize::MAX;
    let reparsed =
        Message::parse_with(rebuilt.as_bytes(), &options).expect("a written message always parses");
    assert_eq!(reparsed.root(), message.root());
});
