#!/bin/sh
# Leave PAM alone: removing rules that another provider might rely on is worse
# than leaving them, and the user is told either way.
set -e

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi

cat <<'MSG'

validity-fprintd removed.

If you masked or disabled fprintd for it, restore it:
  sudo systemctl unmask fprintd.service      # systemd
  sudo rc-update add fprintd default         # OpenRC

Fingerprint PAM rules were left untouched. Remove them if nothing else provides
pam_fprintd, or authentication will point at a service that is gone.

Sensor state (pairing, enrolments, calibration) lives on the sensor itself and
is unaffected. Host-side calibration cache: /var/lib/validity-fprintd

MSG
exit 0
