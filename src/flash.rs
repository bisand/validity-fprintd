//! Flash partition access.
//!
//! Partition 1 holds the TLS pairing record and carries access level 7, meaning
//! it can be read before a secure session exists. That is what makes an
//! offline, read-only diagnosis of pairing state possible.

use crate::usb::{check_status, Transport};
use anyhow::{bail, Context, Result};

pub const PARTITION_TLS: u8 = 1;
pub const TLS_FLASH_SIZE: u32 = 0x1000;

#[derive(Debug, Clone)]
pub struct PartitionInfo {
    pub id: u8,
    pub kind: u8,
    pub access_lvl: u16,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct FlashInfo {
    pub jedec_id: (u16, u16),
    pub blocks: u16,
    pub blocksize: u16,
    pub partitions: Vec<PartitionInfo>,
}

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub kind: u16,
    pub subtype: u16,
    pub major: u16,
    pub minor: u16,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct FirmwareInfo {
    pub major: u16,
    pub minor: u16,
    pub buildtime: u32,
    pub modules: Vec<ModuleInfo>,
}

/// `0x3e` — enumerate flash chip geometry and partition table.
pub fn get_flash_info(t: &mut impl Transport) -> Result<FlashInfo> {
    let rsp = t.cmd(&[0x3e])?;
    check_status(&rsp).context("flash info command (0x3e)")?;
    let body = &rsp[2..];

    if body.len() < 0xe {
        bail!("flash info reply too short: {} bytes", body.len());
    }
    let rd16 = |o: usize| u16::from_le_bytes([body[o], body[o + 1]]);
    let (jid0, jid1, blocks, blocksize, pcnt) = (rd16(0), rd16(2), rd16(4), rd16(8), rd16(12));

    let table = &body[0xe..];
    let mut partitions = Vec::new();
    for i in 0..pcnt as usize {
        let e = table.get(i * 0xc..(i + 1) * 0xc);
        let Some(e) = e else { break };
        partitions.push(PartitionInfo {
            id: e[0],
            kind: e[1],
            access_lvl: u16::from_le_bytes([e[2], e[3]]),
            offset: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
            size: u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
        });
    }

    Ok(FlashInfo { jedec_id: (jid0, jid1), blocks, blocksize, partitions })
}

/// `0x43` — firmware info for a partition. `None` means no firmware present,
/// which is the expected state before the firmware extension is uploaded.
pub fn get_fw_info(t: &mut impl Transport, partition: u8) -> Result<Option<FirmwareInfo>> {
    let rsp = t.cmd(&[0x43, partition])?;

    // 0x04b0 = "no firmware detected"; a normal condition, not an error.
    if rsp.len() == 2 && rsp[0] == 0xb0 && rsp[1] == 0x04 {
        return Ok(None);
    }
    check_status(&rsp).context("firmware info command (0x43)")?;
    let body = &rsp[2..];
    if body.len() < 0xa {
        bail!("firmware info reply too short: {} bytes", body.len());
    }

    let rd16 = |o: usize| u16::from_le_bytes([body[o], body[o + 1]]);
    let major = rd16(0);
    let minor = rd16(2);
    let modcnt = rd16(4);
    let buildtime = u32::from_le_bytes([body[6], body[7], body[8], body[9]]);

    let table = &body[0xa..];
    let mut modules = Vec::new();
    for i in 0..modcnt as usize {
        let Some(e) = table.get(i * 0xc..(i + 1) * 0xc) else { break };
        modules.push(ModuleInfo {
            kind: u16::from_le_bytes([e[0], e[1]]),
            subtype: u16::from_le_bytes([e[2], e[3]]),
            major: u16::from_le_bytes([e[4], e[5]]),
            minor: u16::from_le_bytes([e[6], e[7]]),
            size: u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
        });
    }

    Ok(Some(FirmwareInfo { major, minor, buildtime, modules }))
}

/// `0x40` — read `size` bytes at `addr` from `partition`.
pub fn read_flash(t: &mut impl Transport, partition: u8, addr: u32, size: u32) -> Result<Vec<u8>> {
    let mut cmd = Vec::with_capacity(13);
    cmd.push(0x40);
    cmd.push(partition);
    cmd.push(1);
    cmd.extend_from_slice(&0u16.to_le_bytes());
    cmd.extend_from_slice(&addr.to_le_bytes());
    cmd.extend_from_slice(&size.to_le_bytes());

    let rsp = t.cmd(&cmd)?;
    check_status(&rsp).context("flash read command (0x40)")?;
    if rsp.len() < 8 {
        bail!("flash read reply too short: {} bytes", rsp.len());
    }

    let sz = u32::from_le_bytes([rsp[2], rsp[3], rsp[4], rsp[5]]) as usize;
    let end = 8usize.checked_add(sz).filter(|e| *e <= rsp.len()).ok_or_else(|| {
        anyhow::anyhow!("flash read claimed {sz} bytes but reply holds {}", rsp.len() - 8)
    })?;
    Ok(rsp[8..end].to_vec())
}

/// Read the pairing record from partition 1. Safe without a TLS session.
pub fn read_tls_flash(t: &mut impl Transport) -> Result<Vec<u8>> {
    read_flash(t, PARTITION_TLS, 0, TLS_FLASH_SIZE)
}
