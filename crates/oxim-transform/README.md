# oxim-transform

Declarative filters, mapping operations and code tables for [OXIM](../../README.md) channels.

`oxim_transform::register(&mut registry, TransformEnvironment::new(config_dir))` adds three step types:

| Type | Kind | Purpose |
|---|---|---|
| `condition` | filter | keep messages that match a condition tree |
| `map` | transformer | edit the parsed document with ordered operations |
| `map-observations` | transformer | translate codes in the normalized clinical model |

Steps carry values; they never interpret them. Conditions compare text and numbers, operations copy, reformat and translate, but nothing computes, verifies or flags a clinical result ([ADR 0011](../../docs/adr/0011-no-clinical-interpretation.md)). Code tables are read when a channel is deployed; processing never touches files, the network or the clock.

## Paths

Paths follow the document's data type: `PID-5.1` (HL7 v2), `R[2]-3.4` (ASTM, where the record type is field 1), `/order/test/@code` (XML), `results[0].value` (JSON), `2/code` (delimited and fixed width).

A single `[*]` after an HL7 segment, an ASTM record or an XML element means "every occurrence": `OBX[*]-3.1`, `R[*]-3.4`, `/order/test[*]/@code`. A map operation with such a path runs once per occurrence. In a filter, a wildcard condition holds when any occurrence satisfies it.

## `condition` filter

```yaml
filters:
  - type: condition
    all:
      - {path: MSH-9.1, in: [ORU, OUL]}
      - any:
          - {path: "OBX[*]-8", equals: H}
          - {path: "OBX[*]-8", equals: L}
      - not: {variable: skip, equals: "yes"}
```

Groups: `all: [...]`, `any: [...]`, `not: {...}` (exactly one key per group, nested up to 32 levels).

A leaf reads a `path` or a `variable` and has exactly one test:

| Test | Holds when |
|---|---|
| `equals: X`, `not_equals: X` | the value is (is not) `X`; a missing value is never equal |
| `in: [..]`, `not_in: [..]` | the value is (is not) in the list |
| `matches: REGEX` | the value matches the regular expression (Rust `regex` syntax) |
| `exists: true` | the value is present and not empty (`false` inverts) |
| `empty: true` | the value is missing or empty (`false` inverts) |
| `greater_than`, `greater_or_equal`, `less_than`, `less_or_equal` | both sides are decimal numbers and compare as stated |

`case_insensitive: true` applies to `equals`, `not_equals`, `in`, `not_in` and `matches`.

Numeric tests compare the decimal text exactly, without floating point: `5.40` equals `5.4`, `1e2` equals `100`. Values that are not plain decimals, such as `<0.5` or `POS`, never satisfy a numeric test. Quote numbers in YAML (`"5.40"`) when their exact text matters.

## `map` transformer

```yaml
transformers:
  - type: map
    operations:
      - set: {path: MSH-3, value: OXIM}
      - set: {path: MSH-10, value: "{message.id}"}
      - copy: {from: PID-3.1, to: PID-2}
      - clear: {path: PID-19}
      - trim: {path: PID-5.1}
      - upper: {path: PID-5.1}
      - lower: {path: PID-5.2}
      - replace: {path: PID-3.1, pattern: "^0+", with: ""}
      - pad: {path: OBR-3, length: 10, char: "0", side: left}
      - substring: {path: PID-5.1, start: 0, length: 20}
      - date: {path: PID-7, from: hl7, to: "%d.%m.%Y"}
      - lookup: {table: tables/tests.csv, from: "OBX[*]-3.1", on_missing: keep}
      - store: {path: PID-3.1, as: patient_id}
      - when: {path: MSH-9.1, equals: ORU}
        set: {path: MSH-5, value: "LIS-{$patient_id}"}
```

Each operation has exactly one action and an optional `when` condition, evaluated per occurrence for wildcard operations. Operations run in order.

| Action | Settings | Effect |
|---|---|---|
| `set` | `path`, `value` (template) | writes the rendered template |
| `copy` | `from`, `to` | copies a value; a missing source writes an empty value |
| `clear` | `path` | empties a value |
| `trim`, `upper`, `lower` | `path` | trims whitespace or changes letter case |
| `replace` | `path`, `pattern`, `with`, `case_insensitive` | regular-expression replacement; `with` may use `$1` groups |
| `pad` | `path`, `length`, `char` (default space), `side` (`left` default, or `right`) | pads to a minimum length in characters; never truncates |
| `substring` | `path`, `start` (0-based characters), `length` | keeps part of a value |
| `date` | `path`, `from`, `to`, `on_invalid` (`error` default, `keep`, `empty`) | converts between `hl7`, `iso` and patterns, keeping the recorded precision |
| `lookup` | `table`, `from`, `to` (default `from`), `column` (`to`, `display`, `system`), `context` (template), `default` (template), `on_missing` (`keep` default, `empty`, `error`), `case_insensitive` | translates a code through a code table; `keep` writes the original code |
| `store` | `path` or `value` (template), `as` | saves a value in a variable for later operations |

Operations that read a missing value leave the document unchanged.

### Templates

| Placeholder | Inserts |
|---|---|
| `{PID-3.1}` | a document value |
| `{OBX[*]-5}` | the value in the current wildcard occurrence |
| `{$name}` | a variable |
| `{channel}`, `{message.id}`, `{message.received_at}` | channel identifier, message identifier, receive time (RFC 3339, UTC) |
| `{message.connector}`, `{message.data_type}`, `{message.peer}`, `{message.device}`, `{message.correlation_id}` | envelope properties |
| `{message.metadata.KEY}` | connector metadata |

`{PID-8|U}` inserts `U` when the value is missing or empty. Write `{{` and `}}` for literal braces.

### Date patterns

`%Y` (4-digit year), `%m`, `%d`, `%H`, `%M`, `%S` (two digits each) and `%%`; other characters must appear literally. A pattern must describe a date from the year down without gaps. Patterns carry no UTC offset. `hl7` (also `astm`) and `iso` (also `fhir`) keep offsets and precision.

## `map-observations` transformer

```yaml
transformers:
  - type: map-observations
    table: tables/chemistry.csv
    system: http://loinc.org      # optional; otherwise the table's system column
    context: "{message.device}"   # optional; selects context rows
    on_missing: keep              # keep | drop | error
    device_id: "{message.device}" # optional
    operator: "{$operator}"       # optional
```

Translates the codes of result and quality-control observations, order tests and host-query tests in the normalized content (the channel source needs `normalize: true`). The translated code becomes the primary coding and the device coding is kept after it. Values, units, ranges and flags are never changed.

## Code tables

CSV with a header row; `from` and `to` are required, `display`, `system` and `context` are optional, in any order:

```csv
from,to,display,system,context
GLU,1520,Glucose,urn:lis,
GLU,1521,Glucose (POCT),urn:lis,poct-1
```

Rows with a `context` apply only when a lookup names that context; rows without one apply everywhere. A code defined twice for the same context is an error. Table paths are relative to the configuration directory; absolute paths and `..` are rejected. Tables are cached and reloaded on redeploy when the file changed.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
