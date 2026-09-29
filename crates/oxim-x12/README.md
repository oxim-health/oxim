# oxim-x12

Lossless ASC X12 EDI interchanges for [OXIM](../../README.md): parsing, editing, envelope validation and acknowledgments.

```rust
use oxim_x12::Interchange;

let mut interchange = Interchange::parse(input)?;
assert_eq!(interchange.to_bytes(), input);          // byte for byte
let member = interchange.get("NM1[2]-9");            // path notation
let place = interchange.get("CLM05-01");             // reference designator
interchange.set("NM1[2]-4", "JANET")?;
let issues = interchange.validate();                 // envelope problems
```

- **Delimiters from `ISA`:** element separator, repetition separator (`ISA-11`, version 00402 and later; `U` before), component separator (`ISA-16`) and segment terminator. Line breaks after terminators, a missing final terminator and whitespace or a byte order mark before `ISA` are kept.
- **Paths:** `NM1-3`, `NM1[2]-3` (second `NM1`), `CLM-5.1` (component), `HI-1[2].2` (repetition and component), `ST-0` (segment identifier), and X12 reference designators such as `NM103` and `CLM05-01`.
- **No escaping:** X12 has no escape mechanism, so text containing a delimiter is rejected instead of silently corrupting the interchange.
- **Envelope:** `envelope()` lists interchanges, functional groups (`GS`) and transaction sets (`ST`) with their control numbers. `validate()` reports missing `IEA`/`GE`/`SE` trailers, trailer control numbers that differ from the header and wrong segment, transaction set and group counts. `ParseOptions::strict()` rejects such interchanges.
- **Acknowledgments:** `build_ta1` (interchange), `build_functional_ack` with `997` (functional, 4010) or `999` (implementation, `005010X231A1`). Envelope issues are reported automatically (`AK5`/`IK5` codes 2, 3, 4; `AK9` codes 3, 4, 5; `TA1` notes 001, 021, 023, 024); implementation guide errors are added as `AK3`/`IK3` and `AK4`/`IK4` entries. Sender and receiver are swapped; control numbers, date and time come from the caller.

Implementation guide rules (loops, situational elements, code lists) are outside the scope of this crate. The crate performs no I/O and never reads the clock.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
