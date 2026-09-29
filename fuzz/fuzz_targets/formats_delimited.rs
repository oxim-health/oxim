#![no_main]

//! Parses arbitrary bytes as delimited text with several dialects, checks
//! losslessness and reads every field.

use libfuzzer_sys::fuzz_target;
use oxim_formats::{DelimitedDocument, DelimitedError, DelimitedOptions};

fuzz_target!(|data: &[u8]| {
    let Some((&selector, input)) = data.split_first() else {
        return;
    };
    let options = match selector % 4 {
        0 => DelimitedOptions::csv(),
        1 => DelimitedOptions::tsv(),
        2 => DelimitedOptions::csv().with_escape(Some(b'\\')),
        _ => DelimitedOptions::csv().with_delimiter(b'|').with_quote(None).with_header(true),
    };
    let doc = match DelimitedDocument::parse(input, &options) {
        Ok(doc) => doc,
        Err(DelimitedError::LimitExceeded(_)) => return,
        Err(error) => panic!("any input within limits must parse: {error}"),
    };
    assert_eq!(doc.to_bytes(), input, "input must be reproduced");
    let _ = doc.headers();
    for row in 0..doc.row_count() {
        for column in 0..doc.field_count(row).unwrap_or(0) {
            assert!(doc.get(row, column).is_some());
        }
    }
});
