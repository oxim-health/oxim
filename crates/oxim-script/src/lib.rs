//! JavaScript filters, transformers and encoders for OXIM channels, run in
//! an embedded QuickJS sandbox.
//!
//! [`register`] adds the step type `script` as a filter, a transformer and
//! an encoder:
//!
//! ```yaml
//! filters:
//!   - type: script
//!     source: msg.get('MSH-9.1') === 'ORU'
//! transformers:
//!   - type: script
//!     file: normalize-names.js     # below the scripts directory
//!     timeout: 500ms
//! destinations:
//!   - id: archive
//!     type: file
//!     encoder:
//!       type: script
//!       data_type: json
//!       source: return JSON.stringify({ id: msg.get('MSH-10'), patient: msg.get('PID-3.1') });
//! ```
//!
//! # Settings
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `source` | | the script, inline |
//! | `file` | | a script file relative to the scripts directory (no absolute paths, no `..`) |
//! | `timeout` | `1s` | how long the script may run per message |
//! | `memory_limit` | `32MiB` | heap limit of the script's runtime (at least `1MiB`) |
//! | `max_stack` | `512KiB` | stack limit (`64KiB` to `1MiB`) |
//! | `mirth` | `false` | Mirth Connect compatibility globals (see below) |
//! | `data_type` | the message's | encoders only: the data type of the output |
//!
//! Exactly one of `source` and `file` is required. Scripts are compiled when
//! the channel is deployed, so syntax errors are configuration errors.
//!
//! # Script API
//!
//! | Name | Meaning |
//! |---|---|
//! | `msg.get(path)` | the text at `path` (null when absent); paths follow the data type: `PID-5.1`, `OBX[2]-5` (HL7 v2), `R[2]-4` (ASTM), `results[0].value` (JSON), `/order/@id` (XML), `3/name` (delimited, fixed width) |
//! | `msg.set(path, value)` | stores text at `path` (transformers only) |
//! | `msg.raw` | the message as text, with the changes made so far |
//! | `msg.dataType` | the data type, for example `hl7v2` |
//! | `clinical` | the normalized content as an object (null without `normalize: true`); a transformer may change it or assign a new object, which must be valid normalized content |
//! | `vars` | the message variables; values are stored as text |
//! | `reply(data, dataType?)` | sets the reply to the sender (transformers only, for `source.response.mode: pipeline`); `data` is a string or a `Uint8Array` |
//! | `log.debug/info/warn/error(...)` | writes to the OXIM log with the channel and message identifiers |
//!
//! A script is a function body. A filter returns `true` or `false`, an
//! encoder returns a string or a `Uint8Array`, and a transformer returns
//! nothing. Filters and encoders may also be a single expression, as in
//! `msg.get('MSH-9.1') === 'ORU'`. Filters and encoders see the message
//! read-only: `msg.set` and `reply` throw, and changes to `vars` and
//! `clinical` are discarded.
//!
//! # Sandbox
//!
//! Scripts have no filesystem, network, timer or module access; `eval` is
//! removed and the `Function` constructors throw, so no code is generated
//! from strings. Each run is interrupted after `timeout`, the heap is
//! limited to `memory_limit` and the stack to `max_stack`; a script that
//! exceeds a limit fails the message, which can be reprocessed. Failures
//! name the script and, for exceptions, the line and column.
//!
//! Each script is compiled into a small pool of QuickJS runtimes that are
//! reused across messages. Global variables a script creates may therefore
//! survive between messages in one runtime; scripts must not rely on them
//! (use `globalChannelMap` or `vars` to pass data on).
//!
//! # Mirth Connect compatibility
//!
//! With `mirth: true` a script sees what Mirth Connect scripts expect:
//!
//! - `msg` and `tmp` give E4X-style access to HL7 v2 messages:
//!   `msg['PID']['PID.5']['PID.5.1'].toString()`, repeated segments and
//!   fields by zero-based index (`msg['OBX'][1]['OBX.5']['OBX.5.1']`,
//!   `msg['PID']['PID.3'][1]['PID.3.1']`), `.length()` counts, and
//!   assignment (`msg['PID']['PID.8'] = 'F'`). Values compare with `==` to
//!   strings. `msg.get`, `msg.set` and `msg.raw` stay available.
//! - `channelMap`, `connectorMap` and `responseMap` (`put`, `get`,
//!   `containsKey`, `remove`, `keySet`) are per-message maps kept in the
//!   message variables: `channelMap` entries under their own names,
//!   the others prefixed `connectorMap.` and `responseMap.`.
//! - `globalChannelMap` (shared by the scripts of a channel) and `globalMap`
//!   (shared by all scripts) keep values as JSON until OXIM restarts.
//! - `$c`, `$co`, `$r`, `$gc`, `$g` read and write those maps; `$(key)`
//!   searches them in Mirth Connect's order.
//! - `logger.debug/info/warn/error`.
//!
//! E4X XML literal syntax (`<tag/>`), `for each` loops and XML methods other
//! than `toString`, `text` and `length` are not supported.

mod environment;
mod runtime;
mod settings;
mod steps;

use std::sync::Arc;

use oxim_core::{Encoder, Filter, Registry, Transformer};

pub use environment::ScriptEnvironment;
pub use steps::{ScriptEncoder, ScriptFilter, ScriptTransformer};

/// Registers the `script` filter, transformer and encoder.
pub fn register(registry: &mut Registry, environment: ScriptEnvironment) {
    let filters = environment.clone();
    let transformers = environment.clone();
    let encoders = environment;
    registry
        .add_filter("script", move |step| {
            Ok(Arc::new(ScriptFilter::from_step(step, &filters)?) as Arc<dyn Filter>)
        })
        .add_transformer("script", move |step| {
            Ok(Arc::new(ScriptTransformer::from_step(step, &transformers)?)
                as Arc<dyn Transformer>)
        })
        .add_encoder("script", move |step| {
            Ok(Arc::new(ScriptEncoder::from_step(step, &encoders)?) as Arc<dyn Encoder>)
        });
}
