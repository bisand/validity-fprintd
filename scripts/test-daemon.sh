#!/usr/bin/env bash
# Run validity-fprintd temporarily and exercise it with the stock fprintd
# client. Changes nothing permanently: the stock fprintd is restored on exit.
set -uo pipefail

BIN="$(cd "$(dirname "$0")/.." && pwd)/target/release/validity-fprintd"
[ -x "$BIN" ] || { echo "build first: cargo build --release"; exit 1; }
[ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }

TARGET_USER="${SUDO_USER:-root}"
DAEMON_PID=""

cleanup() {
    echo
    echo "--- cleaning up ---"
    [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null && wait "$DAEMON_PID" 2>/dev/null
    systemctl unmask fprintd.service 2>/dev/null
    echo "stock fprintd restored (it is socket/dbus activated, so nothing to start)"
}
trap cleanup EXIT

echo "--- stopping and masking stock fprintd for the duration of this test ---"
systemctl stop fprintd.service 2>/dev/null
systemctl mask fprintd.service 2>/dev/null

echo "--- starting validity-fprintd ---"
"$BIN" &
DAEMON_PID=$!
sleep 2

if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
    echo "daemon exited immediately; see output above"
    exit 1
fi

echo
echo "--- listing enrolled fingers for '$TARGET_USER' via D-Bus ---"
busctl --system call net.reactivated.Fprint \
    /net/reactivated/Fprint/Device/0 \
    net.reactivated.Fprint.Device ListEnrolledFingers s "$TARGET_USER"

echo
echo "--- running fprintd-verify (touch the sensor when prompted) ---"
if command -v fprintd-verify >/dev/null; then
    sudo -u "$TARGET_USER" fprintd-verify "$TARGET_USER"
else
    echo "fprintd-verify not installed; skipping"
fi
