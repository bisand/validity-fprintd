//! A long-lived handle to the sensor, for daemon use.
//!
//! The session is held open across operations. The sensor allocates a context
//! per session and never reclaims them, so repeatedly opening and closing
//! sessions leaks its memory; and closing a session cleanly requires a reboot,
//! after which the device drops off the USB bus long enough to break any
//! operation that follows. Holding one session avoids both problems.

use crate::capture::{capture, glow_end_scan, glow_start_scan, match_finger, Calibration};
use crate::db::{finger_name, get_user, list_users};
use crate::init::{open_session, reboot};
use crate::sensor::{CaptureMode, SensorConfig};
use crate::sid::SidIdentity;
use crate::tls::Tls;
use crate::usb::Usb;
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;

/// Domain component python-validity uses for synthetic Linux SIDs. Matching it
/// keeps enrolments interoperable between the two drivers.
const SYNTHETIC_DOMAIN: [u32; 3] = [111111111, 1111111111, 1111111111];

/// Map a Linux username to the SID its enrolments are stored under.
pub fn sid_for_user(username: &str) -> Result<SidIdentity> {
    let uid = uid_for_user(username)?;
    let mut subauth = vec![21];
    subauth.extend_from_slice(&SYNTHETIC_DOMAIN);
    subauth.push(uid);
    Ok(SidIdentity { revision: 1, auth: 5, subauth })
}

fn uid_for_user(username: &str) -> Result<u32> {
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    for line in passwd.lines() {
        let mut f = line.split(':');
        if f.next() == Some(username) {
            let uid = f.nth(1).ok_or_else(|| anyhow::anyhow!("malformed passwd entry"))?;
            return uid.parse().context("parsing uid");
        }
    }
    anyhow::bail!("no such user: {username}")
}

/// Outcome of a verification attempt, in fprintd's vocabulary.
#[derive(Debug, Clone)]
pub enum VerifyOutcome {
    Match { finger: String },
    NoMatch,
    /// The scan was unusable; the caller should ask for another.
    Retry(String),
}

pub struct Sensor {
    tls: Tls,
    cfg: SensorConfig,
    calib: Calibration,
    /// Vendor-signed blob that unlocks database writes on this model.
    write_enable_blob: Vec<u8>,
}

impl Sensor {
    /// Open the sensor, establish a session and load or build calibration.
    pub fn open(trace: bool) -> Result<Self> {
        let mut usb = Usb::open_first()?;
        usb.trace = trace;

        let (vid, pid) = (usb.vid, usb.pid);
        let write_enable_blob = crate::blobs::blobs_for(vid, pid)
            .ok_or_else(|| anyhow::anyhow!("no blobs for {vid:04x}:{pid:04x}"))?
            .db_write_enable();

        let (mut tls, signature_valid) = open_session(Arc::new(usb))?;
        if !signature_valid {
            eprintln!("warning: sensor firmware signature did not verify");
        }
        let cfg = SensorConfig::probe(&mut tls)?;
        eprintln!("session: opened with {} (type {:#06x})", cfg.device_name, cfg.sensor_type);
        let calib = Calibration::load_or_calibrate(&mut tls, &cfg)?;

        Ok(Self { tls, cfg, calib, write_enable_blob })
    }

    pub fn device_name(&self) -> &str {
        self.cfg.device_name
    }

    /// Finger names enrolled for `username`.
    pub fn list_enrolled_fingers(&mut self, username: &str) -> Result<Vec<String>> {
        let sid = sid_for_user(username)?;
        let users = list_users(&mut self.tls)?;

        Ok(users
            .iter()
            .filter(|u| u.identity == sid)
            .flat_map(|u| u.fingers.iter().map(|f| finger_name(f.subtype).to_string()))
            .collect())
    }

    /// Capture a fingerprint and match it against `username`'s enrolments.
    pub fn verify(&mut self, username: &str, timeout: Duration) -> Result<VerifyOutcome> {
        let sid = sid_for_user(username)?;

        let _ = glow_start_scan(&mut self.tls);
        let captured =
            capture(&mut self.tls, &self.calib, &self.cfg, CaptureMode::Identify, timeout);

        let outcome = match captured {
            Err(e) => VerifyOutcome::Retry(format!("{e:#}")),
            Ok(_) => match match_finger(&mut self.tls) {
                Err(_) => VerifyOutcome::NoMatch,
                Ok(m) => {
                    // The sensor matched something; it must be this user's finger.
                    let owner = get_user(&mut self.tls, m.user_dbid as u16).ok();
                    if owner.map(|u| u.identity) == Some(sid) {
                        VerifyOutcome::Match { finger: finger_name(m.subtype).to_string() }
                    } else {
                        VerifyOutcome::NoMatch
                    }
                }
            },
        };

        let _ = glow_end_scan(&mut self.tls);
        match &outcome {
            VerifyOutcome::Match { finger } => eprintln!("verify: {username} matched {finger}"),
            VerifyOutcome::NoMatch => eprintln!("verify: {username} no match"),
            VerifyOutcome::Retry(e) => eprintln!("verify: {username} unusable scan: {e}"),
        }
        Ok(outcome)
    }

    /// Enrol a finger for `username`, reporting progress through `on_stage`.
    pub fn enroll(
        &mut self,
        username: &str,
        finger: &str,
        timeout: Duration,
        on_stage: impl FnMut(usize, Option<&str>),
    ) -> Result<u16> {
        let sid = sid_for_user(username)?;
        let subtype = crate::db::finger_subtype(finger)
            .ok_or_else(|| anyhow::anyhow!("unknown finger name: {finger}"))?;

        crate::enroll::enroll(
            &mut self.tls,
            &self.calib,
            &self.cfg,
            &self.write_enable_blob,
            &sid,
            subtype,
            timeout,
            on_stage,
        )
    }

    /// Delete every finger enrolled for `username`. Returns how many went.
    pub fn delete_enrolled_fingers(&mut self, username: &str) -> Result<usize> {
        let sid = sid_for_user(username)?;
        crate::db::delete_all_fingers(&mut self.tls, &sid)
    }

    /// Reboot the sensor to release its session context. Only on shutdown:
    /// the device leaves the USB bus briefly afterwards.
    pub fn shutdown(mut self) {
        let _ = reboot(&mut self.tls);
    }
}

/// Resolve a uid to its login name.
pub fn username_for_uid(uid: u32) -> Result<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() > 2 && f[2].parse::<u32>().ok() == Some(uid) {
            return Ok(f[0].to_string());
        }
    }
    anyhow::bail!("no user with uid {uid}")
}
