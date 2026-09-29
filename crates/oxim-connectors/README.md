# oxim-connectors

Source and destination connectors for [OXIM](../../README.md). Register them all with `oxim_connectors::register(&mut registry)`; channel files then refer to them by type name.

Sources acknowledge senders only after the message is stored durably. Destinations report temporary failures (retried according to the destination's retry policy) and permanent ones (the delivery fails at once).

Byte sequences in settings are written as text with the escapes `\r`, `\n`, `\t`, `\\` and `\xHH`, or as `hex:` followed by hexadecimal digits. Durations are written like `250ms`, `5s`, `2m`.

## `mllp`: HL7 v2 over MLLP

### Source

| Setting | Default | Meaning |
|---|---|---|
| `listen` | required | Address to listen on, for example `0.0.0.0:2575` |
| `max_connections` | `100` | Concurrent connections; more are closed |
| `max_frame_len` | 16 MiB | Largest accepted message |
| `require_trailing_cr` | `false` | Reject frames whose end block is not followed by CR |

In original mode the sender gets `AA` once the message is stored, or `AE` when it could not be stored. In enhanced mode (MSH-15/16 valued) it gets `CA`/`CE` when MSH-15 asks for a commit acknowledgment (`NE` means none). Payloads that are not valid HL7 are stored for inspection and answered with `AR`. The acknowledgment's MSH-10 is the OXIM message identifier.

### Destination

| Setting | Default | Meaning |
|---|---|---|
| `target` | required | Receiver address, for example `10.0.0.20:2575` |
| `connect_timeout` | `10s` | Time to establish a connection |
| `ack_timeout` | `30s` | Time to wait for the acknowledgment |
| `ack` | `required` | `required`, or `none` for receivers that never acknowledge |
| `max_frame_len` | 16 MiB | Largest accepted acknowledgment |

The connection is reused and re-established after errors. `AA`/`CA` (or an MLLP release 2 commit ACK) delivers the message and the acknowledgment is stored as the response. `AE`/`CE`, timeouts and connection errors are retried; `AR`/`CR` fails the delivery. An acknowledgment whose MSA-2 does not match the sent MSH-10 is treated as a transport error.

## `tcp`: raw TCP with configurable framing

| `framing.mode` | Further settings | A message is |
|---|---|---|
| `delimited` | `end`, optional `start` | the bytes between `start` (if set) and `end` |
| `length_prefix` | `length_bytes`: 2 or 4 (default 4) | a big-endian length followed by that many bytes |
| `none` | | everything sent on one connection until it is closed |

### Source

| Setting | Default | Meaning |
|---|---|---|
| `listen` | required | Address to listen on |
| `framing` | required | See above |
| `response` | none | Bytes sent after each message is stored, for example `hex:06` |
| `error_response` | none | Bytes sent when a message could not be stored, for example `hex:15` |
| `max_connections` | `100` | Concurrent connections |
| `max_message_len` | 16 MiB | Largest accepted message |

### Destination

| Setting | Default | Meaning |
|---|---|---|
| `target` | required | Receiver address |
| `framing` | required | See above |
| `connect_timeout` | `10s` | Time to establish a connection |
| `response_timeout` | `30s` | Time to wait for a response |
| `wait_for_response` | `false` | Read one framed response after each message |
| `expected_response` | none | A response that differs fails the attempt (retried) |
| `max_message_len` | 16 MiB | Largest accepted response |

With framing `none`, each message uses its own connection and the response is everything the receiver sends before closing.

## `file`: directories

### Source

| Setting | Default | Meaning |
|---|---|---|
| `directory` | required | Directory to watch |
| `pattern` | `*` | File names to pick up (`*` and `?`, ASCII case-insensitive) |
| `poll_interval` | `5s` | Time between scans |
| `sort` | `name` | `name` or `modified` |
| `after` | `move` | `move` or `delete` |
| `processed_directory` | none | Where stored files go (required for `move`) |
| `error_directory` | none | Where files that are too large go |
| `max_file_size` | 64 MiB | Largest accepted file |

A file is picked up once its size and modification time are unchanged between two scans. Hidden files are ignored. Files are moved or deleted only after they were stored; a crash in between means the file is read again (at-least-once).

### Destination

| Setting | Default | Meaning |
|---|---|---|
| `directory` | required | Directory to write to |
| `filename` | `{channel}-{message_id}.{extension}` | Template with `{channel}`, `{message_id}`, `{destination}`, `{timestamp}`, `{extension}` |
| `overwrite` | `false` | Replace existing files |
| `create_directory` | `true` | Create the directory when missing |

Files are written to a hidden temporary file, flushed and renamed. Rewriting an identical file (after a retry) succeeds; a different existing file fails the delivery unless `overwrite` is set.

## `http`: HTTP and HTTPS requests

### Destination

| Setting | Default | Meaning |
|---|---|---|
| `url` | required | `http://` or `https://` endpoint |
| `method` | `POST` | `POST` or `PUT` |
| `headers` | none | Extra request headers |
| `content_type` | from the data type | `Content-Type` of the request |
| `timeout` | `30s` | Limit for the whole request |
| `ca_file` | none | PEM file with extra trusted certificate authorities |
| `max_response_size` | 16 MiB | Largest accepted response body |

HTTPS uses rustls with the operating system's trusted certificates plus `ca_file`. Every request carries an `X-OXIM-Message-Id` header. `2xx` delivers the message and the body is stored as the response; `408`, `429`, `3xx`, `5xx` and transport errors are retried; other `4xx` responses fail the delivery.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
