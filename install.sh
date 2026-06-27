#!/usr/bin/env bash
# Installer for scroll-accel (wheel + pointer acceleration).
# Build first (as your normal user):  cargo build --release
# Then:                                sudo bash install.sh
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$DIR/target/release/scroll-accel"
SCHEMA=org.gnome.desktop.peripherals.mouse

if [ "$(id -u)" -ne 0 ]; then
    echo "Run with sudo:  sudo bash $0" >&2
    exit 1
fi
if [ ! -x "$BIN" ]; then
    echo "Built binary not found at: $BIN" >&2
    echo "Build it first (as your normal user):  cd $DIR && cargo build --release" >&2
    exit 1
fi

echo "==> Stopping any existing service"
systemctl stop scroll-accel 2>/dev/null || true

echo "==> Installing binary -> /usr/local/bin/scroll-accel"
install -m 0755 "$BIN" /usr/local/bin/scroll-accel

echo "==> Syncing config -> /etc/scroll-accel.conf (from this project)"
if [ -f /etc/scroll-accel.conf ]; then
    cp -f /etc/scroll-accel.conf /etc/scroll-accel.conf.bak
    echo "    (previous /etc config backed up to /etc/scroll-accel.conf.bak)"
fi
install -m 0644 "$DIR/scroll-accel.conf" /etc/scroll-accel.conf

echo "==> Ensuring uinput loads at boot"
echo uinput > /etc/modules-load.d/uinput.conf
modprobe uinput || true

# --- Turn off GNOME's own mouse acceleration so it doesn't stack with ours. ---
# Must run as the logged-in user (gsettings is per-session), not root.
if [ -n "${SUDO_USER:-}" ] && command -v gsettings >/dev/null 2>&1; then
    UID_N="$(id -u "$SUDO_USER")"
    run_user() { sudo -u "$SUDO_USER" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$UID_N/bus" "$@"; }
    # Back up the user's current values once, so uninstall can restore them.
    if [ ! -f /etc/scroll-accel.gnome-backup ]; then
        {
            echo "PROFILE=$(run_user gsettings get $SCHEMA accel-profile 2>/dev/null)"
            echo "SPEED=$(run_user gsettings get $SCHEMA speed 2>/dev/null)"
        } > /etc/scroll-accel.gnome-backup 2>/dev/null || true
    fi
    if run_user gsettings set $SCHEMA accel-profile flat 2>/dev/null \
        && run_user gsettings set $SCHEMA speed 0.0 2>/dev/null; then
        echo "==> GNOME mouse accel set to flat (so only our curve applies)"
    else
        echo "==> NOTE: couldn't set gsettings automatically. Run as your user:"
        echo "       gsettings set $SCHEMA accel-profile flat"
        echo "       gsettings set $SCHEMA speed 0.0"
    fi
else
    echo "==> NOTE: run as your user to disable GNOME's own mouse accel:"
    echo "       gsettings set $SCHEMA accel-profile flat && gsettings set $SCHEMA speed 0.0"
fi

echo "==> Installing & starting service"
install -m 0644 "$DIR/scroll-accel.service" /etc/systemd/system/scroll-accel.service
systemctl daemon-reload
systemctl enable --now scroll-accel

echo
systemctl --no-pager --full status scroll-accel | sed -n '1,6p' || true
echo
echo "Done — wheel + pointer acceleration are live."
echo "Tune:       edit $DIR/scroll-accel.conf  then  sudo bash $0"
echo "Calibrate:  sudo systemctl stop scroll-accel && sudo /usr/local/bin/scroll-accel --debug"
echo "Uninstall:  sudo bash $DIR/uninstall.sh"
