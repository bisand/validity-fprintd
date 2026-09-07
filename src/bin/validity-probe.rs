//! Read-only diagnostic for Validity fingerprint sensors.
//!
//! Every command issued here is a read. Nothing is written to the sensor, no
//! firmware is uploaded, and the pairing record is never modified.

use anyhow::Result;
use validity_fprintd::crypto::HostKeys;
use validity_fprintd::flash::{get_flash_info, get_fw_info, read_tls_flash};
use validity_fprintd::pairing::{host_identity, inspect_pairing, parse_flash_blocks, PairingState};
use validity_fprintd::usb::{device_name, Usb};

fn main() -> Result<()> {
    let trace = std::env::args().any(|a| a == "--trace");

    println!("validity-fprintd probe (read-only)\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    let name = device_name(usb.vid, usb.pid).unwrap_or("unknown");
    println!("Device      : {:04x}:{:04x} ({name})", usb.vid, usb.pid);

    // 0x01 — ROM info. The most basic liveness check.
    match usb.cmd(&[0x01]) {
        Ok(rsp) => println!("ROM info    : {}", hex::encode(&rsp)),
        Err(e) => println!("ROM info    : failed ({e})"),
    }

    // Partition 2 holds the firmware extension.
    match get_fw_info(&mut usb, 2) {
        Ok(Some(fw)) => {
            println!(
                "Firmware    : v{}.{}, buildtime {:#010x}, {} modules",
                fw.major,
                fw.minor,
                fw.buildtime,
                fw.modules.len()
            );
        }
        Ok(None) => println!("Firmware    : no firmware extension loaded (clean slate)"),
        Err(e) => println!("Firmware    : query failed ({e})"),
    }

    match get_flash_info(&mut usb) {
        Ok(info) => {
            println!(
                "Flash       : JEDEC {:#06x}:{:#06x}, {} blocks x {} bytes",
                info.jedec_id.0, info.jedec_id.1, info.blocks, info.blocksize
            );
            for p in &info.partitions {
                println!(
                    "  partition {:#04x} type {:#04x} access {:#06x} @ {:#010x} size {:#010x}",
                    p.id, p.kind, p.access_lvl, p.offset, p.size
                );
            }
        }
        Err(e) => println!("Flash       : partition table unavailable without TLS ({e})"),
    }

    // The pairing record lives in partition 1, readable without a session.
    println!();
    let flash = match read_tls_flash(&mut usb) {
        Ok(f) => f,
        Err(e) => {
            println!("Pairing     : could not read partition 1 ({e})");
            return Ok(());
        }
    };

    let blocks = parse_flash_blocks(&flash)?;
    println!("Pairing record blocks:");
    for b in &blocks {
        println!("  id {:#06x}  {} bytes", b.id, b.body.len());
    }

    let (product_name, product_serial) = match host_identity() {
        Ok(id) => id,
        Err(e) => {
            println!("\nHost identity unavailable: {e}");
            return Ok(());
        }
    };
    println!("\nHost        : {product_name} / serial {}", mask(&product_serial));

    let keys = HostKeys::derive(&product_name, &product_serial);
    match inspect_pairing(&blocks, &keys)? {
        PairingState::PairedToThisHost { .. } => {
            println!("Status      : PAIRED TO THIS HOST");
            println!("  The sensor's private key decrypted and authenticated.");
            println!("  A TLS session can be established without re-pairing.");
        }
        PairingState::PairedToAnotherHost => {
            println!("Status      : PAIRED TO ANOTHER HOST");
            println!("  A pairing record exists but was sealed by a different install");
            println!("  (most often Windows Hello). Using it here requires a factory");
            println!("  reset and re-pair, which rewrites sensor flash.");
        }
        PairingState::Unpaired => {
            println!("Status      : UNPAIRED");
            println!("  No host key present. The sensor needs provisioning before use.");
        }
    }

    Ok(())
}

fn mask(serial: &str) -> String {
    if serial.len() <= 4 {
        return "*".repeat(serial.len());
    }
    format!("{}{}", "*".repeat(serial.len() - 4), &serial[serial.len() - 4..])
}
