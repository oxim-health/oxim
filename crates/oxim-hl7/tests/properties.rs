//! Property tests: losslessness, panic freedom and edit consistency.

use oxim_hl7::{Delimiters, Message, escape, unescape};
use proptest::prelude::*;

/// Bytes biased towards the characters that matter to the parser.
fn message_bytes() -> impl Strategy<Value = Vec<u8>> {
    let interesting = prop_oneof![
        4 => prop::sample::select(b"|^~\\&#\r\nMSHPIDOBX0123456789 \"".to_vec()),
        1 => any::<u8>(),
    ];
    prop::collection::vec(interesting, 0..400)
}

fn delimiter_sets() -> impl Strategy<Value = Delimiters> {
    prop_oneof![
        Just(Delimiters::default()),
        Just(Delimiters {
            truncation: Some(b'#'),
            ..Delimiters::default()
        }),
        Just(Delimiters {
            field: b'#',
            component: b'*',
            repetition: b'!',
            escape: Some(b'$'),
            subcomponent: Some(b'%'),
            truncation: None,
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn escaping_is_reversible(text in prop::collection::vec(any::<u8>(), 0..200), delimiters in delimiter_sets()) {
        let escaped = escape(&text, &delimiters).unwrap();
        prop_assert!(!escaped.iter().any(|&b| b == delimiters.field || b == b'\r' || b == b'\n'));
        let restored = unescape(&escaped, &delimiters);
        prop_assert_eq!(restored.as_ref(), text.as_slice());
    }

    #[test]
    fn unescape_never_panics(raw in message_bytes(), delimiters in delimiter_sets()) {
        let _ = unescape(&raw, &delimiters);
    }

    #[test]
    fn parsing_arbitrary_bytes_never_panics(input in message_bytes()) {
        let _ = Message::parse(&input);
    }

    #[test]
    fn every_parsed_message_round_trips(suffix in message_bytes()) {
        let mut input = b"MSH|^~\\&|".to_vec();
        input.extend_from_slice(&suffix);
        if let Ok(message) = Message::parse(&input) {
            prop_assert_eq!(message.to_bytes(), input);
            for segment in message.segments() {
                for n in 1..=segment.field_count() + 1 {
                    if let Some(field) = segment.field(n) {
                        for repetition in field.repetitions() {
                            for component in repetition.components() {
                                let _ = component.subcomponents().count();
                                let _ = component.to_string_lossy();
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn set_then_get_returns_the_text(
        text in any::<String>(),
        field in 1usize..12,
        repetition in prop::option::of(1usize..4),
        component in prop::option::of(1usize..6),
        subcomponent in 1usize..4,
        use_subcomponent in any::<bool>(),
    ) {
        let mut message = Message::parse(
            b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|1|P|2.5.1\r\
PID|1||123^^^HOSP~456^^^OTHER||Doe^Jane&J^^^Dr\rOBX|1|NM|GLU||5.4\r",
        ).unwrap();
        let mut path = format!("PID-{field}");
        if let Some(repetition) = repetition {
            path.push_str(&format!("[{repetition}]"));
        }
        if let Some(component) = component {
            path.push_str(&format!(".{component}"));
            if use_subcomponent {
                path.push_str(&format!(".{subcomponent}"));
            }
        }
        let before_obx = message.get("OBX-5").unwrap().raw().to_vec();
        message.set(&path, &text).unwrap();
        prop_assert_eq!(message.get(&path).unwrap().to_string_lossy(), text.as_str());
        prop_assert_eq!(message.get("OBX-5").unwrap().raw(), before_obx.as_slice());
        let reparsed = Message::parse(&message.to_bytes()).unwrap();
        prop_assert_eq!(reparsed, message);
    }
}
