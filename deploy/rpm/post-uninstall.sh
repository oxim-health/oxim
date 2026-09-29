# RPM %postun for OXIM. $1 is 0 on removal and 1 on upgrade.
#
# Removal keeps the message database, logs, channel files and the oxim
# account: the database holds clinical messages that retention rules or
# local policy may still require. Set OXIM_PURGE_DATA=yes to delete them,
# for example: sudo OXIM_PURGE_DATA=yes dnf remove oxim
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || :
fi
if [ "$1" -eq 0 ]; then
    if [ "${OXIM_PURGE_DATA:-no}" = "yes" ]; then
        rm -rf /var/lib/oxim /var/log/oxim /etc/oxim
    else
        echo "Kept /var/lib/oxim, /var/log/oxim and /etc/oxim (set OXIM_PURGE_DATA=yes to delete them)."
    fi
fi
exit 0
