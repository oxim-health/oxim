#![no_main]

//! Parses arbitrary bytes as an HL7 v2 message and walks every value.

use libfuzzer_sys::fuzz_target;
use oxim_hl7::{Message, build_ack, requested_mode, AckCode, AckOptions};

fuzz_target!(|data: &[u8]| {
    let Ok(message) = Message::parse(data) else {
        return;
    };
    assert_eq!(message.to_bytes(), data, "parsing must be lossless");

    let encoding = message
        .declared_encoding()
        .ok()
        .flatten()
        .unwrap_or(oxim_hl7::Encoding::for_label(b"utf-8").unwrap());
    for segment in message.segments() {
        for n in 1..=segment.field_count() + 1 {
            let Some(field) = segment.field(n) else { continue };
            for repetition in field.repetitions() {
                for component in repetition.components() {
                    for subcomponent in component.subcomponents() {
                        let _ = subcomponent.to_text(encoding);
                    }
                }
            }
        }
    }

    let _ = message.message_type();
    let _ = message.version();
    let _ = requested_mode(&message);
    let ack = build_ack(
        &message,
        &AckOptions {
            code: AckCode::ApplicationError,
            control_id: "FUZZ",
            timestamp: "20260101000000",
            text: Some("fuzz"),
            error: None,
        },
    );
    if let Ok(ack) = ack {
        assert!(Message::parse(&ack.to_bytes()).is_ok(), "acknowledgments must parse");
    }
});
