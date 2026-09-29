# oxim-ncpdp

Lossless NCPDP Telecommunication Standard transmissions (version D.0; the 5.1 framing is compatible) for [OXIM](../../README.md): pharmacy claims, eligibility and other pharmacy transactions.

```rust
use oxim_ncpdp::Transmission;

let mut claim = Transmission::parse(input)?;
assert_eq!(claim.to_bytes(), input);                 // byte for byte
let bin = claim.get("HDR.A1");                        // header field
let rx = claim.get("AM07.D2");                        // Claim segment, field D2
claim.set("AM07[2].D2", "000000654321")?;             // second transaction's claim
```

- **Header:** the fixed-width request header (56 bytes) and response header (31 bytes) are read and written field by field, by data dictionary identifier (`A1` BIN number, `A3` transaction code, `B1` service provider, `D1` date of service, …).
- **Segments and groups:** every segment starts with the segment separator (`0x1E`), every transaction group with the group separator (`0x1D`), every field with the field separator (`0x1C`) and a two-character identifier. `AM07` names the segment whose identification field (`AM`) is `07` (Claim).
- **Paths:** `HDR.A1`, `AM07.D2`, `AM07[2].D2` (second Claim segment), `AM08.E4[2]` (second occurrence of a repeating field). Writing a field that is missing appends it to the segment.
- **Validation:** the header's transaction count against the number of groups, and segments without a leading identification field.
- **Responses:** `build_response` answers a request: header status accepted or rejected, per transaction paid, captured, approved, duplicate or rejected with reject codes, an authorization number and message texts, and the claim reference echoed in a Response Claim segment.

NCPDP SCRIPT (XML e-prescribing) is a different standard and out of scope; SCRIPT messages can be handled as XML documents. Transport framing (for example STX/ETX or length prefixes) must be removed before parsing. The crate performs no I/O.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
