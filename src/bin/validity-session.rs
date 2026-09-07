//! Bring up an encrypted session with the sensor and prove it works.
//!
//! Read-only: this opens a session and reads the partition table back through
//! the encrypted channel. Nothing is written to the sensor.

use anyhow::Result;
use validity_fprintd::crypto::HostKeys;
use validity_fprintd::flash::{get_flash_info, read_tls_flash};
use validity_fprintd::init::{reboot, send_init};
use validity_fprintd::pairing::{build_material, host_identity, parse_flash_blocks};
use validity_fprintd::tls::{PairingMaterial, Tls};
use validity_fprintd::usb::{device_name, Usb};

fn main() -> Result<()> {
    let trace = std::env::args().any(|a| a == "--trace");
    println!("validity-fprintd session test (read-only)\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    println!("Device      : {:04x}:{:04x} ({})", usb.vid, usb.pid,
             device_name(usb.vid, usb.pid).unwrap_or("unknown"));

    send_init(&usb)?;
    println!("Init        : signed init blob accepted");

    let blocks = parse_flash_blocks(&read_tls_flash(&mut usb)?)?;
    let (product_name, product_serial) = host_identity()?;
    let keys = HostKeys::derive(&product_name, &product_serial);
    let material = build_material(&blocks, &keys)?;

    println!(
        "Sensor auth : {}",
        if material.firmware_signature_valid {
            "block 6 carries a valid Synaptics signature"
        } else {
            "WARNING: firmware signature did not verify"
        }
    );
    println!("Cert        : {} bytes", material.tls_cert.len());

    let usb = std::sync::Arc::new(usb);
    let mut tls = Tls::new(
        usb.clone(),
        PairingMaterial {
            private_key_d: material.private_key_d,
            tls_cert: material.tls_cert,
            ecdh_public: material.ecdh_public,
        },
    );

    println!("\nOpening session...");
    tls.open()?;
    println!("SESSION ESTABLISHED (secure_tx={}, secure_rx={})", tls.secure_tx, tls.secure_rx);

    // Round-trip a command through the encrypted channel. Matching the values
    // the plain-text probe reported proves encrypt, MAC and decrypt all work.
    let info = get_flash_info(&mut tls)?;
    println!("\nPartition table read back over the encrypted channel:");
    println!(
        "  JEDEC {:#06x}:{:#06x}, {} blocks x {} bytes",
        info.jedec_id.0, info.jedec_id.1, info.blocks, info.blocksize
    );
    for p in &info.partitions {
        println!(
            "  partition {:#04x} type {:#04x} access {:#06x} @ {:#010x} size {:#010x}",
            p.id, p.kind, p.access_lvl, p.offset, p.size
        );
    }

    println!("\nRebooting sensor to release the session context.");
    reboot(&mut tls)?;
    Ok(())
}
