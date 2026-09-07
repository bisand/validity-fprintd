# validity-rs

A Rust driver for Synaptics/Validity **match-on-chip** fingerprint sensors — the
family found in ThinkPads and other laptops that stock `libfprint` does not
support, so `fprintd` reports *"No devices available"* on every distribution.

Developed against a ThinkPad X1 Carbon 6th gen (`06cb:009a`, "Metallica MIS").

## Status

Working, verified against real hardware — this driver successfully captures a
fingerprint and matches it on-chip:

| Capability | Status |
|---|---|
| USB bulk transport, endpoint discovery, kernel-driver handoff | Working |
| Signed vendor init handshake | Working |
| Pairing-record parsing and host key derivation | Working |
| Sensor authentication via Synaptics firmware signature | Working |
| Encrypted session (the firmware's TLS 1.2 dialect) | Working |
| Flash access over plain and encrypted transports | Working |
| On-chip enrolment database (storage, users, fingers) | Working |
| Sensor identification, geometry and capture-program selection | Working |
| Calibration and capture-program patching (type-1 sensors) | Working |
| Image capture and on-chip matching | Working |
| Enrolment of new fingers | Not yet implemented |
| `fprintd` / PAM integration | Not yet implemented |

Matching requires fingers that are already enrolled — by the Windows driver, by
python-validity, or by this driver once enrolment lands. There is no login
integration yet, so this is not yet a drop-in replacement for `fprintd`.

Only the type-1 line-update path is implemented. Type-2 sensors are recognised
but will refuse to capture.

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

## Tools

All of these write nothing to the sensor.

```sh
# Device, firmware, flash layout and pairing state.
sudo ./target/release/vfs-probe

# Open an encrypted session and read the partition table back through it.
sudo ./target/release/vfs-session

# List storage objects, users and enrolled fingers.
sudo ./target/release/vfs-db

# Sensor identity, geometry and capture-program selection.
sudo ./target/release/vfs-sensor

# Calibrate, capture a fingerprint and match it on-chip.
sudo ./target/release/vfs-verify
```

Root is required for raw USB access and to read `/sys/class/dmi/id/product_serial`,
which the pairing keys derive from. Pass `--trace` to any tool for a hex dump of
the wire traffic.

### Pairing states

`vfs-probe` reports one of three states:

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
