# validity-fprintd

A Rust driver for Synaptics/Validity **match-on-chip** fingerprint sensors — the
family found in ThinkPads and other laptops that stock `libfprint` does not
support, so `fprintd` reports *"No devices available"* on every distribution.

Developed against a ThinkPad X1 Carbon 6th gen (`06cb:009a`, "Metallica MIS").

## Status

Working and verified against real hardware on a ThinkPad X1 Carbon 6th gen
(`06cb:009a`): it enrols, verifies, and authenticates sudo, polkit and the
lock screen through the stock `pam_fprintd`.

| Capability | Status |
|---|---|
| USB transport, signed init handshake | Verified |
| Pairing-record parsing, host key derivation, sensor authentication | Verified |
| Encrypted session (the firmware's TLS 1.2 dialect) | Verified |
| Flash access, on-chip enrolment database (read and write) | Verified |
| Sensor identification, calibration, image capture | Verified |
| On-chip matching, enrolment, deletion | Verified |
| `fprintd`-compatible D-Bus daemon | Verified |
| Calibration baseline: read, verify, encode | Verified |
| Calibration baseline: write | **Untested** |
| Firmware extension upload | **Untested** |
| Flash partitioning and pairing (`init-flash`) | **Untested** |
| Factory reset | **Untested** |
| Type-2 sensor capture path | **Untested** |

### What "untested" means here

Everything marked untested was developed against a sensor that was **already
provisioned**, so those paths never had to run. They are transcribed from the
reference implementation and compile, but no hardware has executed them.

The baseline encoder is the exception worth calling out: it is validated by
re-encoding the record already in flash and checking it reproduces byte for
byte (`validity-baseline` does this on every run), so the encoding is known
correct even though the write itself has not been exercised.

Treat factory reset especially carefully. It erases the pairing record and
every enrolment, and recovery depends on provisioning paths that have never
been proven. If your sensor currently works, there is no reason to run it.

### Which sensors this works with today

- A sensor previously used with **Windows Hello**, or with python-validity, is
  already provisioned and should work directly. Pairing keys derive from the
  machine's DMI identity, which is identical under either OS, so a
  Windows-paired sensor opens fine on the same laptop.
- A **factory-fresh** sensor reports `UNPAIRED` and needs the provisioning
  paths above, which are untested.

Only sensor type `0x199` has been exercised. The type-2 capture path is
implemented but has never run.

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

## Credit

The wire protocol is undocumented by the vendor. It was established by the
reverse-engineering work of [python-validity](https://github.com/uunicorn/python-validity)
(MIT), without which this project would not be feasible. This is an independent
Rust implementation, not a translation, but the protocol knowledge is theirs.

If you want a working fingerprint reader today rather than a driver project,
use python-validity — it is mature and covers these sensors.

## License

MIT. See [LICENSE](LICENSE).
