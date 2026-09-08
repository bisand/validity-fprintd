#!/bin/sh
# Runs after install on deb, rpm and apk. Deliberately does not enable anything:
# starting the daemon means disabling the distribution's own fprintd, which a
# package should not do behind the user's back.
set -e

command -v udevadm >/dev/null 2>&1 && udevadm control --reload-rules 2>/dev/null || true

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
    _enable="  sudo systemctl mask --now fprintd.service
  sudo systemctl enable --now validity-fprintd.service"
    _check="  sudo systemctl stop validity-fprintd && sudo validity-probe"
else
    _enable="  sudo rc-update del fprintd default
  sudo rc-update add validity-fprintd default && sudo rc-service validity-fprintd start"
    _check="  sudo rc-service validity-fprintd stop && sudo validity-probe"
fi

cat <<MSG

validity-fprintd is installed but not yet running.

It serves the same D-Bus name as fprintd (net.reactivated.Fprint), so the two
cannot run together:

$_enable

Then check that your sensor is recognised:

$_check

'PAIRED TO THIS HOST' means you are ready. 'UNPAIRED' means the sensor needs
provisioning; see "Provisioning a bare sensor" in
/usr/share/doc/validity-fprintd/README.md. A sensor previously used with
Windows Hello is already provisioned.

For fingerprint login, install pam_fprintd (libpam-fprintd on Debian/Ubuntu,
fprintd-pam on Fedora and Alpine) and reference it from /etc/pam.d, keeping it
"sufficient" so a failed finger falls through to your password.

MSG
exit 0
