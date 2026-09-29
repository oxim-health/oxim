# Installing OXIM

OXIM is a single program, `oxim`, with a local SQLite store. It runs fully on premises, needs no internet access and sends no telemetry. Pick the guide for your platform:

| Guide | Package | Service |
|---|---|---|
| [Linux](linux.md) | deb (Debian, Ubuntu) or rpm (RHEL, Rocky, Alma, Fedora, SUSE) | systemd |
| [Windows](windows.md) | MSI installer | Windows service |
| [Docker](docker.md) | OCI image `ghcr.io/oxim-health/oxim` | container |
| [Kubernetes](kubernetes.md) | Helm chart `deploy/helm/oxim` | StatefulSet |
| [Offline](offline.md) | `oxim-<version>-<target>.tar.gz` / `.zip` bundle for air-gapped networks | systemd or Windows service |

Release artifacts are attached to each [GitHub release](https://github.com/oxim-health/oxim/releases): packages, offline bundles, a CycloneDX SBOM, `SHA256SUMS` and its Sigstore signature `SHA256SUMS.sigstore.json`.

## Verifying a download

Check the checksum, then the signature of the checksum file:

```sh
sha256sum --check --ignore-missing SHA256SUMS
cosign verify-blob SHA256SUMS \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp '^https://github.com/oxim-health/oxim/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

Container images are signed the same way:

```sh
cosign verify ghcr.io/oxim-health/oxim:<version> \
  --certificate-identity-regexp '^https://github.com/oxim-health/oxim/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## Common layout

Every installation has the same parts:

| Part | Linux | Windows | Container |
|---|---|---|---|
| Configuration | `/etc/oxim/oxim.yaml` | `%ProgramData%\OXIM\oxim.yaml` | `/etc/oxim/oxim.yaml` |
| Channel files | `/etc/oxim/channels` | `%ProgramData%\OXIM\channels` | `/etc/oxim/channels` |
| Code and routing tables | `/etc/oxim/tables` | `%ProgramData%\OXIM\tables` | `/etc/oxim/tables` |
| Message database | `/var/lib/oxim` | `%ProgramData%\OXIM\data` | `/var/lib/oxim` (volume) |
| Logs | journal (`journalctl -u oxim`) | `%ProgramData%\OXIM\logs` | standard output |

`oxim.yaml` is described in [crates/oxim/README.md](../../crates/oxim/README.md). Channels are YAML files; examples ship with every package (`examples/channels`). A new installation has no active channel: copy an example without its `.example` suffix, adapt it, check it with `oxim validate` and start the service.

```sh
oxim -c /etc/oxim/oxim.yaml validate
```

Channel files are reloaded while OXIM runs; changes to `oxim.yaml` need a restart.

## Before going live

- The message database holds clinical data. Restrict access to the data directory (the packages do) and include it in backups.
- Retention (`retention` in `oxim.yaml`) deletes completed messages after 90 days (contents) and 365 days (records) by default. Set it to your legal and local requirements.
- Listeners bind to the addresses in the channel files. Bind to specific interfaces where possible and restrict ports with the host firewall.
