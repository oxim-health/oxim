# OXIM

**Open eXchange for Interoperable Medicine** is an open-source clinical integration engine written in Rust. It runs entirely on-premise, including on air-gapped networks, and connects laboratory analyzers, point-of-care devices and imaging modalities to LIS, HIS/EHR, RIS and PACS systems with guaranteed delivery and full visibility of every message.

> **Status: pre-1.0, under active development. Not for production use.**
> No release has been published. The first tagged release will be the complete 1.0 described in the [specification](docs/SPEC.md).

## What OXIM does

- **Speaks the protocols of the lab and the hospital:** HL7 v2.x over MLLP, ASTM E1381/E1394 (CLSI LIS01/LIS02) over TCP and serial lines, POCT1-A, HL7 FHIR, CDA, DICOM and more.
- **Looks like a LIS to devices and like one standardized source to the LIS:** results from every analyzer leave OXIM in one format (HL7 ORU^R01 by default), orders are routed to the right analyzer, barcode queries are answered in real time.
- **Never loses a message:** the sender is acknowledged only after the message is durably stored, and every destination has its own persistent queue with retries.
- **Shows everything:** a local web UI, full-text message search, resend, alerts such as "analyzer silent for 30 minutes", metrics and an audit trail.
- **Stays local:** a single binary with an embedded database. No cloud dependency, no telemetry.

OXIM moves and reformats clinical data; it never interprets it (see [ADR 0011](docs/adr/0011-no-clinical-interpretation.md)).

## Repository layout

| Path | Contents |
|---|---|
| [`crates/oxim-hl7`](crates/oxim-hl7) | Lossless HL7 v2 parsing, editing, serialization and acknowledgments |
| [`crates/oxim-mllp`](crates/oxim-mllp) | Sans-IO MLLP framing |
| [`crates/oxim-astm`](crates/oxim-astm) | ASTM E1381/E1394 (CLSI LIS01/LIS02): frames, link sessions, unframed streams and lossless messages |
| [`crates/oxim-formats`](crates/oxim-formats) | Lossless JSON, XML, delimited and fixed-width documents |
| [`crates/oxim-model`](crates/oxim-model) | Message envelope, identifiers and the normalized, FHIR-aligned clinical model |
| [`crates/oxim-poct1a`](crates/oxim-poct1a) | POCT1-A stream splitting, lossless messages, builders and the host conversation |
| [`crates/oxim-store`](crates/oxim-store) | Durable message store and per-destination delivery queues (SQLite) |
| [`docs/SPEC.md`](docs/SPEC.md) | Product and engineering specification |
| [`docs/adr`](docs/adr) | Architecture decision records |
| [`fuzz`](fuzz) | Fuzz targets (cargo-fuzz) |

The full crate map is described in [section 14 of the specification](docs/SPEC.md#14-architecture).

## Building

OXIM requires Rust 1.88 or newer.

```sh
cargo build --workspace
cargo test --workspace
```

The checks run by CI can be reproduced locally:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo deny check
```

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) and the [code of conduct](CODE_OF_CONDUCT.md) first. Security issues must be reported privately as described in [SECURITY.md](SECURITY.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in OXIM by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
