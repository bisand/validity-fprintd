#!/usr/bin/env bash
# Extract the sensor firmware from Lenovo's Windows driver installer.
#
# The firmware is proprietary and cannot be redistributed, so it has to be
# fetched from Lenovo. This downloads the installer, verifies its SHA-512, and
# extracts the one file the driver needs.
#
# You only need this if `validity-firmware` reports no firmware present. A
# sensor that has been used under Windows already has it.
set -euo pipefail

DEST="${DEST:-/usr/share/validity-fprintd}"

# Installer and firmware file name per USB id.
usb_id="$(lsusb | grep -oiE '(138a:0090|138a:0097|138a:009d|06cb:009a)' | head -1 || true)"
if [ -z "$usb_id" ]; then
    echo "No supported Validity sensor found on the USB bus." >&2
    exit 1
fi

case "$usb_id" in
  138a:0090)
    URL="https://download.lenovo.com/pccbbs/mobiles/n1cgn08w.exe"
    SHA512="d839fa65adf4c952ecb4a5c4b2fc5b5bdedd8e02a421564bdc7fae1d281be4ea26fcde2333f2ab78d56cef0fdccce0a3cf429300b89544cdc9cfee6d0fe0db55"
    FW="6_07f_Lenovo.xpfwext"
    ;;
  *)
    URL="https://download.lenovo.com/pccbbs/mobiles/nz3gf07w.exe"
    SHA512="a4a4e6058b1ea8ab721953d2cfd775a1e7bc589863d160e5ebbb90344858f147d695103677a8df0b2de0c95345df108bda97196245b067f45630038fb7c807cd"
    FW="6_07f_lenovo_mis_qm.xpfwext"
    ;;
esac

[ "$(id -u)" -eq 0 ] || { echo "run with sudo (writes to $DEST)"; exit 1; }
command -v innoextract >/dev/null || { echo "install innoextract first"; exit 1; }
command -v curl >/dev/null || { echo "install curl first"; exit 1; }

if [ -f "$DEST/$FW" ]; then
    echo "Firmware already present: $DEST/$FW"
    exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "Device      : $usb_id"
echo "Installer   : $URL"
echo "Downloading (this is Lenovo's Windows driver package)..."
curl -fSL --progress-bar -o "$WORK/driver.exe" "$URL"

echo "Verifying SHA-512..."
actual="$(sha512sum "$WORK/driver.exe" | cut -d' ' -f1)"
if [ "$actual" != "$SHA512" ]; then
    echo "CHECKSUM MISMATCH - refusing to use this download." >&2
    echo "  expected: $SHA512" >&2
    echo "  actual:   $actual" >&2
    exit 1
fi
echo "Checksum OK."

echo "Extracting..."
innoextract -s -d "$WORK/x" "$WORK/driver.exe" >/dev/null

found="$(find "$WORK/x" -name "$FW" -print -quit)"
if [ -z "$found" ]; then
    echo "Could not find $FW inside the installer." >&2
    exit 1
fi

install -Dm644 "$found" "$DEST/$FW"
echo
echo "Installed $DEST/$FW"
echo "Now run: sudo validity-firmware --upload"
