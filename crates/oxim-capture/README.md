# oxim-capture

Records what a device and its host exchange, for [OXIM](../../README.md) device work: evidence for device profiles, fixtures for `oxim profile test`, and reproducible bug reports.

```text
# The analyzer connects to port 5100 of this machine; OXIM (or the LIS) listens on 5101.
oxim-capture tcp --listen 0.0.0.0:5100 --target 127.0.0.1:5101 --protocol astm-lis01 --out chem-1.oximcap

# OXIM connects to the analyzer: point OXIM at this machine's port 5100 instead.
oxim-capture tcp --listen 0.0.0.0:5100 --target 10.0.0.21:5100 --device target --out chem-1.oximcap

# Serial: bridge the analyzer on COM3 to the host on COM4 (for example a virtual port pair).
oxim-capture serial --port COM3 --bridge COM4 --baud 9600 --protocol astm-lis01 --out chem-1.oximcap

# Serial: only listen, as on a monitoring cable.
oxim-capture serial --port /dev/ttyUSB0 --baud 19200 --out chem-1.oximcap

oxim-capture show chem-1.oximcap         # readable transcript with <STX>, <ACK>, ...
oxim-sim replay chem-1.oximcap --to 127.0.0.1:5101
```

Only traffic routed through the proxy (or bridged through it) is recorded. Watching a network without rerouting it is the job of the companion netKit project.

Captures contain whatever the device sent, including protected health information. Run `oxim-anonymize` on a capture before sharing it or committing it to a repository.

## The `.oximcap` format

JSON Lines: a header line, then one line per event. Bytes are stored exactly as they were read, in standard base64.

```text
{"format":"oximcap","version":1,"created_at":"2026-09-29T09:00:00Z","tool":"oxim-capture 0.0.0","transport":"tcp","protocol":"astm-lis01","device":"client of 0.0.0.0:5100","host":"127.0.0.1:5101"}
{"timestamp":"2026-09-29T09:00:00.01Z","connection":1,"event":"open","transport":"tcp","peer":"10.0.0.21:50211"}
{"timestamp":"2026-09-29T09:00:00.02Z","connection":1,"event":"data","direction":"device-to-host","transport":"tcp","peer":"10.0.0.21:50211","data":"BQ=="}
{"timestamp":"2026-09-29T09:00:00.03Z","connection":1,"event":"data","direction":"host-to-device","transport":"tcp","peer":"10.0.0.21:50211","data":"Bg=="}
{"timestamp":"2026-09-29T09:00:01Z","connection":1,"event":"close","transport":"tcp","peer":"10.0.0.21:50211"}
```

| Header field | Meaning |
|---|---|
| `format`, `version` | `oximcap`, `1` |
| `created_at` | RFC 3339 time the capture started |
| `tool` | the program that wrote it |
| `transport` | `tcp`, `serial` or `other` |
| `protocol` | `hl7v2-mllp`, `astm-lis01`, `astm-raw`, `poct1a` or `raw`; optional |
| `device`, `host` | where each side was |
| `description` | free text |
| `anonymized` | `true` once `oxim-anonymize` processed the capture |

| Record field | Meaning |
|---|---|
| `timestamp` | RFC 3339 time the bytes were seen |
| `connection` | connection number from 1; a capture may hold several connections |
| `event` | `open`, `data` or `close` |
| `direction` | `device-to-host` or `host-to-device` (data records) |
| `transport`, `peer` | transport and remote address or port name |
| `data` | the bytes, base64 (data records) |

Readers ignore unknown fields. Every record is flushed as it is written, so a capture is usable even if the recorder is killed.

## Replay

`oxim_capture::replay` (and `oxim-sim replay`) plays the device side: it sends the device's bytes as recorded and, wherever the capture shows the host answering, waits for the host to answer before continuing. An answer is complete at a LIS01 control character or frame end (ASTM), an MLLP end block (HL7), a closed document (POCT1-A), after as many bytes as recorded (other protocols), or after 300 ms of silence. Answers are collected, not compared: timestamps and control identifiers legitimately differ. Connections are replayed one after another.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
