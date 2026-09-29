# oxim-shadow

Shadow mode for [OXIM](../../README.md): check an OXIM channel against a running Mirth Connect installation without touching a single message.

The traffic of a Mirth channel is captured passively, then:

1. the messages the devices sent to Mirth are taken from the capture;
2. they run through the OXIM channel in an isolated in-process engine, where the destinations only record what they would send;
3. OXIM's output is compared, field by field, with what Mirth sent to each destination (and with Mirth's replies, when the channel answers requests).

```text
analyzer ──▶ Mirth :6661 ──▶ LIS :6662        (captured)
               │                   │
  inbound messages           Mirth's output
               ▼                   │
       OXIM channel (replay) ──▶ compare ──▶ report
```

Nothing is sent anywhere during a shadow run: the channel's connectors are replaced, only its filters, transformers, encoders and replies run.

## Capturing

- **PCAP/PCAPNG** from the companion [netKit](https://github.com/TNYCL/netKit) project (`netkit capture --output mirth.pcapng --filter "port 6661 or port 6662"`), tcpdump or Wireshark, taken on the Mirth host or a mirror port. Ethernet (with VLAN tags), Linux cooked, raw IP and loopback captures over IPv4 and IPv6 are read; TCP streams are reassembled, with retransmissions removed.
- **`.oximcap`** from `oxim-capture tcp` placed in front of Mirth: one capture between the devices and Mirth, and one per destination between Mirth and the receiver.

TLS-encrypted traffic cannot be compared from a capture; capture on the plain side (for example between a TLS terminator and Mirth).

## Running

```text
oxim shadow --channel channels/results-to-lis.yaml --capture mirth.pcapng \
    --inbound-port 6661 --destination lis=6662 \
    --ignore MSH-7 --ignore MSH-10 --report shadow.md
```

| Option | Meaning |
|---|---|
| `--channel` | The OXIM channel file (for example one written by `oxim import mirth`) |
| `--capture` | A PCAP/PCAPNG file, or an `.oximcap` file of the inbound side |
| `--inbound-port` | The port the Mirth channel listens on |
| `--destination id=port` | An OXIM destination and the port Mirth sent its messages to; with `.oximcap`, `id=file.oximcap` (repeatable) |
| `--framing mllp\|astm\|astm-raw` | Framing of the inbound side (`id=port:astm` for a destination) |
| `--ignore path` | Paths ignored in the comparison; `MSH-7` and `MSH-10` by default |
| `--key path` | Pair messages by this path (for example `OBR-3`) instead of by order |
| `--show-values` | Show the differing values in the report (they may be patient data) |
| `--report file` | Markdown, or JSON when the name ends in `.json` |

The exit status is 0 when OXIM produced the same messages as Mirth, 1 when they differ.

## Comparison

HL7 v2 messages are compared field by field with segment occurrences (`PID-5`, `OBX[2]-5`), ASTM messages record by record (`R-4`, `R[3]-4`), other data line by line. Ignored paths are cleared on both sides before comparing. Messages are paired in capture order per destination, or by a key path. The report counts identical and different pairs and messages missing on either side, and lists the differing fields; their values are hidden by default.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
