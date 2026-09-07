//! Windows security identifiers.
//!
//! The sensor's database keys users by Windows SID, because its firmware was
//! written for Windows Hello. A Linux host has to synthesise SIDs to match.

use anyhow::{bail, Result};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidIdentity {
    pub revision: u8,
    pub auth: u64,
    pub subauth: Vec<u32>,
}

impl SidIdentity {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(8 + 4 * self.subauth.len());
        b.push(self.revision);
        b.push(self.subauth.len() as u8);
        // The authority is big-endian across six bytes.
        b.extend_from_slice(&((self.auth >> 32) as u16).to_be_bytes());
        b.extend_from_slice(&((self.auth & 0xffff_ffff) as u32).to_be_bytes());
        for s in &self.subauth {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        if b.len() < 8 {
            bail!("SID is too short ({} bytes)", b.len());
        }
        let revision = b[0];
        let subcnt = b[1] as usize;

        let mut auth: u64 = 0;
        for byte in &b[2..8] {
            auth = (auth << 8) | *byte as u64;
        }

        if b.len() < 8 + 4 * subcnt {
            bail!("SID declares {subcnt} sub-authorities but is only {} bytes", b.len());
        }
        let subauth = (0..subcnt)
            .map(|i| {
                let o = 8 + i * 4;
                u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
            })
            .collect();

        Ok(Self { revision, auth, subauth })
    }
}

impl fmt::Display for SidIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "S-{}-{}", self.revision, self.auth)?;
        for s in &self.subauth {
            write!(f, "-{s}")?;
        }
        Ok(())
    }
}

/// Wrap a SID in the tagged union the database expects.
pub fn identity_to_bytes(identity: &SidIdentity) -> Vec<u8> {
    let sid = identity.to_bytes();
    let mut b = 3u32.to_le_bytes().to_vec();
    b.extend_from_slice(&(sid.len() as u32).to_le_bytes());
    b.extend_from_slice(&sid);

    // Windows pads this union to 0x4c bytes, and the firmware treats otherwise
    // identical SIDs of different lengths as distinct keys.
    b.resize(b.len().max(0x4c), 0);
    b
}

pub fn parse_identity(b: &[u8]) -> Result<SidIdentity> {
    if b.len() < 8 {
        bail!("identity blob is too short ({} bytes)", b.len());
    }
    let t = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    if t != 3 {
        bail!("unsupported identity type {t}");
    }
    let l = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    let sid = b.get(8..8 + l).ok_or_else(|| anyhow::anyhow!("identity blob is truncated"))?;
    SidIdentity::from_bytes(sid)
}
