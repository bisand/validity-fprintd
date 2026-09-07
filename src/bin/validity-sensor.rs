//! Report sensor identity, geometry and capture-program selection.
//!
//! Read-only: identification and factory-calibration reads only.

use anyhow::Result;
use validity_fprintd::init::{open_session, reboot};
use validity_fprintd::sensor::SensorConfig;
use validity_fprintd::tables::flash_ic_lookup;
use validity_fprintd::timeslot::{split_chunks, CHUNK_TIMESLOT_OFFSET, CHUNK_TIMESLOT_TABLE};
use validity_fprintd::usb::{device_name, Usb};

fn main() -> Result<()> {
    // Die quietly when piped into head, or into a less that is quit early.
    validity_fprintd::restore_sigpipe();
    let trace = std::env::args().any(|a| a == "--trace");
    println!("validity-fprintd sensor identification (read-only)\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    println!(
        "Device      : {:04x}:{:04x} ({})",
        usb.vid,
        usb.pid,
        device_name(usb.vid, usb.pid).unwrap_or("unknown")
    );

    let (mut tls, _) = open_session(std::sync::Arc::new(usb))?;
    println!("Session     : established\n");

    let cfg = SensorConfig::probe(&mut tls)?;

    println!("Sensor      : {} (type {:#06x})", cfg.device_name, cfg.sensor_type);
    println!(
        "ROM         : major {} minor {} build {} product {:#x} u1 {}",
        cfg.rom.major, cfg.rom.minor, cfg.rom.build, cfg.rom.product, cfg.rom.u1
    );
    println!("Geometry    : {} bytes/line, {} lines/frame", cfg.bytes_per_line, cfg.lines_per_frame);
    println!(
        "              line width {}, repeat x{}, {} lines/calibration",
        cfg.type_info.line_width,
        cfg.type_info.repeat_multiplier,
        cfg.type_info.lines_per_calibration_data
    );
    println!(
        "Calibration : {} frames, {} iterations, key line {:#x}",
        cfg.calibration_frames, cfg.calibration_iterations, cfg.key_calibration_line
    );
    println!(
        "Line update : type {}",
        if cfg.line_update_type1 { "1" } else { "2" }
    );
    println!("Capture prog: {} bytes", cfg.capture_prog.len());

    let chunks = split_chunks(&cfg.capture_prog)?;
    println!("              {} chunks", chunks.len());
    let ts = chunks.iter().find(|c| c.kind == CHUNK_TIMESLOT_TABLE);
    let off = chunks.iter().find(|c| c.kind == CHUNK_TIMESLOT_OFFSET);
    match (ts, off) {
        (Some(ts), Some(off)) => {
            let o = u32::from_le_bytes([off.body[0], off.body[1], off.body[2], off.body[3]]);
            println!("              timeslot table {} bytes, starts at {:#x}", ts.body.len(), o);
        }
        _ => println!("              (no timeslot table chunk found)"),
    }

    println!(
        "Factory     : {} trim values{}",
        cfg.factory_calibration_values.len(),
        match &cfg.factory_calib_data {
            Some(d) => format!(", {} bytes of calibration data", d.len()),
            None => String::new(),
        }
    );

    // Cross-check the flash IC against the geometry the sensor reports.
    let info = validity_fprintd::flash::get_flash_info(&mut tls)?;
    match flash_ic_lookup(
        info.jedec_id.0 as u32,
        info.jedec_id.1 as u32,
        info.blocks as u32 * info.blocksize as u32,
    ) {
        Some(ic) => println!("Flash IC    : {} ({} bytes)", ic.name, ic.size),
        None => println!("Flash IC    : unrecognised JEDEC id"),
    }

    println!("\nRebooting sensor to release the session context.");
    reboot(&mut tls)?;
    Ok(())
}
