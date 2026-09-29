#!/bin/sh
# Removes OXIM installed by install.sh from this bundle.
#
#   sudo ./uninstall.sh          remove the program and the service
#   sudo ./uninstall.sh --purge  also delete /etc/oxim, /var/lib/oxim and
#                                /var/log/oxim, including stored messages
#
# The oxim account is kept.
set -eu

purge=no
for argument in "$@"; do
    case "$argument" in
        --purge) purge=yes ;;
        -h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $argument" >&2; exit 2 ;;
    esac
done
if [ "$(id -u)" -ne 0 ]; then
    echo "Run as root, for example: sudo $0" >&2
    exit 1
fi

if [ -d /run/systemd/system ]; then
    systemctl disable --now oxim.service 2>/dev/null || :
fi
rm -f /etc/systemd/system/oxim.service /etc/sysusers.d/oxim.conf /etc/tmpfiles.d/oxim.conf
rm -rf /etc/systemd/system/oxim.service.d /usr/local/share/doc/oxim
rm -f /usr/local/bin/oxim
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi

if [ "$purge" = yes ]; then
    rm -rf /etc/oxim /var/lib/oxim /var/log/oxim
    echo "OXIM, its configuration and its data are removed."
else
    echo "OXIM is removed. Kept /etc/oxim, /var/lib/oxim and /var/log/oxim (use --purge to delete them)."
fi
