//! The sensor's capture-program format.
//!
//! A capture program is a flat sequence of type-length-value chunks. One chunk
//! (type 0x34) holds a timeslot table: a small instruction stream driving the
//! analog frontend. Calibration works by locating specific instructions in that
//! table and rewriting their operands.

use anyhow::{bail, Result};

/// Chunk type holding the timeslot table.
pub const CHUNK_TIMESLOT_TABLE: u16 = 0x34;
/// Chunk type holding the table's start offset.
pub const CHUNK_TIMESLOT_OFFSET: u16 = 0x29;
/// Chunk type holding lines-per-frame as a u32.
pub const CHUNK_2D: u16 = 0x2f;

/// Opcode index for a register write, the instruction calibration patches.
pub const OP_REGISTER_WRITE: u8 = 13;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub kind: u16,
    pub body: Vec<u8>,
}

pub fn split_chunks(b: &[u8]) -> Result<Vec<Chunk>> {
    let mut out = Vec::new();
    let mut rest = b;

    while !rest.is_empty() {
        if rest.len() < 4 {
            bail!("trailing bytes in capture program: {}", hex::encode(rest));
        }
        let kind = u16::from_le_bytes([rest[0], rest[1]]);
        let sz = u16::from_le_bytes([rest[2], rest[3]]) as usize;
        let body = rest
            .get(4..4 + sz)
            .ok_or_else(|| anyhow::anyhow!("chunk {kind:#06x} claims {sz} bytes, too few remain"))?;
        out.push(Chunk { kind, body: body.to_vec() });
        rest = &rest[4 + sz..];
    }

    Ok(out)
}

pub fn merge_chunks(cs: &[Chunk]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in cs {
        out.extend_from_slice(&c.kind.to_le_bytes());
        out.extend_from_slice(&(c.body.len() as u16).to_le_bytes());
        out.extend_from_slice(&c.body);
    }
    out
}

/// A decoded timeslot instruction: opcode index, encoded size, operands.
#[derive(Debug, Clone)]
pub struct Insn {
    pub op: u8,
    pub size: usize,
    pub operands: Vec<u32>,
}

/// Decode one instruction from the front of `b`.
///
/// The encoding is a prefix code: short opcodes occupy low byte values, and
/// wider families are selected by masking off progressively fewer high bits.
pub fn decode_insn(b: &[u8]) -> Result<Insn> {
    if b.is_empty() {
        bail!("cannot decode an instruction from an empty buffer");
    }
    let b0 = b[0];
    let need = |n: usize| -> Result<()> {
        if b.len() < n {
            bail!("truncated instruction {b0:#04x}: need {n} bytes, have {}", b.len());
        }
        Ok(())
    };
    let mk = |op: u8, size: usize, operands: Vec<u32>| Ok(Insn { op, size, operands });

    match b0 {
        0..=4 => mk(b0, 1, vec![]),
        5 => {
            need(2)?;
            mk(5, 2, vec![b[1] as u32])
        }
        6 => {
            need(2)?;
            mk(6, 2, vec![b[1] as u32])
        }
        7 => {
            need(2)?;
            mk(7, 2, vec![if b[1] == 0 { 0x100 } else { b[1] as u32 }])
        }
        _ if b0 & 0xfe == 0x08 => {
            need(2)?;
            mk(8, 2, vec![((b0 as u32 & 1) << 8) | b[1] as u32])
        }
        _ if b0 & 0xfe == 0x0a => {
            need(2)?;
            mk(9, 2, vec![((b0 as u32 & 1) << 8) | b[1] as u32])
        }
        _ if b0 & 0xfc == 0x0c => mk(10, 1, vec![b0 as u32 & 3]),
        _ if b0 & 0xf8 == 0x10 => {
            need(3)?;
            mk(
                11,
                3,
                vec![
                    b0 as u32 & 7,
                    (b[1] as u32) << 2,
                    if b[2] == 0 { 0x100 } else { b[2] as u32 },
                ],
            )
        }
        _ if b0 & 0xe0 == 0x20 => mk(12, 1, vec![b0 as u32 & 0x1f]),
        _ if b0 & 0xc0 == 0x40 => {
            need(3)?;
            // Register address is a word index off a fixed MMIO base.
            mk(
                OP_REGISTER_WRITE,
                3,
                vec![(b0 as u32 & 0x3f) * 4 + 0x8000_2000, b[1] as u32 | ((b[2] as u32) << 8)],
            )
        }
        _ if b0 & 0xc0 == 0x80 => mk(14, 1, vec![(b0 as u32 & 0x38) >> 3, b0 as u32 & 7]),
        _ => {
            need(2)?;
            mk(
                15,
                2,
                vec![
                    (b0 as u32 & 0x38) >> 3,
                    b0 as u32 & 7,
                    if b[1] == 0 { 0x100 } else { b[1] as u32 },
                ],
            )
        }
    }
}

/// Find the `n`th (1-based) instruction with the given opcode.
/// Returns its offset and encoded length.
pub fn find_nth_insn(b: &[u8], opcode: u8, mut n: usize) -> Result<Option<(usize, usize)>> {
    let mut pc = 0usize;

    while pc < b.len() {
        let insn = decode_insn(&b[pc..])?;
        if insn.op == opcode {
            n -= 1;
            if n == 0 {
                return Ok(Some((pc, insn.size)));
            }
        }
        pc += insn.size;
    }

    Ok(None)
}

/// Find the `n`th (1-based) write to a specific register address.
pub fn find_nth_regwrite(b: &[u8], reg_addr: u32, mut n: usize) -> Result<Option<(usize, usize)>> {
    let mut pc = 0usize;

    while pc < b.len() {
        let insn = decode_insn(&b[pc..])?;
        if insn.op == OP_REGISTER_WRITE && insn.operands.first() == Some(&reg_addr) {
            n -= 1;
            if n == 0 {
                return Ok(Some((pc, insn.size)));
            }
        }
        pc += insn.size;
    }

    Ok(None)
}
