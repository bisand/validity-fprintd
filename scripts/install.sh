#!/usr/bin/env bash
# Install validity-fprintd as the system fingerprint daemon.
#
# Reversible with scripts/uninstall.sh. Does not touch PAM: run
# `omarchy setup security fingerprint` afterwards to configure authentication.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/validity-fprintd"

[ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }
[ -x "$BIN" ] || { echo "build first: cargo build --release"; exit 1; }

echo "--- installing binary ---"
install -Dm755 "$BIN" /usr/local/bin/validity-fprintd
for tool in validity-probe validity-session validity-db validity-sensor validity-verify; do
    [ -x "$ROOT/target/release/$tool" ] && install -Dm755 "$ROOT/target/release/$tool" "/usr/local/bin/$tool"
done

# Remove binaries from before the project was renamed.
for old in vfs-probe vfs-session vfs-db vfs-sensor vfs-verify; do
    rm -f "/usr/local/bin/$old"
done
rm -f /etc/udev/rules.d/70-validity-rs.rules
# The calibration cache moved with the rename; it is rebuilt on first use.
rm -rf /var/lib/validity-rs

echo "--- installing udev rules ---"
install -Dm644 "$ROOT/udev/70-validity-fprintd.rules" /etc/udev/rules.d/70-validity-fprintd.rules
udevadm control --reload-rules 2>/dev/null || true

echo "--- installing systemd unit ---"
install -Dm644 "$ROOT/packaging/validity-fprintd.service" /etc/systemd/system/validity-fprintd.service

echo "--- masking stock fprintd ---"
# Both claim net.reactivated.Fprint. Masking also blocks D-Bus activation,
# which would otherwise start fprintd behind our back.
systemctl stop fprintd.service 2>/dev/null || true
systemctl mask fprintd.service

echo "--- starting validity-fprintd ---"
systemctl daemon-reload
systemctl enable validity-fprintd.service
# Restart rather than start, so reinstalling picks up a rebuilt binary.
systemctl restart validity-fprintd.service

sleep 2
echo
if systemctl is-active --quiet validity-fprintd.service; then
    echo "validity-fprintd is running."
    echo
    busctl --system call net.reactivated.Fprint \
        /net/reactivated/Fprint/Manager \
        net.reactivated.Fprint.Manager GetDefaultDevice \
        && echo "D-Bus interface responding."
else
    echo "validity-fprintd failed to start. Logs:"
    journalctl -u validity-fprintd.service -n 30 --no-pager
    exit 1
fi

echo
echo "Installed. To configure authentication, run:"
echo "    omarchy setup security fingerprint"
