#!/bin/sh
# Installs or upgrades OXIM from this offline bundle on Linux with systemd.
# No network access is needed.
#
#   sudo ./install.sh            install, enable the service, do not start it
#   sudo ./install.sh --start    also start (or restart) the service
#   sudo ./install.sh --no-enable
#
# Layout: /usr/local/bin/oxim, /etc/oxim (configuration, channels, tables),
# /var/lib/oxim (data), /usr/local/share/doc/oxim (guides and examples).
# An existing /etc/oxim/oxim.yaml and all channel files are kept. Do not mix
# this with the deb or rpm package on the same machine.
set -eu

enable=yes
start=no
for argument in "$@"; do
    case "$argument" in
        --start) start=yes ;;
        --no-enable) enable=no ;;
        -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $argument" >&2; exit 2 ;;
    esac
done

here="$(cd "$(dirname "$0")" && pwd)"
if [ "$(uname -s)" != Linux ]; then
    echo "This installer supports Linux with systemd. On other systems run" >&2
    echo "'$here/bin/oxim run -c <config>' under the local service manager." >&2
    exit 1
fi
if [ "$(id -u)" -ne 0 ]; then
    echo "Run as root, for example: sudo $0" >&2
    exit 1
fi

echo "Verifying the bundle"
(cd "$here" && sha256sum --quiet --check SHA256SUMS)

echo "Creating the oxim account and directories"
getent group oxim >/dev/null || groupadd --system oxim
getent passwd oxim >/dev/null || useradd --system --gid oxim --home-dir /var/lib/oxim \
    --no-create-home --shell /usr/sbin/nologin --comment "OXIM integration engine" oxim
if getent group dialout >/dev/null; then
    usermod --append --groups dialout oxim
fi
install -d -m 0750 -o root -g oxim /etc/oxim /etc/oxim/channels /etc/oxim/tables
install -d -m 0750 -o oxim -g oxim /var/lib/oxim /var/log/oxim

was_active=no
if [ -d /run/systemd/system ] && systemctl is-active --quiet oxim.service; then
    was_active=yes
fi

echo "Installing /usr/local/bin/oxim"
install -m 0755 "$here/bin/oxim" /usr/local/bin/oxim.new
mv -f /usr/local/bin/oxim.new /usr/local/bin/oxim

if [ -e /etc/oxim/oxim.yaml ]; then
    echo "Keeping /etc/oxim/oxim.yaml"
else
    install -m 0640 -o root -g oxim "$here/config/oxim.yaml" /etc/oxim/oxim.yaml
fi

install -d -m 0755 /usr/local/share/doc/oxim
cp -R "$here/docs" "$here/examples" /usr/local/share/doc/oxim/
cp "$here/README.md" "$here/LICENSE-MIT" "$here/LICENSE-APACHE" "$here/VERSION" /usr/local/share/doc/oxim/

echo "Installing the systemd unit"
# The unit comes from the binary itself, so it always matches this version.
/usr/local/bin/oxim -c /etc/oxim/oxim.yaml service unit > /etc/systemd/system/oxim.service
install -d -m 0755 /etc/systemd/system/oxim.service.d
install -m 0644 "$here/systemd/oxim.service.d/10-package.conf" /etc/systemd/system/oxim.service.d/
install -m 0644 "$here/systemd/sysusers.d/oxim.conf" /etc/sysusers.d/oxim.conf 2>/dev/null || :
install -m 0644 "$here/systemd/tmpfiles.d/oxim.conf" /etc/tmpfiles.d/oxim.conf 2>/dev/null || :

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    if [ "$enable" = yes ]; then
        systemctl enable oxim.service
    fi
    if [ "$start" = yes ] || [ "$was_active" = yes ]; then
        systemctl restart oxim.service
    fi
fi

echo
echo "OXIM $(cat "$here/VERSION") is installed."
echo "Add channel files to /etc/oxim/channels (examples: /usr/local/share/doc/oxim/examples),"
echo "check them with 'oxim -c /etc/oxim/oxim.yaml validate' and start the service with"
echo "'systemctl start oxim'."
