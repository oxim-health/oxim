# oxim-connectors

Source and destination connectors for [OXIM](../../README.md) channels. Call `oxim_connectors::register(&mut registry)` to make them available to channel configurations.

Every source stores a message durably before acknowledging it (ADR 0004): if storing fails, the device is told to keep the message and send it again.

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

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
