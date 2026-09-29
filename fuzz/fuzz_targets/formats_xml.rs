#![no_main]

//! Parses arbitrary bytes as XML, checks losslessness and applies edits.

use libfuzzer_sys::fuzz_target;
use oxim_formats::{XmlDocument, XmlOptions};

fuzz_target!(|data: &[u8]| {
    let Ok(mut doc) = XmlDocument::parse(data, &XmlOptions::default()) else {
        return;
    };
    assert_eq!(
        doc.to_bytes(),
        data,
        "unmodified documents must be reproduced"
    );
    let root = doc.root_name();
    let _ = doc.get(&format!("/{root}"));
    let _ = doc.get(&format!("/{root}/*[2]/@id"));
    let child = format!("/{root}/oxim-fuzz");
    let edited = doc.set(&format!("{child}[1]"), "a < b & 'c'").is_ok()
        && doc.set(&format!("{child}[1]/@note"), "\"q\"\n").is_ok();
    if edited {
        let reparsed = XmlDocument::parse(&doc.to_bytes(), &XmlOptions::default())
            .expect("edited documents must parse");
        assert_eq!(
            reparsed
                .get(&format!("{child}[1]"))
                .ok()
                .flatten()
                .as_deref(),
            Some("a < b & 'c'")
        );
        assert_eq!(
            reparsed
                .get(&format!("{child}[1]/@note"))
                .ok()
                .flatten()
                .as_deref(),
            Some("\"q\"\n")
        );
    }
});
