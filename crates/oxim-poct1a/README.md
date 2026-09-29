# oxim-poct1a

Sans-IO POCT1-A (CLSI POCT01) device messaging for Rust, part of [OXIM](../../README.md).

POCT1-A connects point-of-care devices, such as blood gas analyzers and glucose meters, to a data manager. Each message is an XML document whose root element names the message type (`HEL.R01`, `DST.R01`, `OBS.R01`, `ACK.R01`, ...), with most values carried in a `V` attribute.

- **Stream splitting:** `Splitter` finds document boundaries in a TCP byte stream incrementally, tracking quotes, comments, CDATA sections and processing instructions. Junk, oversized, too deep, malformed and forbidden documents are reported with exact byte counts, and splitting continues.
- **Lossless messages:** `Message` keeps the original bytes and builds an element tree with attributes, text and document order. Only UTF-8 and ASCII-compatible declared encodings are accepted.
- **Secure parsing:** document type declarations are rejected, so no entity can be defined; only the five predefined entities and character references are resolved. Size, depth, element, attribute and text limits bound memory use.
- **Typed views:** header fields, device identification (`HEL`), device status (`DST`), acknowledgments (`ACK`), end of topic (`EOT`) and observations with analyte, value, unit, method, device interpretation, time, patient, operator, specimen and quality control material.
- **Builders:** acknowledgments, requests, end of topic, termination, directives and keep-alives.
- **Host conversation:** `HostConversation` runs the data manager side (hello, status, observation requests, acknowledgments, end of topic, termination, keep-alive and timeouts) as a sans-IO state machine. Observations are acknowledged only after the application confirms it stored them.

Element names used by the typed views follow published POCT1-A examples and vendor interface documents. Vendor-specific elements remain reachable through the element tree.

## Example

```rust
use oxim_poct1a::{Message, SplitEvent, Splitter};

fn main() -> Result<(), oxim_poct1a::ParseError> {
    let mut splitter = Splitter::default();
    splitter.push(br#"<OBS.R01><HDR><HDR.control_id V="7"/></HDR><SVC><PT>
      <PT.patient_id V="P1"/>
      <OBS><OBS.observation_id V="GLU"/><OBS.value V="5.4" U="mmol/L"/></OBS>
    </PT></SVC></OBS.R01>"#);

    while let Some(event) = splitter.next_event() {
        if let SplitEvent::Document(bytes) = event {
            let message = Message::parse(&bytes)?;
            for observation in message.observations() {
                println!("{:?} = {:?} {:?}", observation.observation_id(), observation.value(), observation.unit());
            }
        }
    }
    Ok(())
}
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
