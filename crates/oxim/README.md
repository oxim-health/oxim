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
oxim service install               # Windows service or systemd unit
oxim service start | stop | uninstall
oxim import mirth export.xml --out channels   # Mirth Connect channels + migration report
```

- **Configuration:** `oxim.yaml` sets the data, channel and table directories, logging (text or JSON, standard output or daily files), engine tuning, retention and live reload. Relative paths are resolved against the file's directory.
- **Live reload:** channel files are watched; new files are deployed, changed files redeployed, removed or disabled channels undeployed. A file that fails to parse keeps its previous deployment.
- **Retention:** contents of completed messages are pruned after `retention.contents_after` (default 90 days) and whole messages after `retention.messages_after` (default 365 days). Cached lab orders (`orders.db`, see `oxim-lab`) that have not changed for `retention.orders_after` (default 90 days) are deleted.
- **Migration:** `oxim import mirth` converts a Mirth Connect channel, channel group or server backup into channel files and writes `mirth-migration-report.md` (or JSON with `--report report.json`); `--value name=value` fills `${name}` placeholders, `--library` adds code template exports, `--dry-run` only prints. See `oxim-mirth`.
- **Services:** on Windows the program registers itself with the service control manager (automatic start); on Linux it writes and enables a hardened systemd unit.

## License

Licensed under either of Apache License, Version 2.0 or MIT license, at your option.
