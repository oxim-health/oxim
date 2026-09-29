# oxim

The OXIM command-line program and service.

```text
oxim init --dir /etc/oxim          # starter oxim.yaml, channels/, tables/, data/
oxim validate -c /etc/oxim/oxim.yaml
oxim run -c /etc/oxim/oxim.yaml    # foreground; Ctrl+C or SIGTERM stops gracefully
oxim channels
oxim messages list --destination-status failed
oxim messages show <id> --stage encoded --destination lis
oxim messages requeue <id> lis
oxim users create-admin            # first administrator; the web server starts once a user exists
oxim users add nurse --role viewer # prompts for the password without echo (or --password-stdin)
oxim users list | disable <user> | enable <user> | passwd <user>
oxim tokens create prometheus --role viewer --expires-in 365d
oxim tokens list | revoke <id>
oxim profile validate profiles/generic-astm-analyzer/profile.yaml
oxim profile test profiles/generic-astm-analyzer/profile.yaml   # replay the profile's fixtures
oxim service install               # Windows service or systemd unit
oxim service start | stop | uninstall
oxim import mirth export.xml --out channels   # Mirth Connect channels + migration report
```

- **Configuration:** `oxim.yaml` sets the data, channel, table and script directories, logging (text or JSON, standard output or daily files), engine tuning, retention and live reload. Relative paths are resolved against the file's directory.
- **Live reload:** channel files are watched; new files are deployed, changed files redeployed, removed or disabled channels undeployed. A file that fails to parse keeps its previous deployment.
- **Retention:** contents of completed messages are pruned after `retention.contents_after` (default 90 days) and whole messages after `retention.messages_after` (default 365 days). Cached lab orders (`orders.db`, see `oxim-lab`) that have not changed for `retention.orders_after` (default 90 days) are deleted.
- **Alerts:** rules in the `alerts` section (queue depth and age, failed deliveries, error rate, device silence, disk space, certificate expiry) notify email, webhook, Teams, Slack, syslog or SNMP targets while `oxim run` runs; `oxim validate` checks them. See [`oxim-alert`](../oxim-alert/README.md).
- **Web server:** `oxim run` also serves the REST API, metrics and web UI ([oxim-server](../oxim-server)) on `server.listen` (default `127.0.0.1:8080`), with HTTPS when `server.tls` is set. Users and API tokens live in `data_dir/auth.db`; sessions time out after `server.session_idle` (30 minutes) or `server.session_max` (12 hours).
- **Migration:** `oxim import mirth` converts a Mirth Connect channel, channel group or server backup into channel files and writes `mirth-migration-report.md` (or JSON with `--report report.json`); `--value name=value` fills `${name}` placeholders, `--library` adds code template exports, `--dry-run` only prints. See `oxim-mirth`.
- **Device profiles:** `oxim profile test` runs each fixture of a profile through the profile's channel in an in-process engine (code tables resolve against the profile's directory) and prints a conformance report; `--bless` writes the actual normalized content to the expected files. See [`oxim-devices`](../oxim-devices/README.md).
- **Device registry:** channels with a `track-device` step record their devices in `devices.db` in the data directory.
- **Services:** on Windows the program registers itself with the service control manager (automatic start); on Linux it writes and enables a hardened systemd unit.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
