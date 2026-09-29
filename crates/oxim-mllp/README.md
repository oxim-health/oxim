# oxim-mllp

Sans-IO MLLP (Minimal Lower Layer Protocol) framing for HL7 v2, part of [OXIM](../../README.md).

MLLP wraps each message in a start block (`0x0B`) and an end block followed by a carriage return (`0x1C 0x0D`). This crate performs no I/O: feed it the bytes read from a socket, serial line or file and act on the events it returns.

- **Incremental:** frames may arrive in any number of chunks; partial frames are never rescanned.
- **Robust:** bytes outside frames, interrupted frames, oversized frames and truncated streams are reported as `Discarded` events with a reason and an exact byte count, and decoding continues with the next frame.
- **Bounded:** frames larger than the configured limit are dropped without being buffered completely.
- **Tolerant or strict:** by default an end block without the carriage return still ends a frame; strict mode rejects it.
- **MLLP release 2:** commit acknowledgment frames are recognized as `CommitAck` and `CommitNak`.

## Example

```rust
use oxim_mllp::{Decoder, Event, encode};

fn main() -> Result<(), oxim_mllp::EncodeError> {
    let mut decoder = Decoder::default();
    let frame = encode(b"MSH|^~\\&|LAB\r")?;

    decoder.push(&frame[..5]);
    assert_eq!(decoder.next_event(), None);

    decoder.push(&frame[5..]);
    assert_eq!(decoder.next_event(), Some(Event::Frame(b"MSH|^~\\&|LAB\r".to_vec())));
    Ok(())
}
```

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
