# oxim-model

Shared data types for [OXIM](../../README.md):

- **Envelope:** a message exactly as received, with a time-ordered `MessageId` (ULID), channel and connector identifiers, receive `Timestamp` and `DataType`.
- **Normalized clinical model:** a small, FHIR-aligned subset (`Patient`, `Specimen`, `Order`, `Observation`, `Device`, quality control, host queries and device events) that every protocol maps to and from, so results from different analyzers leave OXIM in one shape.
- **Exact values:** `Decimal` keeps numbers as written (`5.40` stays `5.40`), and `ClinicalDateTime` keeps the recorded precision and offset of clinical times, reading and writing both HL7 v2 `DTM` and ISO 8601 / FHIR forms.

The model carries values; it never interprets them. The crate performs no I/O and never reads the clock or a random source.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
