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
| `tls` | none | TLS listener settings; see [TLS](#tls) |

In original mode the sender gets `AA` once the message is stored, or `AE` when it could not be stored. In enhanced mode (MSH-15/16 valued) it gets `CA`/`CE` when MSH-15 asks for a commit acknowledgment (`NE` means none). Payloads that are not valid HL7 are stored for inspection and answered with `AR`. The acknowledgment's MSH-10 is the OXIM message identifier.

### Destination

| Setting | Default | Meaning |
|---|---|---|
| `target` | required | Receiver address, for example `10.0.0.20:2575` |
| `connect_timeout` | `10s` | Time to establish a connection |
| `ack_timeout` | `30s` | Time to wait for the acknowledgment |
| `ack` | `required` | `required`, or `none` for receivers that never acknowledge |
| `max_frame_len` | 16 MiB | Largest accepted acknowledgment |
| `tls` | none | TLS sender settings (`tls: {}` for the defaults); see [TLS](#tls) |

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
| `tls` | none | TLS listener settings; see [TLS](#tls) |

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
| `tls` | none | TLS sender settings (`tls: {}` for the defaults); see [TLS](#tls) |

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

## `http`: HTTP and HTTPS

### Source

| Setting | Default | Meaning |
|---|---|---|
| `listen` | required | Address to listen on, for example `0.0.0.0:8081` |
| `path` | `/` | Accepted path prefix; other paths get `404` |
| `methods` | `[POST, PUT]` | Accepted methods; others get `405` |
| `max_body` | 16 MiB | Largest accepted body; larger bodies get `413` |
| `max_connections` | `100` | Concurrent connections |
| `status` | `200` | Status of the answer once a message is stored (2xx) |
| `metadata_headers` | none | Request headers recorded as `http.header.<name>` metadata |
| `auth` | none | `{type: basic, username, password_env}` or `{type: bearer, token_env}`; inline `password`/`token` are accepted but discouraged |
| `tls` | none | TLS listener settings; see [TLS](#tls) |

The request body is the message. Once it is stored the client gets `status` with `{"message_id": "..."}` and an `X-OXIM-Message-Id` header; when it cannot be stored the client gets `503` and should retry. A channel with `source.response` answers with the reply as the response body (with the media type of its data type); without a reply the JSON answer carries the processing status, with `500` when processing failed. `X-Correlation-ID` becomes the message's correlation identifier; `http.method`, `http.path` and `http.content_type` are recorded as metadata. Credentials are compared in constant time.

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

## Laboratory connectors

### `astm-tcp` (source and destination)

ASTM E1381/E1394 (CLSI LIS01/LIS02) over TCP. As a source, OXIM plays the LIS01 host: it acknowledges every frame, but the frame that completes a message (the one carrying the `L` terminator record) is acknowledged only after the message is stored; if storing fails, that frame is refused with NAK and the analyzer retransmits it. As a destination, it sends messages such as worklists and succeeds when the analyzer acknowledged every frame.

An analyzer sends results and receives worklists over one half-duplex link, so an `astm-tcp` source and destination with the same endpoint share one connection. Connectors that share a link must use the same link settings (everything except `send_timeout`); a conflicting configuration is rejected at deployment. Only one source can use a link at a time. In server mode, a new connection replaces the current one, so an analyzer that reconnects after a network failure is served immediately.

| Setting | Default | Meaning |
|---|---|---|
| `mode` | `server` | `server` listens, `client` connects |
| `listen` | — | Address to listen on in server mode, e.g. `0.0.0.0:5100` |
| `connect` | — | Address to connect to in client mode |
| `role` | `host` | LIS01 role: `host` (yields on contention) or `instrument` |
| `reconnect_delay` | `1s` | First delay before reconnecting or listening again |
| `max_reconnect_delay` | `60s` | Upper bound of the exponential reconnect delay |
| `confirmation_timeout` | `10s` | How long the final acknowledgment waits for storage; must be below the 15 s LIS01 reply timeout, after which the frame is refused |
| `send_timeout` | `120s` | Destination only: how long a delivery may take, including waiting for the analyzer to connect |
| `max_message_len` | 16 MiB | Largest message accepted or sent |

### `astm-serial` (source and destination)

The same as `astm-tcp` over a serial port (RS-232/RS-485 or a USB adapter). A source and destination on the same port share it.

| Setting | Default | Meaning |
|---|---|---|
| `port` | — | Port name, e.g. `COM3` or `/dev/ttyUSB0` |
| `baud_rate` | `9600` | Baud rate |
| `data_bits` | `8` | 5 to 8 |
| `parity` | `none` | `none`, `odd` or `even` |
| `stop_bits` | `1` | 1 or 2 |
| `flow_control` | `none` | `none`, `software` (XON/XOFF) or `hardware` (RTS/CTS) |
| `role` | `host` | As for `astm-tcp` |
| `reopen_delay` | `1s` | First delay before reopening the port after a failure |
| `max_reopen_delay` | `60s` | Upper bound of the reopen delay |
| `confirmation_timeout` | `10s` | As for `astm-tcp` |
| `send_timeout` | `120s` | As for `astm-tcp` |
| `max_message_len` | 16 MiB | As for `astm-tcp` |

### `astm-raw-tcp` (source)

ASTM E1394 records over TCP without LIS01 framing: messages run from the header (`H`) to the terminator (`L`) record. Several analyzers may connect in server mode.

| Setting | Default | Meaning |
|---|---|---|
| `mode` | `server` | `server` or `client` |
| `listen` / `connect` | — | As for `astm-tcp` |
| `reconnect_delay`, `max_reconnect_delay` | `1s`, `60s` | Client mode reconnect backoff |
| `acknowledge` | `false` | Answer each message with ACK (0x06) after storing it, or NAK (0x15) if storing failed |
| `max_message_len` | 16 MiB | Largest message accepted |

### `poct1a` (source)

CLSI POCT1-A point-of-care devices over TCP. OXIM listens and plays the observation reviewer (data manager) for each connected device: it acknowledges hello and status messages, requests new observations when the status announces them, and acknowledges each observation (`OBS`) or event (`EVS`) message with `AA` only after storing it, or `AE` if storing failed. Device identifiers from the hello are stored as metadata (`poct1a.device_id`, `poct1a.vendor_id`, `poct1a.model_id`, `poct1a.serial_id`).

| Setting | Default | Meaning |
|---|---|---|
| `listen` | — | Address to listen on, e.g. `0.0.0.0:5200` |
| `hello_timeout` | `60s` | Wait for the device's hello after it connects |
| `response_timeout` | `30s` | Wait for the device to acknowledge a message |
| `idle_timeout` | `300s` | End the conversation after this long without traffic; `null` disables it |
| `keep_alive_interval` | off | Send keep-alive messages while idle (not every device supports them) |
| `request_observations` | `true` | Request observations when the status announces new ones |
| `version_id` | `POCT1` | `HDR.version_id` of OXIM's messages |
| `max_document_len` | 4 MiB | Largest XML document accepted |

## Licensing note

Serial port support uses the `tokio-serial` (MIT) and `serialport` (MPL-2.0) crates. MPL-2.0 is a file-level copyleft; `serialport` is used unmodified, so OXIM itself remains MIT OR Apache-2.0.

## TLS

`mllp` and `tcp` sources and destinations accept a `tls` block (rustls with the ring provider; TLS 1.2 and 1.3; certificates are always verified).

Listener:

| Setting | Default | Meaning |
|---|---|---|
| `cert_file` | required | PEM certificate chain, server certificate first |
| `key_file` | required | PEM private key (PKCS #8, PKCS #1 or SEC1) |
| `client_ca_file` | none | PEM certificate authorities for client certificates; enables mutual TLS |
| `require_client_cert` | `true` | With `client_ca_file`: reject clients without a certificate |
| `handshake_timeout` | `10s` | Limit for the TLS handshake |

Sender:

| Setting | Default | Meaning |
|---|---|---|
| `ca_file` | none | PEM certificate authorities trusted in addition to the system's |
| `system_roots` | `true` | Whether to trust the operating system's certificate authorities |
| `cert_file`, `key_file` | none | Client certificate and key for mutual TLS |
| `server_name` | host of `target` | Name the server certificate must match |

```yaml
source:
  type: mllp
  data_type: hl7v2
  settings:
    listen: 0.0.0.0:2575
    tls:
      cert_file: /etc/oxim/tls/oxim.pem
      key_file: /etc/oxim/tls/oxim-key.pem
      client_ca_file: /etc/oxim/tls/lab-ca.pem   # mutual TLS
```

Certificate files are read when the channel is deployed, so a bad path or key fails the deployment. Use absolute paths: relative paths depend on the working directory of the process.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
