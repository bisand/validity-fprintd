#!/bin/sh
# validity-fprintd installer.
#
#   curl -fsSL https://raw.githubusercontent.com/bisand/validity-fprintd/main/install.sh | sudo sh
#
# Downloads the latest release binaries, verifies their checksum, and sets up
# the service for whichever init system is in use. Does not touch PAM; see the
# instructions it prints at the end.
#
# Environment:
#   VERSION=v0.2.0   install a specific release instead of the latest
#   PREFIX=/usr/local  where binaries go
#
# Written for POSIX sh so it works under busybox ash on Alpine.
set -eu

REPO="bisand/validity-fprintd"
PREFIX="${PREFIX:-/usr/local}"
BINDIR="$PREFIX/bin"
SHAREDIR="/usr/share/validity-fprintd"
TOOLS="validity-fprintd validity-probe validity-session validity-db validity-sensor validity-verify validity-baseline validity-firmware validity-provision"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need_root() {
    [ "$(id -u)" -eq 0 ] || die "run as root (try: curl ... | sudo sh)"
}

# --- environment detection -------------------------------------------------

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64-unknown-linux-musl" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-musl" ;;
        *) die "unsupported architecture: $(uname -m)" ;;
    esac
}

# systemd, openrc, or none. Determines how the daemon is supervised.
detect_init() {
    if [ -d /run/systemd/system ]; then
        echo systemd
    elif command -v rc-update >/dev/null 2>&1; then
        echo openrc
    else
        echo none
    fi
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        die "need curl or wget"
    fi
}

fetch_stdout() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$1"
    else
        die "need curl or wget"
    fi
}

latest_version() {
    # Ask the API for the newest release tag. Kept to sed so no jq is needed.
    fetch_stdout "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -1
}

# --- uninstall -------------------------------------------------------------

uninstall() {
    need_root
    say "Removing validity-fprintd..."

    case "$(detect_init)" in
        systemd)
            systemctl disable --now validity-fprintd.service 2>/dev/null || true
            rm -f /etc/systemd/system/validity-fprintd.service
            systemctl daemon-reload 2>/dev/null || true
            systemctl unmask fprintd.service 2>/dev/null || true
            ;;
        openrc)
            rc-service validity-fprintd stop 2>/dev/null || true
            rc-update del validity-fprintd default 2>/dev/null || true
            rm -f /etc/init.d/validity-fprintd
            ;;
    esac

    for t in $TOOLS; do rm -f "$BINDIR/$t"; done
    rm -f /etc/udev/rules.d/70-validity-fprintd.rules
    command -v udevadm >/dev/null 2>&1 && udevadm control --reload-rules 2>/dev/null || true

    say ""
    say "Removed. Fingerprint PAM rules were left alone; remove them if nothing"
    say "else provides pam_fprintd, or authentication will point at a missing"
    say "service. On Omarchy: omarchy-remove-security-fingerprint"
    say ""
    say "Sensor state (pairing, enrolments, calibration) lives on the sensor and"
    say "is untouched. Host cache: /var/lib/validity-fprintd"
    exit 0
}

[ "${1:-}" = "--uninstall" ] && uninstall

# --- install ---------------------------------------------------------------

need_root

ARCH="$(detect_arch)"
INIT="$(detect_init)"

VERSION="${VERSION:-$(latest_version)}"
[ -n "$VERSION" ] || die "could not determine the latest release; set VERSION=vX.Y.Z"

NAME="validity-fprintd-${VERSION#v}-${ARCH}"
URL="https://github.com/$REPO/releases/download/$VERSION/$NAME.tar.gz"

say "validity-fprintd installer"
say "  version : $VERSION"
say "  target  : $ARCH"
say "  init    : $INIT"
say ""

TMP="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$TMP'" EXIT INT TERM

say "Downloading $NAME.tar.gz"
fetch "$URL" "$TMP/pkg.tar.gz" || die "download failed: $URL"

say "Verifying checksum"
if fetch "https://github.com/$REPO/releases/download/$VERSION/SHA256SUMS" "$TMP/SHA256SUMS" 2>/dev/null; then
    EXPECTED="$(sed -n "s/^\([0-9a-f]\{64\}\)  *$NAME\.tar\.gz$/\1/p" "$TMP/SHA256SUMS" | head -1)"
    if [ -z "$EXPECTED" ]; then
        die "no checksum for $NAME.tar.gz in SHA256SUMS"
    fi
    ACTUAL="$(sha256sum "$TMP/pkg.tar.gz" | cut -d' ' -f1)"
    [ "$EXPECTED" = "$ACTUAL" ] || die "checksum mismatch: expected $EXPECTED, got $ACTUAL"
    say "  ok"
else
    die "could not fetch SHA256SUMS; refusing to install unverified binaries"
fi

tar -xzf "$TMP/pkg.tar.gz" -C "$TMP"
SRC="$TMP/$NAME"

say "Installing binaries to $BINDIR"
mkdir -p "$BINDIR"
for t in $TOOLS; do
    install -m 755 "$SRC/bin/$t" "$BINDIR/$t"
done

mkdir -p "$SHAREDIR"
install -m 755 "$SRC/share/fetch-firmware.sh" "$SHAREDIR/fetch-firmware.sh"

say "Installing udev rules"
mkdir -p /etc/udev/rules.d
install -m 644 "$SRC/share/70-validity-fprintd.rules" /etc/udev/rules.d/70-validity-fprintd.rules
command -v udevadm >/dev/null 2>&1 && udevadm control --reload-rules 2>/dev/null || true

case "$INIT" in
    systemd)
        say "Installing systemd unit"
        sed "s|/usr/local/bin/|$BINDIR/|" "$SRC/share/validity-fprintd.service" \
            > /etc/systemd/system/validity-fprintd.service
        chmod 644 /etc/systemd/system/validity-fprintd.service

        # Both claim net.reactivated.Fprint. Masking also blocks D-Bus activation.
        if systemctl list-unit-files fprintd.service >/dev/null 2>&1; then
            say "Masking fprintd (it claims the same D-Bus name)"
            systemctl stop fprintd.service 2>/dev/null || true
            systemctl mask fprintd.service 2>/dev/null || true
        fi

        systemctl daemon-reload
        systemctl enable validity-fprintd.service >/dev/null 2>&1 || true
        systemctl restart validity-fprintd.service
        ;;

    openrc)
        say "Installing OpenRC service"
        sed "s|/usr/local/bin/|$BINDIR/|" "$SRC/share/validity-fprintd.openrc" \
            > /etc/init.d/validity-fprintd
        chmod 755 /etc/init.d/validity-fprintd

        if [ -f /etc/init.d/fprintd ]; then
            say "Disabling fprintd (it claims the same D-Bus name)"
            rc-service fprintd stop 2>/dev/null || true
            rc-update del fprintd default 2>/dev/null || true
        fi

        rc-update add validity-fprintd default >/dev/null 2>&1 || true
        rc-service validity-fprintd restart
        ;;

    none)
        say "No supported init system found; the daemon is installed but not supervised."
        say "Start it manually with: $BINDIR/validity-fprintd"
        ;;
esac

say ""
say "Installed."
say ""
say "Check that your sensor is recognised:"
say "  sudo $BINDIR/validity-probe"
say ""
say "It reports one of three states. 'PAIRED TO THIS HOST' means you are ready."
say "'UNPAIRED' means the sensor needs provisioning; see the README section"
say "'Provisioning a bare sensor'."
say ""
say "To use a fingerprint for login and sudo, pam_fprintd must be present"
say "(package: fprintd) and referenced from /etc/pam.d. On Omarchy:"
say "  omarchy setup security fingerprint"
say "Elsewhere, add this above the password line in the relevant files:"
say "  auth sufficient pam_fprintd.so"
say "Keep it 'sufficient' so a failed finger falls through to your password."
say ""
say "Uninstall:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sudo sh -s -- --uninstall"
