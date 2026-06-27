#!/usr/bin/env bash
# Uninstaller for scroll-accel. Run with: sudo bash uninstall.sh
set -euo pipefail
SCHEMA=org.gnome.desktop.peripherals.mouse

if [ "$(id -u)" -ne 0 ]; then
    echo "Run with sudo:  sudo bash $0" >&2
    exit 1
fi

systemctl disable --now scroll-accel 2>/dev/null || true
rm -f /etc/systemd/system/scroll-accel.service
systemctl daemon-reload
rm -f /usr/local/bin/scroll-accel
rm -f /etc/modules-load.d/uinput.conf

# Restore the GNOME mouse acceleration settings we changed at install time.
if [ -n "${SUDO_USER:-}" ] && [ -f /etc/scroll-accel.gnome-backup ] && command -v gsettings >/dev/null 2>&1; then
    UID_N="$(id -u "$SUDO_USER")"
    run_user() { sudo -u "$SUDO_USER" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$UID_N/bus" "$@"; }
    # shellcheck disable=SC1091
    . /etc/scroll-accel.gnome-backup
    run_user gsettings set $SCHEMA accel-profile "${PROFILE:-default}" 2>/dev/null || true
    [ -n "${SPEED:-}" ] && run_user gsettings set $SCHEMA speed "$SPEED" 2>/dev/null || true
    rm -f /etc/scroll-accel.gnome-backup
    echo "Restored GNOME mouse acceleration to its previous values."
fi

echo "Removed daemon, service, and uinput autoload."
echo "Left in place: /etc/scroll-accel.conf (delete manually if you want)."
