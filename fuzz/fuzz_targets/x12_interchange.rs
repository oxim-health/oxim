#![no_main]

//! Parses arbitrary bytes as an X12 interchange, walks every value and
//! builds acknowledgments.

use libfuzzer_sys::fuzz_target;
use oxim_x12::{
    AckHeader, FunctionalAckKind, FunctionalAckOptions, Interchange, Ta1Options,
    build_functional_ack, build_ta1,
};

fuzz_target!(|data: &[u8]| {
    let Ok(interchange) = Interchange::parse(data) else {
        return;
    };
    assert_eq!(interchange.to_bytes(), data, "parsing must be lossless");

    for segment in interchange.segments() {
        for n in 0..=segment.element_count() {
            let Some(element) = segment.element(n) else {
                continue;
            };
            for repetition in element.repetitions() {
                for component in repetition.components() {
                    let _ = component.to_string_lossy();
                }
            }
        }
    }
    let _ = interchange.envelope();
    let _ = interchange.validate();

    let header = AckHeader {
        control_number: 1,
        date: "20260101",
        time: "0000",
    };
    if let Ok(ta1) = build_ta1(
        &interchange,
        &Ta1Options {
            header,
            interchange: 0,
            code: None,
            note: None,
        },
    ) {
        let reparsed = Interchange::parse(&ta1.to_bytes()).expect("TA1 must parse");
        assert!(reparsed.validate().is_empty(), "TA1 must be valid");
    }
    for kind in [FunctionalAckKind::Ack997, FunctionalAckKind::Ack999] {
        if let Ok(ack) = build_functional_ack(
            &interchange,
            &FunctionalAckOptions {
                kind,
                header,
                group_control_number: 1,
                interchange: 0,
                group: 0,
                transactions: Vec::new(),
            },
        ) {
            let reparsed = Interchange::parse(&ack.to_bytes()).expect("acknowledgment must parse");
            assert!(
                reparsed.validate().is_empty(),
                "acknowledgment must be valid"
            );
        }
    }
});
