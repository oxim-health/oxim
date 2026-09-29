# oxim-hl7

Lossless HL7 v2 message parsing, editing and serialization for Rust, part of [OXIM](../../README.md).

- **Lossless:** serializing an unmodified message reproduces the input byte for byte, including custom delimiters, Z-segments, trailing empty fields, escape sequences, non-UTF-8 bytes and CR, LF or CRLF segment endings. Edits rewrite only the values they touch.
- **HL7 paths:** read and write values with `PID-5.1`, `OBX[2]-5`, `PID-3[2].4` or the dotted form `PID.5.1`. Missing fields, repetitions and components are created on write.
- **Character sets:** text is decoded and encoded with the character set declared in MSH-18 (HL7 table 0211 names and common labels such as `UTF-8` or `windows-1254`). Character sets whose multi-byte sequences could contain delimiter bytes are rejected instead of being split incorrectly.
- **Escape sequences:** delimiter and hexadecimal escapes are resolved; formatting and vendor sequences are preserved verbatim.
- **Acknowledgments:** build `ACK` messages in original or enhanced mode, with v2.5 or legacy ERR segments, and read the acknowledgment mode a sender requested.
- **Safe for untrusted input:** no `unsafe`, no panics on malformed input, bounded by configurable limits, covered by property tests and fuzzing.
- **Sans-IO:** no sockets, files or clocks. Use `oxim-mllp` for MLLP framing.

## Example

```rust
use oxim_hl7::{AckCode, AckOptions, Message, build_ack};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01^ORU_R01|MSG0001|P|2.5.1\r\
PID|1||12345^^^HOSP^MR||Doe^Jane\r\
OBX|1|NM|GLU^Glucose^L||5.4|mmol/L|3.9-6.1|N|||F\r";

    let mut message = Message::parse(input)?;
    assert_eq!(message.to_bytes(), input);
    assert_eq!(message.get("PID-5.1").unwrap(), "Doe");

    message.set("PID-5.2", "Janet")?;

    let ack = build_ack(&message, &AckOptions {
        code: AckCode::ApplicationAccept,
        control_id: "ACK0001",
        timestamp: "20260929120001",
        text: None,
        error: None,
    })?;
    assert_eq!(ack.get("MSA-2").unwrap(), "MSG0001");
    Ok(())
}
```

## Scope

This crate handles the ER7 encoding and message structure at segment and field level. Validation against HL7 message structure definitions and conformance profiles, batch files and HL7 v2 XML encoding are provided by other OXIM components.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
