//! The sensor's blank reference image, or "clean slate".
//!
//! Capture subtracts a stored blank frame to cancel each sensor's fixed
//! pattern noise. That frame lives in flash partition 6, and without it the
//! sensor cannot produce a usable image. This module builds the record, checks
//! it against what is stored, and writes it when it is missing.

use crate::capture::{Calibration, CLEAN_SLATE_MAGIC, PARTITION_CALIBRATION};
use crate::crypto::sha256;
use crate::flash::{erase_flash, read_flash, read_flash_all, write_flash_all};
use crate::sensor::{CaptureMode, SensorConfig};
use crate::tls::Tls;
use crate::usb::check_status;
use anyhow::{bail, Context, Result};
use std::time::Duration;

/// Offset of the body within a baseline record.
const BODY_OFFSET: u32 = 0x44;

/// State of the stored baseline relative to a candidate record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaselineState {
    /// Flash already holds exactly this record.
    Matches,
    /// The partition is erased, so a record can be written directly.
    Blank,
    /// Flash holds a different record, which must be erased first.
    Differs,
}

/// Wrap a baseline body in its record header.
///
/// Layout: magic, body length, SHA-256 of the body, 32 reserved zero bytes,
/// then the body itself.
fn encode_record(body: &[u8]) -> Vec<u8> {
    let mut out = CLEAN_SLATE_MAGIC.to_le_bytes().to_vec();
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(&sha256(body));
    out.resize(out.len() + 0x20, 0);
    out.extend_from_slice(body);
    out
}

/// The body carries the blank image with its own length prefix and a
/// terminating zero word.
fn encode_body(img: &[u8]) -> Vec<u8> {
    let mut body = (img.len() as u16).to_le_bytes().to_vec();
    body.extend_from_slice(img);
    body.extend_from_slice(&0u16.to_le_bytes());
    body
}

/// Capture a blank reference frame and build a record from it.
///
/// The frame depends on accumulated calibration data, so run the calibration
/// iterations before calling this.
pub fn build(calib: &Calibration, tls: &mut Tls, cfg: &SensorConfig) -> Result<Vec<u8>> {
    let cmd = calib.build_cmd_02(CaptureMode::Calibrate, cfg)?;
    check_status(&tls.cmd(&cmd)?).context("requesting a blank frame")?;

    let raw = tls.usb().read_data(Duration::from_secs(10))?;
    let img = calib.average(&raw, cfg)?;
    Ok(encode_record(&encode_body(&img)))
}

/// Read the stored baseline record, header and body together.
pub fn read_stored(tls: &mut Tls) -> Result<Option<Vec<u8>>> {
    let head = read_flash(tls, PARTITION_CALIBRATION, 0, BODY_OFFSET)?;
    if head.len() < BODY_OFFSET as usize {
        return Ok(None);
    }
    if u16::from_le_bytes([head[0], head[1]]) != CLEAN_SLATE_MAGIC {
        return Ok(None);
    }

    let len = u16::from_le_bytes([head[2], head[3]]) as u32;
    let body = read_flash_all(tls, PARTITION_CALIBRATION, BODY_OFFSET, len)?;

    let mut record = head;
    record.extend_from_slice(&body);
    Ok(Some(record))
}

/// What a round-trip check found.
#[derive(Debug)]
pub struct RoundTrip {
    /// Re-encoding the stored body reproduced the stored record exactly.
    pub record_matches: bool,
    /// The body's own length prefix and terminator are well formed.
    pub body_well_formed: bool,
    pub image_len: usize,
}

/// Re-encode the baseline already in flash and check it reproduces byte for byte.
///
/// This validates the encoder against known-good data without writing
/// anything, and without depending on sensor noise being reproducible — which
/// a freshly captured frame would be.
pub fn verify_roundtrip(tls: &mut Tls) -> Result<RoundTrip> {
    let Some(stored) = read_stored(tls)? else {
        bail!("no baseline record in flash to check against");
    };

    let body = &stored[BODY_OFFSET as usize..];
    let rebuilt = encode_record(body);

    // The body should be a length-prefixed image followed by a zero word.
    let declared = if body.len() >= 2 { u16::from_le_bytes([body[0], body[1]]) as usize } else { 0 };
    let terminator_zero = body.len() >= 2 && body[body.len() - 2..] == [0, 0];

    Ok(RoundTrip {
        record_matches: rebuilt == stored,
        body_well_formed: body.len() == declared + 4 && terminator_zero,
        image_len: declared,
    })
}

/// Compare a candidate record against what is stored.
pub fn compare(tls: &mut Tls, candidate: &[u8]) -> Result<BaselineState> {
    let head = read_flash(tls, PARTITION_CALIBRATION, 0, BODY_OFFSET)?;

    if head.iter().all(|b| *b == 0xff) {
        return Ok(BaselineState::Blank);
    }
    // The header alone identifies a record, since it carries the body's hash.
    if candidate.len() >= BODY_OFFSET as usize && head[..] == candidate[..BODY_OFFSET as usize] {
        return Ok(BaselineState::Matches);
    }
    Ok(BaselineState::Differs)
}

/// Store a baseline record, erasing an existing one if it differs.
///
/// Returns whether anything was written. This is the only calibration path
/// that modifies the sensor, and the only one that can leave it unusable if
/// the captured frame is bad, so callers should gate it behind explicit intent.
pub fn persist(tls: &mut Tls, write_enable_blob: &[u8], candidate: &[u8]) -> Result<bool> {
    match compare(tls, candidate)? {
        BaselineState::Matches => {
            eprintln!("baseline: flash already holds this record, nothing to do");
            Ok(false)
        }
        BaselineState::Blank => {
            eprintln!("baseline: partition is blank, writing {} bytes", candidate.len());
            write_flash_all(tls, write_enable_blob, PARTITION_CALIBRATION, 0, candidate)?;
            Ok(true)
        }
        BaselineState::Differs => {
            eprintln!("baseline: flash holds a different record, erasing and rewriting");
            erase_flash(tls, write_enable_blob, PARTITION_CALIBRATION)?;
            write_flash_all(tls, write_enable_blob, PARTITION_CALIBRATION, 0, candidate)?;
            Ok(true)
        }
    }
}
