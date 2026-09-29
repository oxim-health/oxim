//! Property tests: losslessness, panic freedom and edit consistency.

use oxim_x12::{
    AckHeader, FunctionalAckKind, FunctionalAckOptions, Interchange, Ta1Options,
    build_functional_ack, build_ta1,
};
use proptest::prelude::*;

const ISA: &[u8] = b"ISA*00*          *00*          *ZZ*SENDER         *ZZ*RECEIVER       *260929*1200*^*00501*000000001*0*T*:~";

/// Bytes biased towards the characters that matter to the parser.
fn body_bytes() -> impl Strategy<Value = Vec<u8>> {
    let interesting = prop_oneof![
        4 => prop::sample::select(b"*:^~\r\n GSTEIANM1CLMHI0123456789".to_vec()),
        1 => any::<u8>(),
    ];
    prop::collection::vec(interesting, 0..400)
}

fn header() -> AckHeader<'static> {
    AckHeader {
        control_number: 42,
        date: "20260929",
        time: "1200",
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn parsing_arbitrary_bytes_never_panics(input in body_bytes()) {
        let _ = Interchange::parse(&input);
    }

    #[test]
    fn every_parsed_interchange_round_trips(body in body_bytes()) {
        let mut input = ISA.to_vec();
        input.extend_from_slice(&body);
        let interchange = Interchange::parse(&input).unwrap();
        prop_assert_eq!(interchange.to_bytes(), input);
        let _ = interchange.validate();
        let reparsed = Interchange::parse(&interchange.to_bytes()).unwrap();
        prop_assert_eq!(&reparsed, &interchange);
        for segment in interchange.segments() {
            for n in 0..=segment.element_count() {
                let Some(element) = segment.element(n) else { continue };
                for repetition in element.repetitions() {
                    for component in repetition.components() {
                        let _ = component.to_string_lossy();
                    }
                }
            }
        }
    }

    #[test]
    fn acknowledgments_of_any_interchange_are_valid(body in body_bytes()) {
        let mut input = ISA.to_vec();
        input.extend_from_slice(&body);
        let interchange = Interchange::parse(&input).unwrap();
        let ta1 = build_ta1(&interchange, &Ta1Options {
            header: header(),
            interchange: 0,
            code: None,
            note: None,
        });
        if let Ok(ta1) = ta1 {
            let reparsed = Interchange::parse(&ta1.to_bytes()).unwrap();
            prop_assert!(reparsed.validate().is_empty());
        }
        let ack = build_functional_ack(&interchange, &FunctionalAckOptions {
            kind: FunctionalAckKind::Ack999,
            header: header(),
            group_control_number: 1,
            interchange: 0,
            group: 0,
            transactions: Vec::new(),
        });
        if let Ok(ack) = ack {
            let reparsed = Interchange::parse(&ack.to_bytes()).unwrap();
            prop_assert!(reparsed.validate().is_empty(), "{:?}", reparsed.validate());
        }
    }

    #[test]
    fn written_values_read_back(
        occurrence in 1usize..3,
        element in 1usize..12,
        component in proptest::option::of(1usize..4),
        text in "[A-Z0-9 .]{0,12}",
    ) {
        let input = [ISA, b"GS*HC*A*B*20260929*1200*1*X*005010X222A1~ST*837*0001~NM1*IL*1*DOE~NM1*85*2~SE*4*0001~GE*1*1~IEA*1*000000001~"].concat();
        let mut interchange = Interchange::parse(&input).unwrap();
        let path = match component {
            Some(c) => format!("NM1[{occurrence}]-{element}.{c}"),
            None => format!("NM1[{occurrence}]-{element}"),
        };
        interchange.set(&path, &text).unwrap();
        let value = interchange.get(&path).map(|v| v.to_string_lossy().into_owned());
        prop_assert_eq!(value.as_deref(), Some(text.as_str()));
        let reparsed = Interchange::parse(&interchange.to_bytes()).unwrap();
        prop_assert_eq!(reparsed, interchange);
    }
}
