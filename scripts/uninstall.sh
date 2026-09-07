#!/usr/bin/env bash
# Remove validity-fprintd and restore the stock fprintd.
#
# Also removes fingerprint PAM rules if omarchy added them, so authentication
# does not point at a daemon that is no longer there.
set -uo pipefail

[ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }

echo "--- stopping validity-fprintd ---"
systemctl disable --now validity-fprintd.service 2>/dev/null
rm -f /etc/systemd/system/validity-fprintd.service
systemctl daemon-reload

# Remove binaries from before the project was renamed.
for old in vfs-probe vfs-session vfs-db vfs-sensor vfs-verify; do
    rm -f "/usr/local/bin/$old"
done
rm -f /etc/udev/rules.d/70-validity-rs.rules

echo "--- removing binaries and rules ---"
rm -f /usr/local/bin/validity-fprintd
for tool in validity-probe validity-session validity-db validity-sensor validity-verify; do
    rm -f "/usr/local/bin/$tool"
done
rm -f /etc/udev/rules.d/70-validity-fprintd.rules
udevadm control --reload-rules 2>/dev/null || true

echo "--- removing fingerprint PAM rules ---"
if command -v omarchy-remove-security-fingerprint >/dev/null; then
    omarchy-remove-security-fingerprint || true
else
    # Fall back to stripping the lines omarchy would have added.
    for f in /etc/pam.d/sudo /etc/pam.d/polkit-1; do
        [ -f "$f" ] || continue
        sed -i '/pam_fprintd\.so/d;/omarchy-hw-laptop-closed/d' "$f"
    done
    rm -f /etc/pam.d/omarchy-lock-fingerprint
fi

echo "--- restoring stock fprintd ---"
systemctl unmask fprintd.service

echo
echo "Removed. Password authentication is unaffected."
echo "Calibration cache left at /var/lib/validity-fprintd (delete it if you want a clean slate)."
