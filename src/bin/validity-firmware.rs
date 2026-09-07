//! Inspect and install the sensor's firmware extension.
//!
//! Read-only by default. `--upload` writes flash partition 2 and reboots the
//! sensor, and refuses to run if firmware is already present.

use anyhow::{bail, Result};
use std::sync::Arc;
use validity_fprintd::blobs::blobs_for;
use validity_fprintd::firmware;
use validity_fprintd::init::open_session;
use validity_fprintd::usb::{device_name, Usb};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let trace = args.iter().any(|a| a == "--trace");
    let upload = args.iter().any(|a| a == "--upload");
    let file = args.iter().position(|a| a == "--file").and_then(|i| args.get(i + 1)).cloned();

    println!("validity-fprintd firmware tool\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    let (vid, pid) = (usb.vid, usb.pid);
    println!("Device      : {vid:04x}:{pid:04x} ({})", device_name(vid, pid).unwrap_or("unknown"));

    let write_enable_blob = blobs_for(vid, pid)
        .ok_or_else(|| anyhow::anyhow!("no blobs for {vid:04x}:{pid:04x}"))?
        .db_write_enable();

    let (mut tls, _) = open_session(Arc::new(usb))?;

    match firmware::current(&mut tls)? {
        Some(fw) => {
            println!(
                "Firmware    : v{}.{}, buildtime {:#010x}, {} modules",
                fw.major,
                fw.minor,
                fw.buildtime,
                fw.modules.len()
            );
            for m in &fw.modules {
                println!(
                    "  module type {:#06x} subtype {:#06x} v{}.{} — {} bytes",
                    m.kind, m.subtype, m.major, m.minor, m.size
                );
            }
        }
        None => println!("Firmware    : NOT PRESENT — the sensor cannot capture without it"),
    }

    // Where the blob would come from, if it is needed.
    let source = firmware::firmware_source(vid, pid);
    let on_disk = firmware::find_firmware_file(vid, pid);
    match (&source, &on_disk) {
        (Some(s), Some(p)) => println!("Blob        : {} (from {})", p.display(), s.file_name),
        (Some(s), None) => {
            println!("Blob        : {} not found locally", s.file_name);
            println!("              run scripts/fetch-firmware.sh to extract it");
        }
        (None, _) => println!("Blob        : no known firmware source for this device"),
    }

    if !upload {
        println!("\nRead-only run. Pass --upload to install firmware.");
        return Ok(());
    }

    if firmware::current(&mut tls)?.is_some() {
        println!("\nFirmware is already present; not touching it.");
        return Ok(());
    }

    let path = match file {
        Some(f) => std::path::PathBuf::from(f),
        None => on_disk.ok_or_else(|| {
            anyhow::anyhow!("no firmware file found; run scripts/fetch-firmware.sh or pass --file")
        })?,
    };

    println!("\nUploading from {}", path.display());
    let raw = std::fs::read(&path)?;
    let uploaded = firmware::upload(&mut tls, &write_enable_blob, &raw)?;

    if uploaded {
        println!("\nFirmware installed. The sensor has rebooted and will reappear shortly.");
        println!("Run this tool again to confirm, then validity-baseline to calibrate.");
    } else {
        bail!("upload reported nothing to do, which was not expected here");
    }
    Ok(())
}
