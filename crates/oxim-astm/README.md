# oxim-astm

ASTM E1381 and E1394 (CLSI LIS01-A2 and LIS02-A2) for laboratory instruments, part of [OXIM](../../README.md).

- **LIS01 frames:** encode and decode `<STX> FN text <ETB|ETX> C1 C2 <CR><LF>` frames with checksum validation, and split messages into frames (one record per frame, long records across ETB frames).
- **LIS01 link sessions:** a sans-IO state machine for one half-duplex link that both receives and sends: ENQ/ACK/NAK establishment, frame acknowledgment, duplicate-frame detection, up to six retransmissions, the 15 s reply and 30 s receive timers, contention rules for instrument and host roles, receiver interrupts and busy refusal. You supply bytes and time; it tells you what to transmit and what was received.
- **Unframed streams:** many analyzers send ASTM records over TCP without LIS01. `RawSplitter` cuts such streams into `H` ... `L` messages with exact accounting of every dropped byte.
- **Lossless LIS02 messages:** unmodified messages serialize byte for byte, with custom delimiters, CR/LF/CRLF record endings and non-UTF-8 bytes preserved. Values are addressed with paths such as `R-4`, `R[2]-3.4` or the dotted `R.4` used in the standard, edited with automatic padding, and decoded with an explicit character encoding.
- **Hierarchy:** group records into patients, orders, results and their comments.
- **Safe for untrusted input:** no `unsafe`, no panics on malformed input, bounded memory, covered by property tests and fuzzing.

## Field numbering

ASTM numbers fields from the record type: **field 1 is the record type** (`R`), so `R-3` is the universal test ID and `R-4` the measurement value, exactly as `R.3` and `R.4` in the standard. In the header, field 2 is the delimiter definition (`\^&`) and cannot be edited. This differs from HL7, where the segment identifier is not a field.

## Example

```rust
use oxim_astm::Message;

fn main() -> Result<(), oxim_astm::ParseError> {
    let input = b"H|\\^&|||ANALYZER|||||LIS||P|1\r\
P|1||PID001||Doe^Jane\r\
O|1|SMP001||^^^GLU\r\
R|1|^^^GLU|5.4|mmol/L|3.9^6.1|N||F\r\
L|1|N\r";

    let message = Message::parse(input)?;
    assert_eq!(message.to_bytes(), input);
    assert_eq!(message.get("R-3.4").unwrap(), "GLU");
    assert_eq!(message.get("R-4").unwrap(), "5.4");
    assert_eq!(message.patients()[0].orders[0].results.len(), 1);
    Ok(())
}
```

A link session is driven like this:

```rust
use std::time::Instant;
use oxim_astm::session::{Output, Session, SessionConfig};

let mut session = Session::new(SessionConfig::default());
// for every read from the serial port or socket:
//     session.handle_input(&bytes, Instant::now());
// when session.poll_timeout() expires:
//     session.handle_timeout(Instant::now());
// then drain outputs:
while let Some(output) = session.poll_output() {
    match output {
        Output::Transmit(bytes) => { /* write bytes to the link */ }
        Output::Received(message) => { /* parse with oxim_astm::Message */ }
        _ => {}
    }
}
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
