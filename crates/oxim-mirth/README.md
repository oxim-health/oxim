# oxim-mirth

Imports Mirth Connect channels into [OXIM](../../README.md) channel files, with a migration report that says what became of every element.

```text
oxim import mirth export.xml --out channels
oxim import mirth backup.xml --out channels --value lis_host=10.0.0.20 --report report.json
oxim import mirth channel.xml --library code-templates.xml --dry-run
```

```rust
let result = oxim_mirth::import(&xml, &oxim_mirth::ImportOptions::new())?;
for channel in &result.channels {
    std::fs::write(&channel.file_name, &channel.yaml)?;
}
std::fs::write("report.md", result.report.to_markdown())?;
```

## Inputs

Mirth Connect 3.x and 4.x exports:

- a single channel export (`<channel>`), with the code template libraries it includes;
- a channel group export (`<channelGroup>`);
- a server configuration backup (`<serverConfiguration>`): channels, code template libraries, global scripts, configuration map, alerts and users;
- a code template library export, passed with `--library` so its functions reach the channels that call them.

The XML is read without document type declarations or custom entities, so an export can never make the importer read other files.

## Outputs

One channel file per channel, named after the channel (`ADT Inbound` becomes `adt-inbound.yaml`; duplicates get `-2`, `-3`). Every file starts with a comment that names the Mirth channel and counts its report items. A channel whose source connector has no OXIM equivalent yet is written as `<id>.yaml.draft` and disabled: OXIM does not load it until the source is replaced and the file renamed.

The migration report (Markdown or JSON) lists every element as:

| Result | Meaning |
|---|---|
| converted | behaves as in Mirth Connect |
| approximated | converted, with a difference the report describes |
| unsupported | left out; needs attention before the channel goes live |

Each item names the channel, the element, the reason and its XPath-style location in the export.

## Conversion

| Mirth Connect | OXIM | Result |
|---|---|---|
| TCP Listener, MLLP mode | `mllp` source (`listen`, `max_connections`) | converted |
| TCP Listener, basic TCP mode | `tcp` source with `delimited` or `none` framing | converted |
| TCP Sender, MLLP / basic TCP | `mllp` destination (`target`, `ack`, `ack_timeout`) / `tcp` destination | converted |
| TCP connectors in the reverse role (listener in client mode, sender in server mode), MLLP with custom frame bytes | — / raw TCP framing | unsupported / approximated |
| File Reader, File Writer on local files | `file` source / destination | converted; unsupported options approximated |
| File connectors over FTP, SFTP, SMB, S3, WebDAV | — | unsupported (planned) |
| HTTP Sender (POST, PUT) | `http` destination with headers, query parameters and timeout | converted; credentials are never copied |
| HTTP Listener | `http` source with the context path and a fixed 2xx status | converted; other methods, XML request conversion and authentication approximated |
| Channel Reader / Channel Writer | `channel` source / destination; the writer's target is resolved to the imported channel's id | converted; a target outside the export gets a placeholder (approximated) |
| Database, JavaScript, SMTP, JMS, DICOM, web service, document writer | — | unsupported (planned connectors) |
| Data types HL7V2, XML, JSON, RAW | `hl7v2`, `xml`, `json`, `raw` | converted |
| Data type DELIMITED | `delimited` (delimiter, quote, header) or `fixed_width` (column widths) in `source.format` | converted |
| Data types HL7V3, EDI/X12, NCPDP, DICOM | `xml`, `x12`, `ncpdp`, `dicom` | approximated |
| Rule builder rules on HL7 v2 fields (`msg['PID']['PID.3']['PID.3.1']`) | `path-equals`, `path-in`, `path-exists`, `condition` (`matches` for contains) | converted |
| Rules joined with OR | one `condition` filter with `any`/`all`, or one `script` filter when JavaScript is involved | converted |
| JavaScript rules and steps | `script` steps with `mirth: true`, prefixed with the code template functions they call | converted; Java and router APIs are flagged |
| Mapper steps (channel or connector map) | `map` `store` operations (defaults via `{path\|default}`) | converted |
| Message builder steps on HL7 v2 fields | `map` `copy` and `set` operations | converted |
| Mapper and message builder steps with replacements, global maps or `tmp` | `script` steps with the JavaScript Mirth generates | converted / approximated |
| XSLT steps, iterator and other plugin steps, response transformers | — | unsupported |
| Queue, retry interval, retry count, rotate, thread count | destination `queue`: fixed interval, `max_attempts` without a queue, `best_effort` for rotate | converted; threads approximated |
| Response from a destination (`d1`) | `source.response` with `mode: destination` (MLLP sources) | converted |
| Auto-generated responses | OXIM's acknowledgment after durable storage | converted / approximated |
| Destination chains (wait for previous) | independent destination queues | approximated |
| Channel preprocessor, postprocessor, deploy and undeploy scripts; global scripts | — | unsupported (trivial default scripts are ignored) |
| Initial state, encryption, content removal, storage mode, metadata columns, pruning | channel `enabled`, engine retention | approximated |
| Attachment handlers, alerts, users | — | unsupported |
| `${name}` placeholders in connector settings | values from the configuration map or `--value name=value` | converted; missing values approximated |

## The `script` step

JavaScript is kept as Mirth wrote it and runs in OXIM's script runtime in Mirth compatibility mode (`{type: script, mirth: true, source: ...}`): filter scripts are function bodies that return a boolean, transformer scripts are statements that edit `msg`, and values stored by `map` operations are available as channel map entries. Scripts that use Java classes, `router`, attachments or E4X XML literals are reported as approximated so they can be reviewed.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
