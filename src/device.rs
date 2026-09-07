//! A single-operation view of the sensor, for daemon use.
//!
//! Each operation opens a fresh session and closes it again. Session state is
//! cheap to rebuild and the sensor leaks context if sessions are held open, so
//! this is both simpler and better behaved than a long-lived handle.

use crate::capture::{capture, glow_end_scan, glow_start_scan, match_finger, Calibration};
use crate::db::{finger_name, list_users};
use crate::init::{open_session, reboot};
use crate::sensor::{CaptureMode, SensorConfig};
use crate::sid::SidIdentity;
use crate::usb::Usb;
use anyhow::{Context, Result};
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

/// Names of the fingers enrolled for a user, as fprintd finger names.
pub fn list_enrolled_fingers(username: &str) -> Result<Vec<String>> {
    let sid = sid_for_user(username)?;
    let mut usb = Usb::open_first()?;
    let (mut tls, _) = open_session(&mut usb)?;

    let users = list_users(&mut tls)?;
    let fingers = users
        .iter()
        .filter(|u| u.identity == sid)
        .flat_map(|u| u.fingers.iter().map(|f| finger_name(f.subtype).to_string()))
        .collect();

    let _ = reboot(&mut tls);
    Ok(fingers)
}

/// Capture a fingerprint and match it against the enrolments for `username`.
pub fn verify(username: &str, timeout: Duration) -> Result<VerifyOutcome> {
    let sid = sid_for_user(username)?;
    let mut usb = Usb::open_first()?;
    let (mut tls, _) = open_session(&mut usb)?;

    let cfg = SensorConfig::probe(&mut tls)?;
    let calib = Calibration::load_or_calibrate(&mut tls, &cfg)?;

    let _ = glow_start_scan(&mut tls);
    let captured = capture(&mut tls, &calib, &cfg, CaptureMode::Identify, timeout);

    let outcome = match captured {
        Err(e) => VerifyOutcome::Retry(format!("{e:#}")),
        Ok(_) => match match_finger(&mut tls) {
            Err(_) => VerifyOutcome::NoMatch,
            Ok(m) => {
                // The sensor matched, but it must be this user's finger.
                let owner = crate::db::get_user(&mut tls, m.user_dbid as u16).ok();
                if owner.map(|u| u.identity) == Some(sid) {
                    VerifyOutcome::Match { finger: finger_name(m.subtype).to_string() }
                } else {
                    VerifyOutcome::NoMatch
                }
            }
        },
    };

    let _ = glow_end_scan(&mut tls);
    let _ = reboot(&mut tls);
    Ok(outcome)
}
