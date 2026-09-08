#!/bin/sh
# Stop the daemon before its binary goes away.
set -e

if [ -d /run/systemd/system ]; then
    systemctl disable --now validity-fprintd.service >/dev/null 2>&1 || true
else
    rc-service validity-fprintd stop >/dev/null 2>&1 || true
    rc-update del validity-fprintd default >/dev/null 2>&1 || true
fi
exit 0
