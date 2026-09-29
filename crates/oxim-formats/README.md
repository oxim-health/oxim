# oxim-formats

Lossless JSON, XML, delimited (CSV/TSV) and fixed-width documents for Rust, part of [OXIM](../../README.md). These are the non-HL7 data types that OXIM channels read and write.

Every format follows the same pattern: parse bytes into a typed document, read and write values by path, serialize. Unmodified documents serialize to their original bytes, and edits touch only what they change. `Document` offers one text-based interface over all formats.

| Format | Paths | Notes |
|---|---|---|
| JSON | JSON Pointer (`/results/0/value`) or dotted form (`results[0].value`), 0-based | Member order and number text are preserved (`5.40` stays `5.40`); edited documents are re-serialized compactly |
| XML | XPath subset: `/order/test[2]/@code`, `/order/test/text()`, `*`, prefixed or local names, 1-based | Comments, CDATA, quoting and references kept verbatim; DTDs and custom entities rejected; encoding from the XML declaration |
| Delimited | `row/column`, 0-based; column index or header name | RFC 4180 quoting or a custom escape character; lenient parsing; values quoted only when needed |
| Fixed width | `record/field`, 0-based | Declared byte positions, left or right alignment, custom pad characters; overflow rejected unless truncation is enabled |

The crate performs no I/O, never panics on malformed input, bounds memory with configurable limits, and is covered by unit tests, property tests and fuzzing.

## Example

```rust
use oxim_formats::{DataType, DelimitedOptions, Document, JsonOptions, XmlOptions};

fn main() -> Result<(), oxim_formats::FormatError> {
    let mut json = Document::parse(
        br#"{"results":[{"code":"GLU","value":5.40}]}"#,
        &DataType::Json(JsonOptions::default()),
    )?;
    assert_eq!(json.get("results[0].value")?.as_deref(), Some("5.40"));
    json.set("results[0].unit", "mmol/L")?;

    let mut xml = Document::parse(
        br#"<order><test code="GLU">Glucose</test></order>"#,
        &DataType::Xml(XmlOptions::default()),
    )?;
    assert_eq!(xml.get("/order/test/@code")?.as_deref(), Some("GLU"));
    xml.set("/order/test[2]/@code", "HGB")?;

    let csv = Document::parse(
        b"sample,test,value\r\nS1,GLU,5.4\r\n",
        &DataType::Delimited(DelimitedOptions::csv().with_header(true)),
    )?;
    assert_eq!(csv.get("0/test")?.as_deref(), Some("GLU"));
    Ok(())
}
```

## Note on JSON numbers

Exact number text relies on `serde_json`'s `arbitrary_precision` feature. Cargo unifies features across a build, so other crates built together with `oxim-formats` see `serde_json` numbers in that representation.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
