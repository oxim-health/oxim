# RPM %post for OXIM. $1 is 1 on installation and 2 on upgrade.
if command -v systemd-tmpfiles >/dev/null 2>&1; then
    systemd-tmpfiles --create oxim.conf || :
else
    install -d -m 0750 -o root -g oxim /etc/oxim /etc/oxim/channels /etc/oxim/tables
    install -d -m 0750 -o oxim -g oxim /var/lib/oxim /var/log/oxim
fi
# The configuration may hold credentials: only the service group reads it.
chown root:oxim /etc/oxim/oxim.yaml
chmod 0640 /etc/oxim/oxim.yaml
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || :
fi
if [ "$1" -eq 1 ]; then
    # Enable, but do not start before channels are configured.
    systemctl enable oxim.service >/dev/null 2>&1 || :
    echo "OXIM is installed and enabled. Add channel files to /etc/oxim/channels,"
    echo "check them with 'oxim validate -c /etc/oxim/oxim.yaml' and start the"
    echo "service with 'systemctl start oxim'."
elif [ -d /run/systemd/system ]; then
    systemctl try-restart oxim.service >/dev/null 2>&1 || :
fi
exit 0
