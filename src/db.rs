//! The sensor's on-chip record database.
//!
//! Enrolled fingers live in flash on the sensor, not on the host. Records form
//! a tree: a named storage object holds users, and each user holds fingers.
//! All of these commands run over the encrypted channel.

use crate::sid::{parse_identity, SidIdentity};
use crate::usb::{check_status, Transport};
use anyhow::{bail, Result};

/// The storage object name the Windows driver creates, which we reuse.
pub const STORAGE_NAME: &str = "StgWindsor";

/// Returned when a lookup finds nothing.
const STATUS_NOT_FOUND: u16 = 0x04b3;

pub fn finger_name(subtype: u16) -> &'static str {
    match subtype {
        1 => "right-thumb",
        2 => "right-index-finger",
        3 => "right-middle-finger",
        4 => "right-ring-finger",
        5 => "right-little-finger",
        6 => "left-thumb",
        7 => "left-index-finger",
        8 => "left-middle-finger",
        9 => "left-ring-finger",
        10 => "left-little-finger",
        13 => "right-hand-four-fingers",
        14 => "left-hand-four-fingers",
        15 => "two-thumbs",
        0xf5..=0xfe => "unspecified",
        _ => "unknown",
    }
}

#[derive(Debug, Clone)]
pub struct FingerRef {
    pub dbid: u16,
    pub subtype: u16,
    pub storage: u16,
    pub value_size: u16,
}

#[derive(Debug, Clone)]
pub struct UserRef {
    pub dbid: u16,
    pub value_size: u16,
}

#[derive(Debug, Clone)]
pub struct UserStorage {
    pub dbid: u16,
    pub name: String,
    pub users: Vec<UserRef>,
}

#[derive(Debug, Clone)]
pub struct User {
    pub dbid: u16,
    pub identity: SidIdentity,
    pub fingers: Vec<FingerRef>,
}

#[derive(Debug, Clone)]
pub struct DbInfo {
    /// Partition size in bytes.
    pub total: u32,
    /// Bytes held by live records.
    pub used: u32,
    /// Unallocated bytes.
    pub free: u32,
    /// Record count, including deleted ones.
    pub records: u16,
    pub roots: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct RecordChild {
    pub dbid: u16,
    pub kind: u16,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub dbid: u16,
    pub kind: u16,
    pub storage: u16,
    pub value: Vec<u8>,
    pub children: Vec<RecordChild>,
}

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// `0x45` — partition usage statistics.
pub fn db_info(t: &mut impl Transport) -> Result<DbInfo> {
    let rsp = t.cmd(&[0x45])?;
    check_status(&rsp)?;
    let b = &rsp[2..];
    if b.len() < 0x18 {
        bail!("db info reply too short ({} bytes)", b.len());
    }

    let nroots = rd16(b, 22) as usize;
    let roots = (0..nroots)
        .filter_map(|i| b.get(0x18 + i * 2..0x18 + i * 2 + 2).map(|s| rd16(s, 0)))
        .collect();

    Ok(DbInfo {
        total: rd32(b, 8),
        used: rd32(b, 12),
        free: rd32(b, 16),
        records: rd16(b, 20),
        roots,
    })
}

/// `0x4b` — look up a storage object by name.
pub fn get_user_storage(t: &mut impl Transport, name: &str) -> Result<Option<UserStorage>> {
    let mut name_bytes = name.as_bytes().to_vec();
    if !name_bytes.is_empty() {
        name_bytes.push(0);
    }

    let mut cmd = vec![0x4b];
    cmd.extend_from_slice(&0u16.to_le_bytes());
    cmd.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    cmd.extend_from_slice(&name_bytes);

    let rsp = t.cmd(&cmd)?;
    if rsp.len() >= 2 && rd16(&rsp, 0) == STATUS_NOT_FOUND {
        return Ok(None);
    }
    check_status(&rsp)?;

    let b = &rsp[2..];
    if b.len() < 8 {
        bail!("user storage reply too short ({} bytes)", b.len());
    }
    let (recid, usercnt, namesz) = (rd16(b, 0), rd16(b, 2) as usize, rd16(b, 4) as usize);

    let tab = b.get(8..8 + 4 * usercnt).ok_or_else(|| anyhow::anyhow!("user table truncated"))?;
    let name_field = b
        .get(8 + 4 * usercnt..8 + 4 * usercnt + namesz)
        .ok_or_else(|| anyhow::anyhow!("storage name truncated"))?;

    let users = (0..usercnt)
        .map(|i| UserRef { dbid: rd16(tab, i * 4), value_size: rd16(tab, i * 4 + 2) })
        .collect();

    Ok(Some(UserStorage {
        dbid: recid,
        name: String::from_utf8_lossy(name_field).trim_end_matches('\0').to_string(),
        users,
    }))
}

/// `0x4a` — fetch a user record and its finger list.
pub fn get_user(t: &mut impl Transport, dbid: u16) -> Result<User> {
    let mut cmd = vec![0x4a];
    cmd.extend_from_slice(&dbid.to_le_bytes());
    cmd.extend_from_slice(&0u16.to_le_bytes());
    cmd.extend_from_slice(&0u16.to_le_bytes());

    let rsp = t.cmd(&cmd)?;
    check_status(&rsp)?;
    parse_user(&rsp[2..])
}

fn parse_user(b: &[u8]) -> Result<User> {
    if b.len() < 8 {
        bail!("user reply too short ({} bytes)", b.len());
    }
    let (recid, fingercnt, identitysz) = (rd16(b, 0), rd16(b, 2) as usize, rd16(b, 6) as usize);

    let tab =
        b.get(8..8 + 8 * fingercnt).ok_or_else(|| anyhow::anyhow!("finger table truncated"))?;
    let identity = b
        .get(8 + 8 * fingercnt..8 + 8 * fingercnt + identitysz)
        .ok_or_else(|| anyhow::anyhow!("identity field truncated"))?;

    let fingers = (0..fingercnt)
        .map(|i| FingerRef {
            dbid: rd16(tab, i * 8),
            subtype: rd16(tab, i * 8 + 2),
            storage: rd16(tab, i * 8 + 4),
            value_size: rd16(tab, i * 8 + 6),
        })
        .collect();

    Ok(User { dbid: recid, identity: parse_identity(identity)?, fingers })
}

/// `0x49` — read a record's value.
pub fn get_record_value(t: &mut impl Transport, dbid: u16) -> Result<Record> {
    let mut cmd = vec![0x49];
    cmd.extend_from_slice(&dbid.to_le_bytes());

    let rsp = t.cmd(&cmd)?;
    check_status(&rsp)?;
    if rsp.len() < 12 {
        bail!("record value reply too short ({} bytes)", rsp.len());
    }
    let sz = rd16(&rsp, 8) as usize;

    Ok(Record {
        dbid: rd16(&rsp, 2),
        kind: rd16(&rsp, 4),
        storage: rd16(&rsp, 6),
        value: rsp.get(12..12 + sz).unwrap_or_default().to_vec(),
        children: Vec::new(),
    })
}

/// `0x46` — list a record's children.
pub fn get_record_children(t: &mut impl Transport, dbid: u16) -> Result<Record> {
    let mut cmd = vec![0x46];
    cmd.extend_from_slice(&dbid.to_le_bytes());

    let rsp = t.cmd(&cmd)?;
    check_status(&rsp)?;
    if rsp.len() < 14 {
        bail!("record children reply too short ({} bytes)", rsp.len());
    }
    let cnt = rd16(&rsp, 10) as usize;
    let tab = &rsp[14..];

    let children = (0..cnt)
        .filter_map(|i| {
            tab.get(i * 4..i * 4 + 4).map(|c| RecordChild { dbid: rd16(c, 0), kind: rd16(c, 2) })
        })
        .collect();

    Ok(Record {
        dbid: rd16(&rsp, 2),
        kind: rd16(&rsp, 4),
        storage: rd16(&rsp, 6),
        value: Vec::new(),
        children,
    })
}

/// Every enrolled user, with their fingers.
pub fn list_users(t: &mut impl Transport) -> Result<Vec<User>> {
    let Some(storage) = get_user_storage(t, STORAGE_NAME)? else {
        return Ok(Vec::new());
    };
    storage.users.iter().map(|u| get_user(t, u.dbid)).collect()
}
