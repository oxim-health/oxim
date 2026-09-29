# oxim-script

JavaScript filters, transformers and encoders for [OXIM](../../README.md) channels, run in an embedded [QuickJS](https://github.com/quickjs-ng/quickjs) sandbox (through [rquickjs](https://crates.io/crates/rquickjs)).

`oxim_script::register(&mut registry, ScriptEnvironment::new("scripts"))` adds the step type `script` as a filter, a transformer and an encoder:

```yaml
filters:
  - type: script
    source: msg.get('MSH-9.1') === 'ORU'
transformers:
  - type: script
    file: normalize-names.js     # relative to the scripts directory
    timeout: 500ms
destinations:
  - id: archive
    type: file
    encoder:
      type: script
      data_type: json
      source: return JSON.stringify({ id: msg.get('MSH-10'), patient: msg.get('PID-3.1') });
```

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `source` | | the script, inline |
| `file` | | a script file relative to the scripts directory (`scripts_dir` in `oxim.yaml`); absolute paths and `..` are rejected |
| `timeout` | `1s` | how long the script may run per message |
| `memory_limit` | `32MiB` | heap limit (at least `1MiB`) |
| `max_stack` | `512KiB` | stack limit (`64KiB` to `1MiB`) |
| `mirth` | `false` | Mirth Connect compatibility globals |
| `data_type` | the message's | encoders only: the data type of the output |

Exactly one of `source` and `file` is required. Scripts are compiled when the channel is deployed, so a syntax error is reported as a configuration error with its line.

## Script API

| Name | Meaning |
|---|---|
| `msg.get(path)` | the text at `path`, or `null`; paths follow the data type: `PID-5.1`, `OBX[2]-5`, `PID-3[2].1` (HL7 v2), `R[2]-4` (ASTM), `results[0].value` (JSON), `/order/@id` (XML), `3/name` (delimited, fixed width) |
| `msg.set(path, value)` | stores text at `path` (transformers only) |
| `msg.raw` | the message as text, including the changes made so far |
| `msg.dataType` | the data type, for example `hl7v2` |
| `clinical` | the normalized content (`normalize: true`) as an object, or `null`; a transformer may edit it or assign a new object, which must be valid normalized content |
| `vars` | the message variables shared with other steps; values are stored as text |
| `reply(data, dataType?)` | sets the reply to the sender (transformers of channels with `source.response.mode: pipeline`); `data` is a string or a `Uint8Array` |
| `log.debug/info/warn/error(...)` | writes to the OXIM log with the channel and message identifiers |

A script is a function body:

- a **filter** returns `true` to keep the message or `false` to filter it;
- a **transformer** changes `msg`, `clinical`, `vars` or the reply and returns nothing;
- an **encoder** returns the bytes to send, as a string (UTF-8) or a `Uint8Array`.

Filters and encoders may also be written as a single expression, such as `msg.get('MSH-9.1') === 'ORU'`. They see the message read-only: `msg.set` and `reply` throw, and changes to `vars` and `clinical` are discarded.

## Sandbox and limits

- No filesystem, network, timers or module loading; `eval` is removed and the `Function` constructors throw, so no code is generated from strings.
- Each run is interrupted after `timeout`; the interruption cannot be caught by the script.
- The heap is limited to `memory_limit` and the stack to `max_stack`.
- A script that fails, throws or exceeds a limit fails the message with an error that names the script and, for exceptions, the line and column. The message is stored and can be reprocessed after the script is fixed.

Each script is compiled into a small pool of QuickJS runtimes that are reused across messages and threads; a runtime that hit a limit is discarded. Global variables a script creates may survive between messages in one runtime, so scripts must not rely on them. A small HL7 transformer (one get, one set, one variable) costs about 17 µs per message in a release build.

## Mirth Connect compatibility

With `mirth: true` a script sees what Mirth Connect scripts expect, so imported filter rules and transformer steps run unchanged (`{type: script, mirth: true, source: ...}`):

| Mirth Connect | OXIM |
|---|---|
| `msg['PID']['PID.5']['PID.5.1'].toString()` | E4X-style access to HL7 v2 messages; values compare with `==` to strings |
| `msg['OBX'][1]['OBX.5']['OBX.5.1']`, `msg['PID']['PID.3'][1]['PID.3.1']` | repeated segments and fields by zero-based index |
| `msg['OBX'].length()` | number of segments, or of field repetitions |
| `msg['PID']['PID.8'] = 'F'` | assignment at any level below a segment |
| `tmp` | the same message as `msg`, since OXIM edits the message in place |
| `channelMap`, `connectorMap`, `responseMap` | per-message maps (`put`, `get`, `containsKey`, `remove`, `keySet`) kept in the message variables: `channelMap` entries under their own names, the others prefixed `connectorMap.` and `responseMap.`; values are stored as text |
| `globalChannelMap`, `globalMap` | shared by the scripts of one channel, or by all scripts; values are kept as JSON until OXIM restarts |
| `$c`, `$co`, `$r`, `$gc`, `$g`, `$` | map shortcuts; `$(key)` searches the maps in Mirth Connect's order |
| `logger.debug/info/warn/error` | the OXIM log |

Not supported: E4X XML literals (`<tag/>`), `for each` loops, XML methods other than `toString`, `text` and `length`, access to Java classes, and Mirth's code template libraries. The Mirth Connect importer reports scripts that use them.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option. QuickJS is distributed under the MIT license.
