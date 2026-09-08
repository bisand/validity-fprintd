#!/bin/sh
# validity-fprintd installer.
#
#   curl -fsSL https://raw.githubusercontent.com/bisand/validity-fprintd/main/install.sh | sudo sh
#
# Needs a terminal for sudo to prompt on. Without one, download the script
# first and run `sudo sh install.sh`.
#
# Installs the native package for your distribution where there is one, so
# removal goes through your package manager, and falls back to a tarball
# otherwise. Every download is checked against the release's SHA256SUMS.
#
# Environment:
#   VERSION=v0.2.0     install a specific release instead of the latest
#   PREFIX=/usr/local  where the tarball fallback puts binaries
#   FORCE_TARBALL=1    skip the native package even if one applies
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

# Which package format this system actually installs, or "tar" for none.
detect_pkg_fmt() {
    [ "${FORCE_TARBALL:-}" = "1" ] && { echo tar; return; }
    if command -v apk >/dev/null 2>&1; then
        echo apk
    elif command -v pacman >/dev/null 2>&1; then
        echo archlinux
    elif command -v dpkg >/dev/null 2>&1; then
        echo deb
    elif command -v rpm >/dev/null 2>&1; then
        echo rpm
    else
        echo tar
    fi
}

# Architecture as each packaging format spells it.
pkg_arch() {
    case "$1" in
        deb) case "$(uname -m)" in x86_64|amd64) echo amd64 ;; *) echo arm64 ;; esac ;;
        *)   case "$(uname -m)" in x86_64|amd64) echo x86_64 ;; *) echo aarch64 ;; esac ;;
    esac
}

# systemd, openrc, or none. Only used by the tarball path.
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

    # Prefer the package manager, in case this was a package install.
    removed=""
    if command -v apk >/dev/null 2>&1 && apk info -e validity-fprintd >/dev/null 2>&1; then
        apk del validity-fprintd && removed=apk
    elif command -v dpkg >/dev/null 2>&1 && dpkg -s validity-fprintd >/dev/null 2>&1; then
        if command -v apt-get >/dev/null 2>&1; then
            apt-get -y remove validity-fprintd && removed=deb
        else
            dpkg -r validity-fprintd && removed=deb
        fi
    elif command -v pacman >/dev/null 2>&1 && pacman -Qi validity-fprintd >/dev/null 2>&1; then
        pacman -R --noconfirm validity-fprintd && removed=pacman
    elif command -v rpm >/dev/null 2>&1 && rpm -q validity-fprintd >/dev/null 2>&1; then
        if command -v dnf >/dev/null 2>&1; then
            dnf -y remove validity-fprintd && removed=rpm
        else
            rpm -e validity-fprintd && removed=rpm
        fi
    fi

    if [ -n "$removed" ]; then
        say "Removed the $removed package."
    else
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
        say "Removed the files installed from the tarball."
    fi

    say ""
    say "Fingerprint PAM rules were left alone; remove them if nothing else"
    say "provides pam_fprintd, or authentication will point at a missing"
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
FMT="$(detect_pkg_fmt)"

VERSION="${VERSION:-$(latest_version)}"
[ -n "$VERSION" ] || die "could not determine the latest release; set VERSION=vX.Y.Z"

BASE="https://github.com/$REPO/releases/download/$VERSION"

TMP="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$TMP'" EXIT INT TERM

# SHA256SUMS lists every asset, so asset names are read from it rather than
# guessed. That keeps this script working if a packaging tool changes how it
# names its output.
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" 2>/dev/null \
    || die "could not fetch SHA256SUMS for $VERSION; refusing to install unverified binaries"

# Name of the first asset matching an extension and an architecture token.
find_asset() {
    _ext="$1"; _arch="$2"
    sed -n "s/^[0-9a-f]\{64\} \{1,\}\(.*${_arch}.*\.${_ext}\)$/\1/p" "$TMP/SHA256SUMS" | head -1
}

verify() {
    _file="$1"; _name="$2"
    _expected="$(sed -n "s/^\([0-9a-f]\{64\}\) \{1,\}${_name}$/\1/p" "$TMP/SHA256SUMS" | head -1)"
    [ -n "$_expected" ] || die "no checksum recorded for $_name"
    _actual="$(sha256sum "$_file" | cut -d' ' -f1)"
    [ "$_expected" = "$_actual" ] || die "checksum mismatch for $_name"
}

ASSET=""
if [ "$FMT" != "tar" ]; then
    # Arch packages end in .pkg.tar.zst rather than a bare format name.
    case "$FMT" in
        archlinux) EXT="pkg.tar.zst" ;;
        *)         EXT="$FMT" ;;
    esac
    ASSET="$(find_asset "$EXT" "$(pkg_arch "$FMT")")"
    # No package published for this format or architecture; use the tarball.
    [ -n "$ASSET" ] || FMT=tar
fi

say "validity-fprintd installer"
say "  version : $VERSION"
say "  target  : $ARCH"
if [ "$FMT" = "tar" ]; then
    say "  method  : tarball into $BINDIR"
else
    say "  method  : $FMT package"
fi
say ""

if [ "$FMT" != "tar" ]; then
    # --- native package ---
    say "Downloading $ASSET"
    fetch "$BASE/$ASSET" "$TMP/$ASSET" || die "download failed: $BASE/$ASSET"
    say "Verifying checksum"
    verify "$TMP/$ASSET" "$ASSET"
    say "  ok"

    say "Installing"
    case "$FMT" in
        deb)
            dpkg -i "$TMP/$ASSET" || {
                command -v apt-get >/dev/null 2>&1 \
                    && apt-get -y -f install \
                    || die "dpkg failed and apt-get is unavailable to fix dependencies"
            }
            ;;
        rpm)
            if command -v dnf >/dev/null 2>&1; then
                dnf -y install "$TMP/$ASSET"
            else
                rpm -Uvh --replacepkgs "$TMP/$ASSET"
            fi
            ;;
        apk)
            # nfpm does not sign its output, so the key is not in apk's keyring.
            apk add --allow-untrusted "$TMP/$ASSET"
            ;;
        archlinux)
            pacman -U --noconfirm "$TMP/$ASSET"
            ;;
    esac

    say ""
    say "Installed. The package printed how to enable it above."
    say "Uninstall:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sudo sh -s -- --uninstall"
    exit 0
fi

# --- tarball fallback ------------------------------------------------------

INIT="$(detect_init)"
NAME="validity-fprintd-${VERSION#v}-${ARCH}"

say "Downloading $NAME.tar.gz"
fetch "$BASE/$NAME.tar.gz" "$TMP/pkg.tar.gz" || die "download failed: $BASE/$NAME.tar.gz"
say "Verifying checksum"
verify "$TMP/pkg.tar.gz" "$NAME.tar.gz"
say "  ok"

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
say "To use a fingerprint for login and sudo, pam_fprintd must be present and"
say "referenced from /etc/pam.d. Install it with your package manager:"
say "  Arch    pacman -S fprintd        Debian/Ubuntu  apt install libpam-fprintd"
say "  Fedora  dnf install fprintd-pam  Alpine         apk add fprintd-pam"
say ""
say "On Omarchy:"
say "  omarchy setup security fingerprint"
say "Elsewhere, add this above the password line in the relevant files:"
say "  auth sufficient pam_fprintd.so"
say "Keep it 'sufficient' so a failed finger falls through to your password."
say ""
say "Uninstall:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sudo sh -s -- --uninstall"
