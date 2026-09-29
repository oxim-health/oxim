# RPM %pre for OXIM: the service account must exist before files are
# installed. $1 is 1 on installation and 2 on upgrade. The same account is
# described in /usr/lib/sysusers.d/oxim.conf for systemd-sysusers.
getent group oxim >/dev/null || groupadd --system oxim
getent passwd oxim >/dev/null || useradd --system --gid oxim --home-dir /var/lib/oxim \
    --no-create-home --shell /sbin/nologin --comment "OXIM integration engine" oxim
# Serial analyzers are reached through /dev/tty* devices owned by dialout.
if getent group dialout >/dev/null; then
    usermod --append --groups dialout oxim || :
fi
exit 0
