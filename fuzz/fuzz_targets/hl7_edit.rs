#![no_main]

//! Applies arbitrary edits to a message and checks that every edit can be
//! read back and survives serialization.

use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use oxim_hl7::Message;

const TEMPLATE: &[u8] = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260101000000||ORU^R01^ORU_R01|1|P|2.5.1\r\
PID|1||123^^^HOSP~456^^^OTHER||Doe^Jane&J^^^Dr\r\
OBX|1|NM|GLU^Glucose||5.4|mmol/L\r";

#[derive(Debug, Arbitrary)]
struct Edit {
    segment: u8,
    field: u8,
    repetition: Option<u8>,
    component: Option<u8>,
    subcomponent: Option<u8>,
    text: String,
}

fuzz_target!(|edits: Vec<Edit>| {
    let mut message = Message::parse(TEMPLATE).unwrap();
    for edit in edits.iter().take(16) {
        let segment = ["PID", "OBX", "MSH"][usize::from(edit.segment % 3)];
        // MSH-18 changes the character set, which changes how text is
        // encoded, so MSH edits stay below it.
        let field = match segment {
            "MSH" => 3 + u16::from(edit.field % 12),
            _ => 1 + u16::from(edit.field % 30),
        };
        let mut path = format!("{segment}-{field}");
        if let Some(repetition) = edit.repetition {
            path.push_str(&format!("[{}]", 1 + u16::from(repetition % 8)));
        }
        if let Some(component) = edit.component {
            path.push_str(&format!(".{}", 1 + u16::from(component % 12)));
            if let Some(subcomponent) = edit.subcomponent {
                path.push_str(&format!(".{}", 1 + u16::from(subcomponent % 6)));
            }
        }
        message.set(&path, &edit.text).unwrap();
        let stored = message.get(&path).unwrap();
        assert_eq!(stored.to_string_lossy(), edit.text.as_str());
    }
    let reparsed = Message::parse(&message.to_bytes()).unwrap();
    assert_eq!(reparsed, message);
});
