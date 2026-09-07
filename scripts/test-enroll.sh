#!/usr/bin/env bash
# Enrol a NEW finger through the daemon, leaving existing enrolments alone.
#
# This is the first operation that writes to the sensor. It writes only to the
# record database (partition 4), never to firmware, and it adds a record rather
# than modifying existing ones.
set -uo pipefail

FINGER="${FINGER:-left-index-finger}"
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
    echo "stock fprintd restored"
}
trap cleanup EXIT

echo "--- stopping and masking stock fprintd for the duration of this test ---"
systemctl stop fprintd.service 2>/dev/null
systemctl mask fprintd.service 2>/dev/null

echo "--- starting validity-fprintd ---"
"$BIN" &
DAEMON_PID=$!
sleep 2
kill -0 "$DAEMON_PID" 2>/dev/null || { echo "daemon exited immediately"; exit 1; }

echo
echo "--- fingers enrolled before ---"
sudo -u "$TARGET_USER" fprintd-list "$TARGET_USER" 2>&1 | tail -5

echo
echo "--- enrolling '$FINGER' (touch the sensor repeatedly when prompted) ---"
sudo -u "$TARGET_USER" fprintd-enroll -f "$FINGER" "$TARGET_USER"

echo
echo "--- fingers enrolled after ---"
sudo -u "$TARGET_USER" fprintd-list "$TARGET_USER" 2>&1 | tail -6

echo
echo "--- verifying the newly enrolled finger ---"
sudo -u "$TARGET_USER" fprintd-verify -f "$FINGER" "$TARGET_USER"
