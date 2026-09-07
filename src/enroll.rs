//! Fingerprint enrolment.
//!
//! Enrolment is iterative and driven by the sensor: each scan is fed back in
//! along with the accumulated template, and the sensor decides when it has
//! enough. Completion is signalled by the appearance of a template id.

use crate::capture::{capture, glow_end_scan, glow_start_scan, wait_int, Calibration};
use crate::db::{delete_fingers_of_subtype, lookup_user, new_finger, new_user};
use crate::flash::{call_cleanups, write_enable};
use crate::sensor::{CaptureMode, SensorConfig};
use crate::sid::SidIdentity;
use crate::tls::Tls;
use crate::usb::check_status;
use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

/// Every record in an enrolment reply is prefixed by this much header.
const MAGIC_LEN: usize = 0x38;

/// Upper bound on scans before giving up, so a bad sensor cannot loop forever.
const MAX_STAGES: usize = 32;

/// `0x69 1` — begin an enrolment.
pub fn create_enrollment(tls: &mut Tls) -> Result<()> {
    let mut cmd = vec![0x69];
    cmd.extend_from_slice(&1u32.to_le_bytes());
    check_status(&tls.cmd(&cmd)?).context("creating enrolment")
}

/// `0x69 0` — end the current enrolment step.
pub fn enrollment_update_end(tls: &mut Tls) -> Result<()> {
    let mut cmd = vec![0x69];
    cmd.extend_from_slice(&0u32.to_le_bytes());
    check_status(&tls.cmd(&cmd)?).context("ending enrolment step")
}

/// `0x68` — start an update round, returning the next round key.
pub fn enrollment_update_start(tls: &mut Tls, key: u32) -> Result<u32> {
    let mut cmd = vec![0x68];
    cmd.extend_from_slice(&key.to_le_bytes());
    cmd.extend_from_slice(&0u32.to_le_bytes());

    let rsp = tls.cmd(&cmd)?;
    check_status(&rsp).context("starting enrolment update")?;
    if rsp.len() < 6 {
        bail!("enrolment update reply too short");
    }
    let new_key = u32::from_le_bytes([rsp[2], rsp[3], rsp[4], rsp[5]]);

    wait_int(tls, Instant::now() + Duration::from_secs(5))?;
    Ok(new_key)
}

/// `0x6b` — feed the accumulated template back to the sensor.
fn enrollment_update(tls: &mut Tls, write_enable_blob: &[u8], prev: &[u8]) -> Result<Vec<u8>> {
    write_enable(tls, write_enable_blob)?;

    let mut cmd = vec![0x6b];
    cmd.extend_from_slice(prev);

    let result = (|| -> Result<Vec<u8>> {
        let rsp = tls.cmd(&cmd)?;
        check_status(&rsp).context("enrolment update")?;
        Ok(rsp[2..].to_vec())
    })();

    let cleanup = call_cleanups(tls);
    let out = result?;
    cleanup?;
    Ok(out)
}

/// One enrolment round. Returns `(header, template, template_id)`; a non-empty
/// template id means the sensor considers the enrolment complete.
fn append_new_image(
    tls: &mut Tls,
    write_enable_blob: &[u8],
    prev: &[u8],
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    // The first call arms the sensor and raises an interrupt; the second
    // returns the updated template.
    enrollment_update(tls, write_enable_blob, prev)?;
    wait_int(tls, Instant::now() + Duration::from_secs(10))?;
    let res = enrollment_update(tls, write_enable_blob, prev)?;

    if res.len() < 2 {
        bail!("enrolment reply too short");
    }
    let l = u16::from_le_bytes([res[0], res[1]]) as usize;
    let mut rest = &res[2..];
    if l != rest.len() {
        bail!("enrolment reply length mismatch: {l} != {}", rest.len());
    }

    let (mut header, mut template, mut tid) = (Vec::new(), Vec::new(), Vec::new());

    while rest.len() >= 4 {
        let tag = u16::from_le_bytes([rest[0], rest[1]]);
        let len = u16::from_le_bytes([rest[2], rest[3]]) as usize;
        let end = MAGIC_LEN + len;
        if rest.len() < end {
            bail!("enrolment record {tag} is truncated");
        }

        match tag {
            // The template keeps its header; the others are payload only.
            0 => template = rest[..end].to_vec(),
            1 => header = rest[MAGIC_LEN..end].to_vec(),
            3 => tid = rest[MAGIC_LEN..end].to_vec(),
            _ => {}
        }
        rest = &rest[end..];
    }

    Ok((header, template, tid))
}

/// Wrap a finished template in the record layout the database expects.
fn make_finger_data(subtype: u16, template: &[u8], tid: &[u8]) -> Vec<u8> {
    let mut tinfo = Vec::new();
    tinfo.extend_from_slice(&1u16.to_le_bytes());
    tinfo.extend_from_slice(&(template.len() as u16).to_le_bytes());
    tinfo.extend_from_slice(template);
    tinfo.extend_from_slice(&2u16.to_le_bytes());
    tinfo.extend_from_slice(&(tid.len() as u16).to_le_bytes());
    tinfo.extend_from_slice(tid);

    let mut out = Vec::new();
    out.extend_from_slice(&subtype.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&(tinfo.len() as u16).to_le_bytes());
    out.extend_from_slice(&0x20u16.to_le_bytes());
    out.extend_from_slice(&tinfo);
    out.resize(out.len() + 0x20, 0);
    out
}

/// Enrol a finger, calling `on_stage` after each accepted scan.
///
/// Each iteration asks the user for one touch. The sensor decides how many it
/// needs, so the stage count is not known in advance.
pub fn enroll(
    tls: &mut Tls,
    calib: &Calibration,
    cfg: &SensorConfig,
    write_enable_blob: &[u8],
    identity: &SidIdentity,
    subtype: u16,
    finger_timeout: Duration,
    mut on_stage: impl FnMut(usize, Option<&str>),
) -> Result<u16> {
    create_enrollment(tls)?;

    let mut key = 0u32;
    let mut template: Vec<u8> = Vec::new();
    let mut tid: Vec<u8> = Vec::new();
    let mut stage = 0usize;

    while stage < MAX_STAGES {
        let _ = glow_start_scan(tls);

        let round = (|| -> Result<(Vec<u8>, Vec<u8>)> {
            capture(tls, calib, cfg, CaptureMode::Enroll, finger_timeout)?;
            key = enrollment_update_start(tls, key)?;
            let (_header, new_template, new_tid) =
                append_new_image(tls, write_enable_blob, &template)?;
            Ok((new_template, new_tid))
        })();

        // The sensor expects this after every round, successful or not.
        let _ = enrollment_update_end(tls);

        match round {
            Ok((new_template, new_tid)) => {
                template = new_template;
                tid = new_tid;
                stage += 1;
                on_stage(stage, None);
                if !tid.is_empty() {
                    break;
                }
            }
            Err(e) => {
                // A bad scan is recoverable; ask for another.
                on_stage(stage, Some(&format!("{e:#}")));
            }
        }
    }

    let _ = glow_end_scan(tls);

    if tid.is_empty() {
        bail!("enrolment did not complete after {MAX_STAGES} scans");
    }

    // The reference implementation ends the update twice; the sensor expects it.
    let _ = enrollment_update_end(tls);

    // fprintd semantics: re-enrolling a finger replaces the old record.
    delete_fingers_of_subtype(tls, identity, subtype)?;

    let tinfo = make_finger_data(subtype, &template, &tid);
    let user_dbid = match lookup_user(tls, identity)? {
        Some(u) => u.dbid,
        None => new_user(tls, write_enable_blob, identity)?,
    };

    let recid = new_finger(tls, write_enable_blob, user_dbid, &tinfo)?;
    let _ = wait_int(tls, Instant::now() + Duration::from_secs(5));
    Ok(recid)
}
