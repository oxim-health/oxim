# oxim-alert

The alert engine of [OXIM](../../README.md). Rules watch the running engine and notify operators when something needs attention: a LIS that stopped accepting results, an analyzer that went quiet, a disk filling up or a certificate about to expire.

Alerts are configured in the `alerts` section of `oxim.yaml` and evaluated every `interval` by `oxim run`; `oxim validate` checks them.

```yaml
alerts:
  interval: 1m      # how often rules are evaluated
  repeat: 1h        # how often a firing alert is notified again (0s: once)
  targets:
    - {id: ops, type: teams, url: "https://example.webhook.office.com/..."}
    - {id: noc, type: syslog, address: "10.0.0.5:514", protocol: udp, min_severity: critical}
    - {id: mail, type: email, settings: {server: smtp.example.org, from: oxim@example.org, to: [lab-it@example.org]}}
  rules:
    - {id: lis-backlog, kind: queue_depth, destination: lis, above: 100, for: 5m}
    - {id: lis-stuck, kind: queue_age, destination: lis, older_than: 15m, severity: critical}
    - {id: gave-up, kind: failed_deliveries, above: 0}
    - {id: errors, kind: error_rate, above: 10, within: 15m}
    - {id: analyzers, kind: device_silence, severity: critical, targets: [ops, noc]}
    - {id: disk, kind: disk_space, below: "10%"}
    - {id: certificates, kind: certificate_expiry, files: [/etc/oxim/tls/lab-ca.pem], within: 30d}
```

## Rules

| `kind` | Settings | Fires when |
|---|---|---|
| `queue_depth` | `above`, optional `channel`, `destination` | more than `above` messages wait for a destination (queued, sending or retrying) |
| `queue_age` | `older_than`, optional `channel`, `destination` | the oldest waiting message of a destination is older than `older_than` |
| `failed_deliveries` | `above` (default 0), optional `channel`, `destination` | more than `above` deliveries were given up and wait for an operator |
| `error_rate` | `above`, `within`, optional `channel` | more than `above` messages received within `within` ended in error |
| `device_silence` | optional `device` | a device tracked with `track-device` and `silence_after` sent nothing for longer than its window |
| `disk_space` | `below` (`10%`, `5GiB`, `500MB` or bytes), optional `path` | less than `below` is free on the volume of the data directory (or `path`) |
| `certificate_expiry` | `within`, optional `files` | the web server certificate or a listed PEM file expires within `within`, has expired, or cannot be read |

Every rule also takes `severity` (`info`, `warning` (default) or `critical`), `for` (how long the condition must hold before the alert fires; default at once) and `targets` (target identifiers; all targets when omitted). An alert is notified when it fires, again every `repeat` while it holds, and once more when it resolves. Each breaching subject (a destination, a device, a volume, a file) is its own alert.

## Targets

| `type` | Settings | Sends |
|---|---|---|
| `log` | | a warning in the OXIM log |
| `webhook` | `url`, `headers`, `ca_file` | a JSON object (`rule`, `kind`, `subject`, `severity`, `state`, `summary`, `since`, `at`, `repeat`) |
| `teams` | `url` | an Adaptive Card to a Microsoft Teams incoming webhook or workflow |
| `slack` | `url` | a message to a Slack incoming webhook |
| `email` | `settings` of the `smtp` destination connector | a text email |
| `destination` | `connector`, `settings`, `format` (`text` or `json`) | the notification through any destination connector, for example `file` or `mllp` |
| `syslog` | `address`, `protocol` (`udp` or `tcp`), `facility` (default `local0`), `app_name` | an RFC 5424 message; TCP uses octet counting (RFC 6587) |
| `snmp` | `address`, `community_env`, `trap_oid` | an SNMPv2c trap: `sysUpTime`, `snmpTrapOID` and the rule, subject, state, severity and summary as `trap_oid.1` to `.5` |

Every target takes `min_severity` to receive only more serious alerts. Web, chat and email targets use the same connectors as channels, with their TLS settings and certificate checks; failed sends are retried three times in the background. The default `trap_oid` is in the IANA experimental arc; sites should use an OID from their own enterprise arc.

Notifications carry states, counts and names, never message contents, so no patient data leaves OXIM through alerts.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
