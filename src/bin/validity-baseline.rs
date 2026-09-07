//! Inspect, verify and repair the sensor's blank reference image.
//!
//! Read-only by default. Only `--write` modifies the sensor, and it refuses to
//! run unless the baseline is actually missing or you also pass `--force`.

use anyhow::{bail, Result};
use std::sync::Arc;
use validity_fprintd::baseline;
use validity_fprintd::blobs::blobs_for;
use validity_fprintd::capture::{check_clean_slate, Calibration};
use validity_fprintd::init::{open_session, reboot};
use validity_fprintd::sensor::SensorConfig;
use validity_fprintd::usb::{device_name, Usb};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let trace = args.iter().any(|a| a == "--trace");
    let write = args.iter().any(|a| a == "--write");
    let force = args.iter().any(|a| a == "--force");

    println!("validity-fprintd baseline tool\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    let (vid, pid) = (usb.vid, usb.pid);
    println!("Device      : {vid:04x}:{pid:04x} ({})", device_name(vid, pid).unwrap_or("unknown"));

    let write_enable_blob = blobs_for(vid, pid)
        .ok_or_else(|| anyhow::anyhow!("no blobs for {vid:04x}:{pid:04x}"))?
        .db_write_enable();

    let (mut tls, _) = open_session(Arc::new(usb))?;
    let cfg = SensorConfig::probe(&mut tls)?;
    println!("Sensor      : {} (type {:#06x})", cfg.device_name, cfg.sensor_type);

    // --- What is stored right now ---
    let stored = baseline::read_stored(&mut tls)?;
    match &stored {
        Some(record) => println!("Stored      : {} byte record present", record.len()),
        None => println!("Stored      : NO baseline record (partition is blank or unrecognised)"),
    }
    println!(
        "Integrity   : {}",
        if check_clean_slate(&mut tls)? { "hash verifies" } else { "FAILED hash check" }
    );

    // --- Validate the encoder against known-good data, writing nothing ---
    if stored.is_some() {
        let rt = baseline::verify_roundtrip(&mut tls)?;
        println!("\nRound-trip check (no writes):");
        println!("  image size      : {} bytes", rt.image_len);
        println!("  body structure  : {}", if rt.body_well_formed { "well formed" } else { "UNEXPECTED" });
        println!(
            "  re-encode       : {}",
            if rt.record_matches {
                "byte-identical to flash — encoder is correct"
            } else {
                "DIFFERS from flash — encoder is wrong, do not write"
            }
        );

        if !rt.record_matches && write {
            bail!("refusing to write: the encoder does not reproduce the stored record");
        }
    }

    if !write {
        println!("\nRead-only run. Pass --write to capture and store a new baseline.");
        let _ = reboot(&mut tls);
        return Ok(());
    }

    // --- Writing path ---
    if stored.is_some() && !force {
        println!("\nA valid baseline is already stored; not overwriting it.");
        println!("Pass --force as well if you really intend to replace it.");
        let _ = reboot(&mut tls);
        return Ok(());
    }

    println!("\nCalibrating ({} iterations)...", cfg.calibration_iterations);
    let mut calib = Calibration::default();
    calib.calibrate(&mut tls, &cfg)?;
    println!("Calibration : {} bytes", calib.calib_data.len());

    println!("Capturing a blank reference frame — keep the sensor clear.");
    let candidate = baseline::build(&calib, &mut tls, &cfg)?;
    println!("Candidate   : {} byte record", candidate.len());

    let written = baseline::persist(&mut tls, &write_enable_blob, &candidate)?;
    println!("Result      : {}", if written { "baseline written" } else { "no change needed" });

    if written {
        let ok = check_clean_slate(&mut tls)?;
        println!("Verify      : {}", if ok { "stored record verifies" } else { "VERIFICATION FAILED" });
        if !ok {
            bail!("the written baseline does not verify; the sensor may not capture correctly");
        }
        // The cached calibration belongs with the old baseline.
        let _ = std::fs::remove_file(validity_fprintd::capture::CALIB_CACHE_PATH);
    }

    let _ = reboot(&mut tls);
    Ok(())
}
