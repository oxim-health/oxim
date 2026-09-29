#![no_main]

//! Parses arbitrary bytes as JSON, checks losslessness and applies an edit.

use libfuzzer_sys::fuzz_target;
use oxim_formats::{JsonDocument, JsonOptions};

fuzz_target!(|data: &[u8]| {
    let Ok(mut doc) = JsonDocument::parse(data, &JsonOptions::default()) else {
        return;
    };
    assert_eq!(doc.to_bytes(), data, "unmodified documents must be reproduced");
    let _ = doc.get_text("");
    let _ = doc.get_text("results[0].value");
    if doc.set_text("/oxim/fuzz/-", "value").is_ok() {
        let reparsed = JsonDocument::parse(&doc.to_bytes(), &JsonOptions::default())
            .expect("edited documents must parse");
        assert_eq!(reparsed.value(), doc.value());
    }
});
