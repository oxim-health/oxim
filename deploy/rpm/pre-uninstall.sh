# RPM %preun for OXIM. $1 is 0 on removal and 1 on upgrade.
if [ "$1" -eq 0 ] && [ -d /run/systemd/system ]; then
    systemctl disable --now oxim.service >/dev/null 2>&1 || :
fi
exit 0
