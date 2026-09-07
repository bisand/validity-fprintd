# validity-fprintd

A Rust driver for Synaptics/Validity **match-on-chip** fingerprint sensors — the
family found in ThinkPads and other laptops that stock `libfprint` does not
support, so `fprintd` reports *"No devices available"* on every distribution.

Developed against a ThinkPad X1 Carbon 6th gen (`06cb:009a`, "Metallica MIS").

## What it looks like

Before, with stock `libfprint` — the sensor is on the bus, but no driver claims it:

```console
$ fprintd-list $USER
Impossible to list devices: GDBus.Error:net.reactivated.Fprint.Error.NoSuchDevice: No devices available
```

After. The sensor identifies itself, and its pairing record decrypts against
this machine:

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

Host        : 20KH003BMX / serial ****6GM6
Status      : PAIRED TO THIS HOST
  The sensor's private key decrypted and authenticated.
  A TLS session can be established without re-pairing.
```

Fingers are enrolled on the sensor itself, not on the host:

```console
$ sudo validity-db
Database    : 524288 bytes total, 71168 used, 313088 free, 15 records
Storage     : 'StgWindsor' (dbid 3), 1 user(s)

User 4 — identity S-1-5-21-111111111-1111111111-1111111111-1000
  finger dbid    6  subtype 0x03 (right-middle-finger)  23064 bytes
  finger dbid    8  subtype 0x07 (left-index-finger)  23080 bytes
  finger dbid    9  subtype 0x02 (right-index-finger)  23064 bytes
```

Matching happens on the chip; the host never sees image data:

```console
$ sudo validity-verify
Sensor      : 57K0 FM-3367-001 (type 0x0199)
Enrolled    : 3 finger(s) across 1 user(s)
Baseline    : valid clean-slate image present on sensor flash
Calibrating (3 iterations)... 13440 bytes of calibration data

>>> Touch the fingerprint sensor now (30s timeout) <<<

Captured    : x=112 y=112 w1=333 w2=8

MATCH
  user dbid : 4
  finger    : 0x02 (right-index-finger)
  identity  : S-1-5-21-111111111-1111111111-1111111111-1000
```

And through PAM, which is the point of the whole exercise:

```console
$ sudo -k && sudo ls
Place your finger on the fingerprint reader
Cargo.toml  LICENSE  README.md  packaging  scripts  src  udev

$ journalctl -u validity-fprintd -n 5 --no-pager
session: opened with 57K0 FM-3367-001 (type 0x0199)
calibration: loaded 13440 bytes from cache
claim: bisand
verify: bisand matched right-index-finger
verify: verify-match after 1.9s
```

## Status

Working and verified against real hardware on a ThinkPad X1 Carbon 6th gen
(`06cb:009a`). It provisions a bare sensor, enrols, verifies, and authenticates
sudo, polkit and the lock screen through the stock `pam_fprintd`.

| Capability | Status |
|---|---|
| USB transport, signed init handshake | Verified |
| Pairing-record parsing, host key derivation, sensor authentication | Verified |
| Encrypted session (the firmware's TLS 1.2 dialect) | Verified |
| Flash access, on-chip enrolment database (read and write) | Verified |
| Sensor identification, calibration, image capture | Verified |
| On-chip matching, enrolment, deletion | Verified |
| `fprintd`-compatible D-Bus daemon | Verified |
| Calibration baseline: read, verify, encode, write | Verified |
| Firmware extension upload | Verified |
| Flash partitioning and pairing (`--init-flash`) | Verified |
| Factory reset | Verified |
| Type-2 sensor capture path | **Untested** |

The whole provisioning chain was exercised by factory-resetting a working
sensor and rebuilding it from nothing: partition table, firmware, pairing
record, calibration baseline and enrolments, ending in a working fingerprint
`sudo`. Every step is reproducible with the tools below.

The type-2 capture path remains untested because no type-2 hardware was
available. It is transcribed from the reference implementation and compiles,
but has never run. Type-2 sensors will also refuse to capture unless the
sensor reports factory calibration data (subtag 7), which type-1 sensors do
not provide.

### Which sensors this works with

- A sensor already provisioned — used with **Windows Hello** or python-validity
  — works directly. Pairing keys derive from the machine's DMI identity, which
  is identical under either OS, so a Windows-paired sensor opens fine on the
  same laptop.
- A **factory-fresh or reset** sensor can be provisioned from scratch; see
  below. This needs the firmware blob from Lenovo's driver installer.

Only sensor type `0x199` has been exercised on hardware.

## Provisioning a bare sensor

Needed only if `validity-provision` reports unformatted flash or an unpaired
sensor. Get the firmware first, since the sensor cannot capture without it:

```sh
sudo ./scripts/fetch-firmware.sh              # download and extract from Lenovo
sudo validity-provision --init-flash          # partition flash, pair to this machine
sudo validity-firmware --upload               # install the firmware extension
sudo validity-baseline --write                # capture the calibration baseline
omarchy setup security fingerprint            # enrol and configure PAM
```

Stop the daemon first (`sudo systemctl stop validity-fprintd`), since it holds
the USB interface.

`validity-provision --factory-reset` returns a sensor to its bare state. It
erases the pairing record, every enrolment and the calibration baseline, and
requires a typed confirmation. There is no reason to run it on a working
sensor.

## Supported devices

| USB ID | Name |
|---|---|
| `138a:0090` | Validity VFS7500 |
| `138a:0097` | Validity VFS7552 |
| `138a:009d` | Validity VFS7552 |
| `06cb:009a` | Synaptics Metallica MIS |

Only `06cb:009a` has been tested on hardware.

## Building

```sh
cargo build --release
```

## Installing

### Quick install (any distribution)

```sh
curl -fsSL https://raw.githubusercontent.com/bisand/validity-fprintd/main/install.sh -o install.sh
less install.sh          # it runs as root; read it first
sudo sh install.sh
```

Downloads the latest release, verifies its SHA-256, installs the binaries and
udev rules, and sets up the service for systemd or OpenRC. It refuses to
install if the checksum cannot be fetched or does not match. Uninstall with
`sudo sh install.sh --uninstall`.

Piping straight into `sudo sh` also works, but only from an interactive
terminal: `sudo` cannot prompt for a password when its standard input is the
script being piped in.

Release binaries are statically linked against musl with libusb built in, so
they do not depend on the host's libc or libusb version.

| Distribution | Daemon | Login via PAM | Package providing `pam_fprintd` |
|---|---|---|---|
| Arch / Omarchy | systemd | yes | `fprintd` |
| Ubuntu / Debian | systemd | yes | `libpam-fprintd` |
| Fedora | systemd | yes | `fprintd-pam` |
| Alpine | OpenRC | yes | `fprintd-pam` (community) |

This driver replaces `fprintd` itself; the package above is still needed for
`pam_fprintd.so` and the `fprintd-*` client tools. The installer masks or
disables the conflicting `fprintd` service, since both claim the
`net.reactivated.Fprint` D-Bus name.

Only `06cb:009a` has been tested on hardware, on Arch. Other distributions are
supported by construction rather than by testing.

### Building the Arch package

The `-git` PKGBUILD lives in this repository, so no AUR account is needed:

```sh
git clone https://github.com/bisand/validity-fprintd.git
cd validity-fprintd/packaging/aur
makepkg -si
```

Then enable it. Masking `fprintd` is required — both claim the
`net.reactivated.Fprint` D-Bus name, and masking also stops D-Bus activating
it:

```sh
sudo systemctl mask --now fprintd.service
sudo systemctl enable --now validity-fprintd.service
```

The package conflicts with `python-validity` and `open-fprintd` for the same
reason.

An AUR submission (`validity-fprintd-git`) is planned; AUR account registration
is paused upstream at the time of writing.

### From source

```sh
cargo build --release
sudo ./scripts/install.sh
```

This installs the daemon and CLI tools, adds a systemd unit and udev rules,
and masks the stock `fprintd.service` — both claim the `net.reactivated.Fprint`
bus name, so they cannot run together. It does not touch PAM.

To configure authentication on Omarchy:

```sh
omarchy setup security fingerprint
```

That enrols a finger, verifies it, and writes the PAM configuration for sudo,
polkit and the lock screen. On other distributions, add
`auth sufficient pam_fprintd.so` to the relevant files in `/etc/pam.d/`.

Removal, including any PAM rules:

```sh
sudo ./scripts/uninstall.sh
```

## Building portable release binaries

Release artifacts are static-pie musl binaries with libusb compiled in, so they
run on any distribution. To reproduce a build locally on Arch:

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

Do **not** set `CARGO_TARGET_*_LINKER=musl-gcc`. Doing so produces a dynamic
PIE that still requires `/lib/ld-musl-x86_64.so.1` at run time and therefore
fails on any host without musl installed. Letting rustc link with its own
self-contained CRT objects produces a static-pie instead, which needs nothing
and keeps ASLR. Verify with:

```sh
readelf -l target/.../validity-fprintd | grep INTERP   # must print nothing
readelf -d target/.../validity-fprintd | grep NEEDED   # must print nothing
```

## Tools

Read-only unless noted.

```sh
sudo validity-probe      # device, firmware, flash layout and pairing state
sudo validity-session    # open an encrypted session, read the partition table back
sudo validity-db         # storage objects, users and enrolled fingers
sudo validity-sensor     # sensor identity, geometry, capture-program selection
sudo validity-verify     # calibrate, capture a fingerprint and match it on-chip
sudo validity-baseline   # inspect and verify the calibration baseline
sudo validity-firmware   # inspect the firmware extension
sudo validity-provision  # report provisioning state
```

The daemon holds the USB interface, so stop it first:

```sh
sudo systemctl stop validity-fprintd
sudo validity-probe
sudo systemctl start validity-fprintd
```

Writing variants, for unprovisioned sensors only:

```sh
sudo validity-baseline --write          # capture and store a calibration baseline
sudo validity-firmware --upload         # install firmware (see fetch-firmware.sh)
sudo validity-provision --init-flash    # partition flash and pair to this machine
sudo validity-provision --factory-reset # DESTRUCTIVE, erases everything
```

The firmware blob is proprietary and cannot be shipped. `scripts/fetch-firmware.sh`
downloads Lenovo's driver installer, verifies its SHA-512, and extracts it.

Root is required for raw USB access and to read `/sys/class/dmi/id/product_serial`,
which the pairing keys derive from. Pass `--trace` for a hex dump of the wire
traffic.

### Pairing states

`validity-probe` reports one of three states:

- **Paired to this host** — the sensor's private key decrypts and authenticates.
  Sessions work with no writes to the sensor.
- **Paired to another host** — a pairing record exists but was sealed by a
  different install, usually Windows Hello. Using the sensor would require a
  factory reset that rewrites sensor flash.
- **Unpaired** — no host key; the sensor needs provisioning.

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
`product_name` and `product_serial`. That binding is why a sensor paired under
one OS install will not open under another.

## Making a release

Releases are built when a GitHub Release is **published**, not when a tag is
pushed, so the notes describing what changed are written by hand.

1. Tag and push the commit to release:
   `git tag v0.1.0 && git push origin v0.1.0`
2. Draft a release for that tag on GitHub and write the notes.
3. Publish it. CI builds both architectures, checks each binary is
   self-contained, and attaches the tarballs and `SHA256SUMS` to the release.

The notes are never overwritten: CI uploads assets to the existing release
rather than creating one. To check a build without publishing anything, run the
workflow manually and give it an existing tag.

## Credit

The wire protocol is undocumented by the vendor. It was established by the
reverse-engineering work of [python-validity](https://github.com/uunicorn/python-validity)
(MIT), without which this project would not be feasible. This is an independent
Rust implementation, not a translation, but the protocol knowledge is theirs.

If you want a working fingerprint reader today rather than a driver project,
use python-validity — it is mature and covers these sensors.

## License

MIT. See [LICENSE](LICENSE).
