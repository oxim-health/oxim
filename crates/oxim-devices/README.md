# oxim-devices

The device connectivity toolkit of [OXIM](../../README.md): device profiles, the device registry and profile conformance tests.

## Device profiles

A profile describes one device model in a versioned YAML file (`profile_version: 1`): vendor, model and firmware range; transport, protocol and dialect; the OXIM channel that talks to the device; default code tables; known quirks; a setup guide; a verification level with its evidence; and fixtures, recorded sessions with their expected outcome. See [`profiles/`](../../profiles) for examples and `src/profile.rs` for the complete schema.

```yaml
profile_version: 1
id: generic-astm-analyzer
vendor: Generic
model: ASTM LIS01/LIS02 analyzer
connection: {transport: tcp, protocol: astm-lis01, device_role: client}
channel:
  source: {type: astm-tcp, data_type: astm, normalize: true, settings: {listen: 0.0.0.0:5100}}
  transformers:
    - {type: map-observations, table: tables/device-to-lis.csv}
verification:
  level: simulated
  evidence: [{note: Passes the recorded fixtures.}]
fixtures:
  - name: two-result-messages
    capture: fixtures/results.oximcap
    expect: {messages: 2, status: completed, normalized: fixtures/results.expected.json}
```

`Profile::load` checks everything the schema cannot express and reports each problem with its location (`fixtures[1].capture: ...`): the channel's source type must match the transport and protocol, referenced files must exist, code tables need `from` and `to` columns, and the verification level must be backed:

| Level | Meaning | Required |
|---|---|---|
| `unverified` | the profile exists | nothing |
| `documented` | based on the vendor's interface specification | evidence |
| `simulated` | passes recorded or specification-based simulated sessions | fixtures |
| `lab-verified` | verified against a real device in a test environment | fixtures and dated evidence |
| `field-verified` | verified in production use at a real site | fixtures and dated evidence |

## Conformance tests

```text
oxim profile validate profile.yaml
oxim profile test profile.yaml            # PASS/FAIL per fixture, with the first difference
oxim profile test profile.yaml --bless    # write the actual normalized content as expected
```

Each fixture runs in a fresh in-process engine with an in-memory store. A capture fixture (recorded with `oxim-capture`) is replayed over TCP against the channel's real source on a free local port; an `astm-serial` source is tested as `astm-tcp` with the same link settings. A message fixture submits files directly to the channel. Destinations are removed for the test unless the source relays a destination's response. The run then checks the message count, the final status (without one, no message may end in error), the normalized content (the first difference is reported as a JSON pointer), text in the replies to the device, and answers the replayed device waited for but did not get.

## Device registry

```yaml
transformers:
  - {type: track-device, device: chem-1, silence_after: 30m}
```

The `track-device` step records every message of a channel in `devices.db` in the data directory: first and last message, message count, and what the device reported about itself (ASTM H-5 name, version and serial number, HL7 MSH-3, POCT1-A hello data). `DeviceEnvironment::snapshot` lists every device with its status: `online`, `silent` (nothing within `silence_after`) or `never_seen` (configured but no message yet); `silent` lists the devices a silence alarm should report. The step never changes or rejects a message; if the registry cannot be written it logs a warning and the message continues.

## Tools

- [`oxim-capture`](../oxim-capture/README.md) records device conversations through a TCP proxy or serial bridge into `.oximcap` files.
- `oxim-sim replay` replays the device side of a capture against a host.
- [`oxim-anonymize`](../oxim-anonymize/README.md) removes protected health information from messages and captures before they become fixtures.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
