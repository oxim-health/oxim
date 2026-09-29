#![no_main]

//! Parses arbitrary bytes as an NCPDP transmission, walks every field and
//! builds a response.

use libfuzzer_sys::fuzz_target;
use oxim_ncpdp::{
    HeaderStatus, ResponseOptions, TransactionResponse, TransactionStatus, Transmission,
    build_response,
};

fuzz_target!(|data: &[u8]| {
    let Ok(transmission) = Transmission::parse(data) else {
        return;
    };
    assert_eq!(transmission.to_bytes(), data, "parsing must be lossless");
    for segment in transmission.segments() {
        let _ = segment.id();
        for (id, value) in segment.fields() {
            let _ = (id.len(), value.len());
        }
    }
    let _ = transmission.validate();
    let transactions = (0..transmission.transaction_count().min(9))
        .map(|_| TransactionResponse {
            status: TransactionStatus::Paid,
            authorization_number: Some("FUZZ".into()),
            reject_codes: Vec::new(),
            message: None,
        })
        .collect();
    if let Ok(response) = build_response(
        &transmission,
        &ResponseOptions {
            status: HeaderStatus::Accepted,
            message: Some("FUZZ".into()),
            transactions,
        },
    ) {
        let reparsed = Transmission::parse(&response.to_bytes()).expect("responses must parse");
        assert!(reparsed.validate().is_empty(), "responses must be valid");
    }
});
