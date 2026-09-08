# validity-fprintd

**Makes Synaptics/Validity match-on-chip fingerprint sensors work on Linux.**

These sensors ship in ThinkPads and other laptops, and stock `libfprint` has no
driver for them — so on every distribution `fprintd` says this:

```console
$ fprintd-list $USER
Impossible to list devices: GDBus.Error:net.reactivated.Fprint.Error.NoSuchDevice: No devices available
```

`validity-fprintd` is a Rust driver and a drop-in replacement for the `fprintd`
daemon. It speaks the sensor's own protocol, and serves the same D-Bus
interface, so `pam_fprintd`, GNOME and KDE settings, and the `fprintd-*` tools
all work unchanged — fingerprint login for sudo, polkit and the lock screen.

![validity-fprintd capturing a fingerprint and matching it on the sensor](docs/demo.gif)

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/bisand/validity-fprintd/main/install.sh | sudo sh
```

Installs the native package for your distribution — `.deb`, `.rpm`, `.apk` or
`.pkg.tar.zst` — so removal goes through your package manager. Where there is no
package it unpacks a tarball into `/usr/local` instead. Every download is
checked against the release's `SHA256SUMS`, and it refuses to install if that
cannot be fetched or does not match.

Then enable it, replacing the stock daemon — both claim the
`net.reactivated.Fprint` D-Bus name and cannot run together:

```sh
sudo systemctl mask --now fprintd.service
sudo systemctl enable --now validity-fprintd.service
```

Check that your sensor is recognised:

```sh
sudo systemctl stop validity-fprintd && sudo validity-probe; sudo systemctl start validity-fprintd
```

Uninstall with `curl -fsSL … | sudo sh -s -- --uninstall`, and pass
`FORCE_TARBALL=1` to skip the package where one applies. To read the script
before running it as root, download it first and run `sudo sh install.sh`.

### Fingerprint login

The driver serves the D-Bus interface; the PAM module itself comes from your
distribution's `fprintd` package:

| Distribution | Package providing `pam_fprintd` | Service manager |
|---|---|---|
| Arch / Omarchy | `fprintd` | systemd |
| Debian / Ubuntu | `libpam-fprintd` | systemd |
| Fedora | `fprintd-pam` | systemd |
| Alpine | `fprintd-pam` (community) | OpenRC |

Enrol a finger and wire up PAM:

```sh
sudo fprintd-enroll "$USER"          # or: omarchy setup security fingerprint
```

On Omarchy that one command enrols, verifies and writes the PAM configuration
for sudo, polkit and the lock screen. On Fedora, use
`sudo authselect enable-feature with-fingerprint`. Elsewhere, add
`auth sufficient pam_fprintd.so` above the password line in the relevant files
in `/etc/pam.d/`.

Keep it **`sufficient`**, not `required`, so a failed or unavailable fingerprint
falls through to your password.

### Will it work with my sensor?

| USB ID | Name | Tested |
|---|---|---|
| `06cb:009a` | Synaptics Metallica MIS | yes |
| `138a:0090` | Validity VFS7500 | no |
| `138a:0097` | Validity VFS7552 | no |
| `138a:009d` | Validity VFS7552 | no |

`validity-probe` reports one of three states:

- **Paired to this host** — ready to use. A sensor previously used with
  **Windows Hello**, or with python-validity, is already in this state: pairing
  keys derive from the machine's DMI identity, which is the same under either
  OS, so a Windows-paired sensor opens fine on the same laptop.
- **Unpaired** — factory-fresh or reset. It can be provisioned from scratch;
  see below.
- **Paired to another host** — the sensor came from a different machine. Using
  it needs a factory reset.

### Other ways to install

Native packages built from source link your distribution's own libusb, so a
libusb security fix arrives with your normal updates. The release packages
above bundle it instead, which is what lets one artifact work on every version
of a distribution family.

<details>
<summary>Arch, Fedora, Alpine and from-source builds</summary>

**Arch** — the `-git` PKGBUILD is in the repository, so no AUR account is
needed. It conflicts with `python-validity` and `open-fprintd`.

```sh
git clone https://github.com/bisand/validity-fprintd.git
cd validity-fprintd/packaging/aur && makepkg -si
```

**Fedora**

```sh
rpmbuild -ba validity-fprintd/packaging/fedora/validity-fprintd.spec
sudo dnf install ~/rpmbuild/RPMS/*/validity-fprintd-*.rpm
```

**Alpine** — the service is in the `validity-fprintd-openrc` subpackage.

```sh
cd validity-fprintd/packaging/alpine && abuild -r
```

**From source**

```sh
cargo build --release
sudo ./scripts/install.sh      # binaries, systemd unit, udev rules; masks fprintd
sudo ./scripts/uninstall.sh    # removal, including PAM rules
```

The Fedora and Alpine recipes have not been built by the author, who has only
Arch hardware. Corrections welcome.

</details>

## Provisioning a bare sensor

Only needed if `validity-probe` reports **unpaired**. The firmware blob is
proprietary and cannot be shipped, so it is extracted from Lenovo's Windows
driver installer. Fetch it first — the sensor cannot capture without it.

```sh
sudo systemctl stop validity-fprintd          # it holds the USB interface

sudo /usr/share/validity-fprintd/fetch-firmware.sh
sudo validity-provision --init-flash          # partition flash, pair to this machine
sudo validity-firmware --upload               # install the firmware extension
sudo validity-baseline --write                # capture the calibration baseline

sudo systemctl start validity-fprintd
sudo fprintd-enroll "$USER"
```

`validity-provision --factory-reset` returns a sensor to its bare state,
erasing the pairing record, every enrolment and the calibration baseline. It
requires a typed confirmation. There is no reason to run it on a working
sensor.

## Status

Verified against real hardware on a ThinkPad X1 Carbon 6th gen (`06cb:009a`).
The whole provisioning chain was exercised by factory-resetting a working
sensor and rebuilding it from nothing — partition table, firmware, pairing
record, calibration baseline and enrolments — ending in a working fingerprint
`sudo`.

| Capability | Status |
|---|---|
| USB transport, signed init handshake | Verified |
| Pairing-record parsing, host key derivation, sensor authentication | Verified |
| Encrypted session (the firmware's TLS 1.2 dialect) | Verified |
| Flash access, on-chip enrolment database (read and write) | Verified |
| Sensor identification, calibration, image capture | Verified |
| On-chip matching, enrolment, deletion | Verified |
| `fprintd`-compatible D-Bus daemon | Verified |
| Calibration baseline, firmware upload, flash partitioning, factory reset | Verified |
| Type-2 sensor capture path | **Untested** |
| aarch64 binaries | **Never executed** |

The type-2 capture path is transcribed from the reference implementation and
compiles, but no type-2 hardware was available. It also needs factory
calibration data (subtag 7), which type-1 sensors do not report.

## Warranty and risk

There is none: this is MIT-licensed and provided as is, without warranty of any
kind. See [LICENSE](LICENSE). Beyond the boilerplate, it is worth being
concrete, because most drivers do not do what this one does:

- It **writes to the sensor's flash** — firmware, the pairing record, the
  calibration baseline and the enrolment database. Those writes are how a bare
  sensor is made to work at all.
- `--factory-reset` **erases the pairing record, every enrolled fingerprint and
  the calibration baseline.** Recovery means reprovisioning, which needs the
  Lenovo firmware blob. Fetch it before you reset anything.
- Destructive operations sit behind an explicit flag and a typed confirmation.
  Nothing writes to the sensor unless you ask it to.
- Users are looked up in `/etc/passwd` directly, because the release binaries
  are statically linked and cannot use glibc's NSS. Accounts existing only in
  LDAP, SSSD or systemd-homed will not resolve.
- It has run on **one sensor, one machine, one distribution**. Other
  distributions are supported by construction rather than by testing.

A fingerprint is a convenience, not a stronger factor than the password behind
it. If you would rather not rely on it, leave PAM alone and use the CLI tools.

## Tools

Read-only unless noted. The daemon holds the USB interface, so stop it first:

```sh
sudo systemctl stop validity-fprintd
sudo validity-probe
sudo systemctl start validity-fprintd
```

| Tool | Shows |
|---|---|
| `validity-probe` | device, firmware, flash layout, pairing state |
| `validity-session` | opens an encrypted session, reads the partition table back |
| `validity-db` | storage objects, users and enrolled fingers |
| `validity-sensor` | sensor identity, geometry, capture-program selection |
| `validity-verify` | calibrates, captures a fingerprint, matches it on-chip |
| `validity-baseline` | inspects and verifies the calibration baseline |
| `validity-firmware` | inspects the firmware extension |
| `validity-provision` | reports provisioning state |

Writing variants, for unprovisioned sensors: `validity-baseline --write`,
`validity-firmware --upload`, `validity-provision --init-flash`, and
`validity-provision --factory-reset` (destructive).

Root is required for raw USB access and to read
`/sys/class/dmi/id/product_serial`, which the pairing keys derive from. Pass
`--trace` for a hex dump of the wire traffic.

<details>
<summary>Example output</summary>

```console
$ sudo validity-probe
Device      : 06cb:009a (Synaptics Metallica MIS)
Firmware    : v1.2, buildtime 0x5e2e83e9, 8 modules
Flash       : JEDEC 0x00ef:0x0040, 4096 blocks x 256 bytes
  partition 0x01 type 0x04 access 0x0007 @ 0x00001000 size 0x00001000
  partition 0x02 type 0x01 access 0x0002 @ 0x00002000 size 0x0003e000
  partition 0x04 type 0x03 access 0x0005 @ 0x00050000 size 0x00080000

Pairing record blocks:
  id 0x0004  161 bytes
  id 0x0003  184 bytes
  id 0x0006  400 bytes

Status      : PAIRED TO THIS HOST
  The sensor's private key decrypted and authenticated.
  A TLS session can be established without re-pairing.
```

Fingers are enrolled on the sensor itself, not on the host:

```console
$ sudo validity-db
Database    : 524288 bytes total, 71168 used, 313088 free, 15 records
Storage     : 'StgWindsor' (dbid 3), 1 user(s)

User 4 — identity S-1-5-21-…-1000
  finger dbid    6  subtype 0x03 (right-middle-finger)  23064 bytes
  finger dbid    9  subtype 0x02 (right-index-finger)   23064 bytes
```

Matching happens on the chip; the host never sees image data:

```console
$ sudo validity-verify
Sensor      : 57K0 FM-3367-001 (type 0x0199)
Baseline    : valid clean-slate image present on sensor flash
Calibrating (3 iterations)... 13440 bytes of calibration data

>>> Touch the fingerprint sensor now (30s timeout) <<<

Captured    : x=112 y=112 w1=333 w2=8

MATCH
  finger    : 0x02 (right-index-finger)
```

And through PAM, which is the point of the whole exercise:

```console
$ sudo -k && sudo ls
Place your finger on the fingerprint reader
Cargo.toml  LICENSE  README.md  packaging  scripts  src  udev

$ journalctl -u validity-fprintd -n 4 --no-pager
session: opened with 57K0 FM-3367-001 (type 0x0199)
calibration: loaded 13440 bytes from cache
verify: bisand matched right-index-finger
verify: verify-match after 1.9s
```

</details>

## How it works

These sensors do enrolment and matching on the chip itself; the host never
receives image data. The host talks to the sensor over bulk USB using a
protocol that looks like TLS 1.2 but deviates from it in ways the firmware
requires:

- The ClientHello extension-block length is written two bytes short.
- The Certificate message carries its length prefix twice, both times holding
  the bare certificate length rather than that of the enclosing structure.
- Record padding is `n-1`-filled rather than PKCS#7 — except the pairing blob,
  which *is* PKCS#7.
- The client Finished is excluded from the handshake transcript.
- ECDH coordinates in the pairing record are stored little-endian at fixed
  offsets.

A standards-conformant TLS implementation is rejected by the firmware. Each
deviation is commented at its site in `src/tls.rs`.

Session keys derive from a static ECDH key pair: the sensor's public point is
stored in flash and signed by Synaptics' firmware key, and the host's private
key is stored encrypted under a key derived from the machine's DMI
`product_name` and `product_serial`. That binding is why a sensor paired to one
machine will not open on another.

## Development

```sh
cargo build --release
```

<details>
<summary>Portable release binaries, and cutting a release</summary>

Release artifacts are static-pie musl binaries with libusb compiled in, so they
run on any distribution. To reproduce a build on Arch:

```sh
sudo pacman -S musl
rustup target add x86_64-unknown-linux-musl

CC_x86_64_unknown_linux_musl=musl-gcc \
CFLAGS_x86_64_unknown_linux_musl="-idirafter /usr/include" \
RUSTFLAGS="-C target-feature=+crt-static" \
cargo build --release --target x86_64-unknown-linux-musl --features vendored
```

`-idirafter` is needed because Arch's `musl-gcc` does not search the Linux uapi
headers that libusb includes; appending the path leaves musl's own headers
taking precedence.

Do **not** set `CARGO_TARGET_*_LINKER=musl-gcc`. That produces a dynamic PIE
which still needs `/lib/ld-musl-x86_64.so.1` at run time and fails on any host
without musl installed. Letting rustc link with its own self-contained CRT
objects produces a static-pie, which needs nothing and keeps ASLR. Verify:

```sh
readelf -l target/…/validity-fprintd | grep INTERP   # must print nothing
readelf -d target/…/validity-fprintd | grep NEEDED   # must print nothing
```

Releases build when a GitHub Release is **published**, not when a tag is
pushed, so the notes are written by hand. Tag and push, draft the release with
its notes, then publish it: CI builds both architectures, checks each binary is
self-contained, builds the packages, and attaches everything. Assets are
uploaded to the existing release, so the notes are never overwritten. To check
a build without publishing, run the workflow manually against an existing tag.

</details>

## Credit

The wire protocol is undocumented by the vendor. It was established by the
reverse-engineering work of [python-validity](https://github.com/uunicorn/python-validity)
(MIT), without which this project would not be feasible. This is an independent
Rust implementation, not a translation, but the protocol knowledge is theirs.

If you want a working fingerprint reader today rather than a driver project,
python-validity is mature and covers these sensors.

## License

MIT. See [LICENSE](LICENSE).
