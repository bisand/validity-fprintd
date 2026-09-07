//! Provision a blank sensor, or reset one to factory state.
//!
//! Read-only by default. The destructive operations require explicit flags and
//! a typed confirmation, and neither has been tested on real hardware.

use anyhow::{bail, Result};
use std::io::Write;
use std::sync::Arc;
use validity_fprintd::blobs::blobs_for;
use validity_fprintd::crypto::HostKeys;
use validity_fprintd::flash::{get_flash_info, get_fw_info, read_tls_flash};
use validity_fprintd::pairing::{host_identity, inspect_pairing, parse_flash_blocks, PairingState};
use validity_fprintd::provision;
use validity_fprintd::usb::{device_name, Usb};

fn confirm(prompt: &str, expected: &str) -> Result<bool> {
    print!("{prompt}\nType {expected} to continue: ");
    std::io::stdout().flush()?;

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim() == expected)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let trace = args.iter().any(|a| a == "--trace");
    let do_init = args.iter().any(|a| a == "--init-flash");
    let do_reset = args.iter().any(|a| a == "--factory-reset");

    println!("validity-fprintd provisioning tool\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    let (vid, pid) = (usb.vid, usb.pid);
    println!("Device      : {vid:04x}:{pid:04x} ({})", device_name(vid, pid).unwrap_or("unknown"));

    let blobs = blobs_for(vid, pid)
        .ok_or_else(|| anyhow::anyhow!("no blobs for {vid:04x}:{pid:04x}"))?;

    // --- Status, using plain commands so an unprovisioned sensor still reports ---
    let flash = get_flash_info(&mut usb);
    let partitioned = flash.as_ref().map(|f| !f.partitions.is_empty()).unwrap_or(false);
    match &flash {
        Ok(f) if partitioned => println!("Flash       : {} partitions", f.partitions.len()),
        Ok(_) => println!("Flash       : UNFORMATTED (no partition table)"),
        Err(e) => println!("Flash       : could not read partition table ({e})"),
    }

    println!(
        "Firmware    : {}",
        match get_fw_info(&mut usb, 2) {
            Ok(Some(fw)) => format!("v{}.{} present", fw.major, fw.minor),
            Ok(None) => "NOT PRESENT".to_string(),
            Err(e) => format!("unknown ({e})"),
        }
    );

    let mut paired_here = false;
    if partitioned {
        if let (Ok(raw), Ok((name, serial))) = (read_tls_flash(&mut usb), host_identity()) {
            if let Ok(blocks) = parse_flash_blocks(&raw) {
                let keys = HostKeys::derive(&name, &serial);
                match inspect_pairing(&blocks, &keys) {
                    Ok(PairingState::PairedToThisHost { .. }) => {
                        paired_here = true;
                        println!("Pairing     : paired to this host");
                    }
                    Ok(PairingState::PairedToAnotherHost) => {
                        println!("Pairing     : paired to a DIFFERENT host")
                    }
                    Ok(PairingState::Unpaired) => println!("Pairing     : UNPAIRED"),
                    Err(e) => println!("Pairing     : could not determine ({e})"),
                }
            }
        }
    }

    if !do_init && !do_reset {
        println!("\nRead-only run.");
        if paired_here {
            println!("This sensor is provisioned; there is nothing to do.");
        } else if !partitioned {
            println!("Flash is unformatted. Run with --init-flash to provision it.");
        } else {
            println!("Flash is partitioned but not paired to this host.");
            println!("Provisioning would require --factory-reset first, which erases everything.");
        }
        return Ok(());
    }

    // --- Destructive paths ---
    if do_reset {
        println!("\n*** FACTORY RESET ***");
        println!("This erases the pairing record, every enrolled fingerprint, and the");
        println!("calibration baseline. The sensor will be unusable until it is");
        println!("provisioned again, and provisioning has never been tested on real");
        println!("hardware. If it fails, you may not be able to recover the sensor");
        println!("with this driver.");
        if paired_here {
            println!("\nNOTE: this sensor currently WORKS. You are about to break it.");
        }

        if !confirm("", "RESET MY FINGERPRINT SENSOR")? {
            println!("Aborted; nothing was changed.");
            return Ok(());
        }

        provision::factory_reset(&usb, &blobs.reset_blob())?;
        println!("Factory reset sent. The sensor is rebooting.");
        println!("Run with --init-flash once it reappears.");
        return Ok(());
    }

    if do_init {
        if partitioned {
            println!("\nFlash already has a partition table; init-flash would do nothing.");
            println!("Use --factory-reset first if you really intend to reprovision.");
            return Ok(());
        }

        println!("\n*** PROVISIONING ***");
        println!("This formats sensor flash and writes a new pairing record bound to");
        println!("this machine. It has never been tested on real hardware.");

        if !confirm("", "PROVISION")? {
            println!("Aborted; nothing was changed.");
            return Ok(());
        }

        let (name, serial) = host_identity()?;
        let done = provision::init_flash(
            Arc::new(usb),
            &blobs.db_write_enable(),
            &blobs.reset_blob(),
            &name,
            &serial,
        )?;

        if done {
            println!("\nProvisioned. Next steps:");
            println!("  1. sudo validity-firmware --upload   (install firmware)");
            println!("  2. sudo validity-baseline --write    (capture the calibration baseline)");
            println!("  3. omarchy setup security fingerprint");
        } else {
            bail!("provisioning reported nothing to do, which was not expected here");
        }
    }

    Ok(())
}
