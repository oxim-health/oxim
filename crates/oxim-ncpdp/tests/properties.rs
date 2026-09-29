//! Property tests: losslessness, panic freedom and edit consistency.

use oxim_ncpdp::{
    HeaderStatus, ResponseOptions, TransactionResponse, TransactionStatus, Transmission,
    build_response,
};
use proptest::prelude::*;

const HEADER: &[u8] = b"999999D0B1PCN1234567101SYNTHPHARM01   20260929SYNTHVEND1";

/// Bytes biased towards separators and field identifiers.
fn body_bytes() -> impl Strategy<Value = Vec<u8>> {
    let interesting = prop_oneof![
        4 => prop::sample::select(b"\x1c\x1d\x1eAM0147D2EMC2 9".to_vec()),
        1 => any::<u8>(),
    ];
    prop::collection::vec(interesting, 0..300)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn parsing_arbitrary_bytes_never_panics(input in body_bytes()) {
        if let Ok(transmission) = Transmission::parse(&input) {
            prop_assert_eq!(transmission.to_bytes(), input);
        }
    }

    #[test]
    fn every_parsed_transmission_round_trips(body in body_bytes()) {
        let mut input = HEADER.to_vec();
        input.push(0x1e);
        input.extend_from_slice(&body);
        let transmission = Transmission::parse(&input).unwrap();
        prop_assert_eq!(transmission.to_bytes(), input);
        let _ = transmission.validate();
        for segment in transmission.segments() {
            for (id, value) in segment.fields() {
                let _ = (id, value);
            }
        }
        let responses = (0..transmission.transaction_count().min(9))
            .map(|_| TransactionResponse {
                status: TransactionStatus::Rejected,
                authorization_number: None,
                reject_codes: vec!["85".into()],
                message: None,
            })
            .collect();
        if let Ok(response) = build_response(&transmission, &ResponseOptions {
            status: HeaderStatus::Accepted,
            message: None,
            transactions: responses,
        }) {
            let reparsed = Transmission::parse(&response.to_bytes()).unwrap();
            prop_assert_eq!(&reparsed, &response);
            prop_assert!(reparsed.validate().is_empty(), "{:?}", reparsed.validate());
        }
    }

    #[test]
    fn written_values_read_back(field in "[A-Z][A-Z0-9]", text in "[A-Z0-9 {}]{0,20}") {
        prop_assume!(field != "AM");
        let mut input = HEADER.to_vec();
        input.extend_from_slice(b"\x1d\x1e\x1cAM07\x1cEM1\x1cD2000000123456");
        let mut transmission = Transmission::parse(&input).unwrap();
        let path = format!("AM07.{field}");
        transmission.set(&path, &text).unwrap();
        let value = transmission.get(&path);
        prop_assert_eq!(value.as_deref(), Some(text.as_str()));
        let reparsed = Transmission::parse(&transmission.to_bytes()).unwrap();
        prop_assert_eq!(reparsed, transmission);
    }
}
