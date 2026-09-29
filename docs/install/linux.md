# Installing OXIM on Linux

OXIM runs as the systemd service `oxim` under the unprivileged account `oxim`. Packages exist for x86_64 and aarch64 and need glibc 2.28 or later (Debian 10, Ubuntu 20.04, RHEL 8 and newer). For machines without network access, use the [offline bundle](offline.md).

## Install

Debian and Ubuntu:

```sh
sudo apt install ./oxim_<version>-1_amd64.deb
```

RHEL, Rocky Linux, AlmaLinux, Fedora and SUSE:

```sh
sudo dnf install ./oxim-<version>-1.x86_64.rpm
```

The package:

- creates the system account `oxim` (member of `dialout` for serial analyzers);
- installs `/usr/bin/oxim`, the configuration `/etc/oxim/oxim.yaml` and the systemd unit `oxim.service`;
- creates `/etc/oxim/channels`, `/etc/oxim/tables`, `/var/lib/oxim` and `/var/log/oxim`, readable only by `root` and the `oxim` group;
- enables the service but does not start it, because no channel is configured yet.

Examples and these guides are in `/usr/share/doc/oxim`.

## Configure and start

```sh
sudo cp /usr/share/doc/oxim/examples/channels/mllp-archive.yaml.example /etc/oxim/channels/mllp-archive.yaml
sudo oxim -c /etc/oxim/oxim.yaml validate
sudo systemctl start oxim
systemctl status oxim
journalctl -u oxim -f
```

Tables referenced by channels (code tables, routing tables) go to `/etc/oxim/tables`, scripts to `/etc/oxim/scripts`. Channel files are picked up while the service runs.

## First user

The web UI and REST API ([oxim-server](../../crates/oxim-server)) start once a user exists. Create the first administrator; the password is asked for without echo (or read from standard input with `--password-stdin`):

```sh
sudo -u oxim oxim -c /etc/oxim/oxim.yaml users create-admin --username admin
sudo systemctl restart oxim
```

The server listens on `127.0.0.1:8080` by default. For access from other machines set `server.listen` (for example `0.0.0.0:8443`) together with `server.tls`, or put a reverse proxy with TLS in front of it.

## The service

The unit `/usr/lib/systemd/system/oxim.service` is exactly what `oxim service unit` prints. The drop-in `/usr/lib/systemd/system/oxim.service.d/10-package.conf` adds the packaged layout and hardening: the working directory and state directory `/var/lib/oxim`, the `dialout` group, and read-only `/usr`, `/boot`, `/etc` and home directories for the service.

Relative paths in channel files, such as the directory of a file destination, resolve against `/var/lib/oxim`. A file connector that writes to another directory needs that directory to be writable by the `oxim` account; below `/usr`, `/boot`, `/etc` or `/home` it must also be allowed in the unit:

```sh
sudo systemctl edit oxim
```

```ini
[Service]
ReadWritePaths=/home/lab/export
```

Serial analyzers need the device to be accessible to the `dialout` group, which is the default for `/dev/ttyS*` and `/dev/ttyUSB*`. A stable name for a USB adapter can be set with a udev rule.

## Upgrade

Install the new package over the old one. The configuration and channel files are kept (`/etc/oxim/oxim.yaml` is a configuration file; if you changed it, the package manager keeps your version and stores the new default next to it). A running service is restarted with the new version.

Back up `/var/lib/oxim` before upgrading. With the service stopped, copying the directory is enough.

## Remove

```sh
sudo apt remove oxim        # or: sudo dnf remove oxim
```

Removing, and on Debian also purging, stops and disables the service but keeps `/etc/oxim`, `/var/lib/oxim`, `/var/log/oxim` and the `oxim` account, because the database holds clinical messages. To delete them as well:

```sh
sudo OXIM_PURGE_DATA=yes apt purge oxim
sudo OXIM_PURGE_DATA=yes dnf remove oxim
```

## Without a package

`oxim service install` registers the running binary as `/etc/systemd/system/oxim.service` with a given configuration:

```sh
sudo useradd --system --home-dir /var/lib/oxim oxim
sudo oxim init --dir /etc/oxim
sudo oxim -c /etc/oxim/oxim.yaml service install
```

On other Unix systems, run `oxim -c <config> run` under the local service manager.
