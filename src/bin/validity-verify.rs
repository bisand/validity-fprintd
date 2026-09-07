//! Capture a fingerprint and match it on-chip against the enrolled templates.
//!
//! Writes nothing to the sensor: calibration is computed in memory, and
//! matching is a read-only query against existing enrolments.

use anyhow::Result;
use std::io::Write;
use std::time::Duration;
use validity_fprintd::capture::{
    capture, check_clean_slate, glow_end_scan, glow_start_scan, match_finger,
    Calibration,
};
use validity_fprintd::db::{finger_name, get_user, list_users};
use validity_fprintd::init::{open_session, reboot};
use validity_fprintd::sensor::{CaptureMode, SensorConfig};
use validity_fprintd::usb::{device_name, Usb};

fn main() -> Result<()> {
    // Die quietly when piped into head, or into a less that is quit early.
    validity_fprintd::restore_sigpipe();
    let trace = std::env::args().any(|a| a == "--trace");
    println!("validity-fprintd fingerprint verification\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    println!(
        "Device      : {:04x}:{:04x} ({})",
        usb.vid,
        usb.pid,
        device_name(usb.vid, usb.pid).unwrap_or("unknown")
    );

    let (mut tls, _) = open_session(std::sync::Arc::new(usb))?;
    let cfg = SensorConfig::probe(&mut tls)?;
    println!("Sensor      : {} (type {:#06x})", cfg.device_name, cfg.sensor_type);

    let enrolled = list_users(&mut tls)?;
    let total: usize = enrolled.iter().map(|u| u.fingers.len()).sum();
    if total == 0 {
        println!("\nNo fingers are enrolled on this sensor; there is nothing to match against.");
        reboot(&mut tls)?;
        return Ok(());
    }
    println!("Enrolled    : {total} finger(s) across {} user(s)", enrolled.len());

    if check_clean_slate(&mut tls)? {
        println!("Baseline    : valid clean-slate image present on sensor flash");
    } else {
        println!("Baseline    : MISSING from sensor flash — capture will likely fail");
    }

    print!("Calibrating ({} iterations)... ", cfg.calibration_iterations);
    std::io::stdout().flush()?;
    let mut calib = Calibration::default();
    calib.calibrate(&mut tls, &cfg)?;
    println!("{} bytes of calibration data", calib.calib_data.len());

    glow_start_scan(&mut tls)?;
    println!("\n>>> Touch the fingerprint sensor now (30s timeout) <<<\n");

    let result = capture(&mut tls, &calib, &cfg, CaptureMode::Identify, Duration::from_secs(30));

    match result {
        Ok(c) => {
            println!("Captured    : x={} y={} w1={} w2={}", c.x, c.y, c.w1, c.w2);

            match match_finger(&mut tls) {
                Ok(m) => {
                    println!("\nMATCH");
                    println!("  user dbid : {}", m.user_dbid);
                    println!("  finger    : {:#04x} ({})", m.subtype, finger_name(m.subtype));
                    if !m.hash.is_empty() {
                        println!("  hash      : {}", hex::encode(&m.hash));
                    }
                    if let Ok(u) = get_user(&mut tls, m.user_dbid as u16) {
                        println!("  identity  : {}", u.identity);
                    }
                }
                Err(e) => println!("\nNO MATCH: {e}"),
            }
        }
        Err(e) => println!("Capture failed: {e:#}"),
    }

    let _ = glow_end_scan(&mut tls);
    println!("\nRebooting sensor to release the session context.");
    reboot(&mut tls)?;
    Ok(())
}
