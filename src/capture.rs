//! Image capture: calibration, capture-program patching and the capture
//! state machine.
//!
//! Several transforms here look wrong and are reproduced deliberately — the
//! sensor firmware expects the Windows driver's exact behaviour, quirks and
//! all. Those sites are marked individually.

use crate::sensor::{CaptureMode, SensorConfig};
use crate::timeslot::{
    decode_insn, find_nth_insn, find_nth_regwrite, merge_chunks, split_chunks, Chunk,
    CHUNK_TIMESLOT_TABLE,
};
use crate::tls::Tls;
use crate::usb::check_status;
use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

/// Chunk appended to configure the reply format.
const CHUNK_REPLY_CONFIG: u16 = 0x17;
/// Chunk carrying the identify-mode finger-detect parameters.
const CHUNK_WTF_4E: u16 = 0x4e;
/// Chunk carrying enroll-mode finger detect.
const CHUNK_FINGER_DETECT: u16 = 0x26;
/// Chunk carrying image reconstruction parameters.
const CHUNK_IMAGE_RECON: u16 = 0x2e;
/// Chunk carrying the interleave factor.
const CHUNK_INTERLEAVE: u16 = 0x44;
/// Chunk carrying the line update table.
const CHUNK_LINE_UPDATE: u16 = 0x30;
/// Chunk carrying the line update transform table.
const CHUNK_LINE_UPDATE_TRANSFORM: u16 = 0x43;

/// Register the calibration path rewrites in the timeslot table.
const REG_CALIBRATION: u32 = 0x8000_203c;

// Opaque parameter blocks lifted from the Windows driver. Their internal
// structure is only partly understood; identify and enroll differ by one byte
// in the reconstruction block.
const IDENTIFY_4E: &str = "fbb20f0000000f00300000008700020067000a00018000000a0200000b1900008813b80b01091000";
const IDENTIFY_2E: &str = "0200180002000000700070004d010000a0008c003c32321e3c0a0202";
const ENROLL_26: &str = "fbb20f0000000f00300000008700020067000a00018000000a0200000b19000050c360ea01091000";
const ENROLL_2E: &str = "0200180023000000700070004d010000a0008c003c32321e3c0a0202";

fn clip(x: i32) -> u8 {
    x.clamp(-128, 127) as u8
}

/// Rescale a raw calibration sample. The divisor is sensor-family specific.
fn scale(x: u8) -> u8 {
    let v = x as i32 - 0x80;
    clip((v as f64 * 10.0 / 34.0) as i32)
}

/// Add two calibration deltas as signed bytes, saturating.
fn add(l: u8, r: u8) -> u8 {
    clip(l as i8 as i32 + r as i8 as i32)
}

fn chunks_of(b: &[u8], l: usize) -> Vec<&[u8]> {
    b.chunks(l).collect()
}

/// Pack values into a dense little-endian bit stream, `u` bits each, biased by
/// the minimum. Returns `(bits, minimum, packed)`.
fn bitpack(b: &[u8]) -> (u8, u8, Vec<u8>) {
    if b.is_empty() {
        return (0, 0, Vec::new());
    }
    let m = *b.iter().min().unwrap();
    let mut span = *b.iter().max().unwrap() - m;

    let mut u = 0u8;
    while span > 0 {
        span >>= 1;
        u += 1;
    }
    if u == 0 {
        return (0, m, Vec::new());
    }

    let total_bits = u as usize * b.len();
    let mut out = vec![0u8; total_bits.div_ceil(8)];
    for (j, v) in b.iter().enumerate() {
        let val = (v - m) as u32;
        for bit in 0..u as usize {
            if val >> bit & 1 == 1 {
                let pos = j * u as usize + bit;
                out[pos / 8] |= 1 << (pos % 8);
            }
        }
    }

    (u, m, out)
}

#[derive(Default, Clone)]
struct Line {
    mask: u32,
    flags: u32,
    data: Vec<u8>,
    v0: u8,
    v1: u8,
    v2: u16,
}

impl Line {
    /// Lines with a transform index above 1 go to the transform chunk instead
    /// of the line-update chunk.
    fn transform_index(&self) -> u32 {
        (self.flags & 0x00f0_0000) >> 0x14
    }
}

/// Holds calibration state across runs.
#[derive(Default)]
pub struct Calibration {
    pub calib_data: Vec<u8>,
}

impl Calibration {
    /// Scale the sensor's timeslot sampling to the configured line multiplier.
    ///
    /// Reproduced from the driver verbatim, including its early exit on any
    /// unrecognised opcode.
    fn patch_timeslot_table(b: &[u8], inc_address: bool, mult: u32) -> Vec<u8> {
        let mut b = b.to_vec();
        let mut i = 0usize;

        while i + 3 < b.len() {
            if b[i] & 0xf8 == 0x10 {
                if b[i + 2] > 1 {
                    b[i + 2] = b[i + 2].wrapping_mul(mult as u8);
                    if inc_address {
                        b[i + 1] = b[i + 1].wrapping_add(1);
                    }
                }
                i += 3;
                continue;
            }
            if b[i] == 0 {
                i += 1;
                continue;
            }
            if b[i] == 7 {
                i += 2;
                continue;
            }
            break;
        }

        b
    }

    /// Point the calibration register at the trim value for the middle of the
    /// sensor, following the last call target in the table.
    fn patch_timeslot_again(&self, b: &[u8], cfg: &SensorConfig) -> Result<Vec<u8>> {
        let mut b = b.to_vec();

        // Locate the last Call instruction before the table terminates.
        let mut pc = 0usize;
        let mut target: Option<u32> = None;
        while pc < b.len() {
            let insn = decode_insn(&b[pc..])?;
            if matches!(insn.op, 1 | 2 | 4) {
                break;
            }
            if insn.op == 11 {
                target = insn.operands.get(1).copied();
            }
            pc += insn.size;
        }
        let Some(target) = target else { return Ok(b) };

        // Then the last write to the calibration register within that routine.
        let mut pc = target as usize;
        let mut found: Option<usize> = None;
        while pc < b.len() {
            let insn = decode_insn(&b[pc..])?;
            if matches!(insn.op, 1 | 2 | 4) {
                break;
            }
            if insn.op == crate::timeslot::OP_REGISTER_WRITE
                && insn.operands.first() == Some(&REG_CALIBRATION)
            {
                found = Some(pc);
            }
            pc += insn.size;
        }
        let Some(found) = found else { return Ok(b) };

        let trim = *cfg
            .factory_calibration_values
            .get(cfg.key_calibration_line as usize)
            .ok_or_else(|| anyhow::anyhow!("key calibration line is outside the trim table"))?;
        b[found + 1] = trim;

        Ok(b)
    }

    /// Average interleaved calibration lines down to one frame.
    fn average(&self, raw: &[u8], cfg: &SensorConfig) -> Result<Vec<u8>> {
        let frame_size = (cfg.lines_per_frame * cfg.bytes_per_line) as usize;
        let interleave = (cfg.lines_per_frame / cfg.type_info.lines_per_calibration_data) as usize;
        let mut input_frames = cfg.calibration_frames as usize;
        let mut base = 0usize;

        if interleave > 1 {
            if input_frames > 1 {
                // The first frame is discarded; the sensor is still settling.
                base = frame_size;
            }
            let frame = raw
                .get(base..base + frame_size)
                .ok_or_else(|| anyhow::anyhow!("calibration data shorter than one frame"))?;

            let mut out = Vec::with_capacity(frame_size / interleave);
            for group in chunks_of(frame, interleave * cfg.bytes_per_line as usize) {
                let lines = chunks_of(group, cfg.bytes_per_line as usize);
                for col in 0..cfg.bytes_per_line as usize {
                    let sum: u32 = lines.iter().filter_map(|l| l.get(col)).map(|v| *v as u32).sum();
                    out.push((sum / lines.len() as u32) as u8);
                }
            }
            Ok(out)
        } else {
            if input_frames > 1 {
                input_frames -= 2;
                base = frame_size * 2;
            }
            let frames = raw
                .get(base..base + frame_size * input_frames)
                .ok_or_else(|| anyhow::anyhow!("calibration data shorter than expected"))?;
            let frames = chunks_of(frames, frame_size);

            let mut out = Vec::with_capacity(frame_size);
            for col in 0..frame_size {
                let sum: u32 = frames.iter().filter_map(|f| f.get(col)).map(|v| *v as u32).sum();
                out.push((sum / input_frames as u32) as u8);
            }
            Ok(out)
        }
    }

    /// Fold a calibration frame into the accumulated calibration data.
    ///
    /// The first 8 bytes of each line are a header and are never scaled or
    /// combined.
    fn process_calibration_results(&mut self, cooked: &[u8], cfg: &SensorConfig) {
        let bpl = cfg.bytes_per_line as usize;

        let mut frame = Vec::with_capacity(cooked.len());
        for line in chunks_of(cooked, bpl) {
            let split = line.len().min(8);
            frame.extend_from_slice(&line[..split]);
            frame.extend(line[split..].iter().map(|v| scale(*v)));
        }

        if self.calib_data.is_empty() {
            self.calib_data = frame;
            return;
        }

        let prev = chunks_of(&self.calib_data, bpl);
        let next = chunks_of(&frame, bpl);
        let mut combined = Vec::with_capacity(self.calib_data.len());

        for (ll, rr) in prev.iter().zip(next.iter()) {
            let split = ll.len().min(8);
            combined.extend_from_slice(&ll[..split]);
            for (l, r) in ll[split..].iter().zip(rr[split.min(rr.len())..].iter()) {
                combined.push(add(*l, *r));
            }
        }
        self.calib_data = combined;
    }

    /// The calibration line used to seed the patched timeslot table.
    fn get_key_line(&self, cfg: &SensorConfig) -> Vec<u8> {
        let width = cfg.type_info.line_width as usize;
        if self.calib_data.is_empty() {
            return vec![0u8; width];
        }

        let bpcl = self.calib_data.len() / cfg.type_info.lines_per_calibration_data as usize;
        let off = 8 + bpcl * cfg.key_calibration_line as usize;
        let mut key: Vec<u8> =
            self.calib_data.get(off..off + width).unwrap_or(&[]).to_vec();
        key.resize(width, 0);

        // The value 5 is reserved by the firmware; nudge it out of the way.
        for v in key.iter_mut() {
            if *v == 5 {
                *v -= 1;
            }
        }
        key
    }

    /// Patch the capture program for the requested mode (type-1 sensors).
    fn line_update_type_1(
        &self,
        mode: CaptureMode,
        mut chunks: Vec<Chunk>,
        cfg: &SensorConfig,
    ) -> Result<Vec<Chunk>> {
        let width = cfg.type_info.line_width as usize;
        let mut tst = Vec::new();

        for c in chunks.iter_mut() {
            if c.kind != CHUNK_TIMESLOT_TABLE {
                continue;
            }
            let mut patched =
                Self::patch_timeslot_table(&c.body, true, cfg.type_info.repeat_multiplier);
            if mode != CaptureMode::Calibrate {
                patched = self.patch_timeslot_again(&patched, cfg)?;
            }
            tst = patched.clone();

            // The head of the table is replaced by the calibration key line.
            let mut body = self.get_key_line(cfg);
            body.extend_from_slice(patched.get(width..).unwrap_or(&[]));
            c.body = body;
        }

        if tst.is_empty() {
            bail!("capture program has no timeslot table chunk");
        }

        chunks.push(Chunk { kind: CHUNK_REPLY_CONFIG, body: Vec::new() });

        match mode {
            CaptureMode::Identify => {
                chunks.push(Chunk { kind: CHUNK_WTF_4E, body: hex::decode(IDENTIFY_4E)? });
                chunks.push(Chunk { kind: CHUNK_IMAGE_RECON, body: hex::decode(IDENTIFY_2E)? });
            }
            CaptureMode::Enroll => {
                chunks.push(Chunk { kind: CHUNK_FINGER_DETECT, body: hex::decode(ENROLL_26)? });
                chunks.push(Chunk { kind: CHUNK_IMAGE_RECON, body: hex::decode(ENROLL_2E)? });
            }
            CaptureMode::Calibrate => {}
        }

        chunks.push(Chunk { kind: CHUNK_INTERLEAVE, body: 1u32.to_le_bytes().to_vec() });

        let mut lines: Vec<Line> = Vec::new();
        // Transform slots 0 and 1 are reserved, so user entries start at 2.
        let mut cnt = 2u32;

        // Sensor calibration blob, anchored at the 2nd "Enable Rx".
        let (pc, _) = find_nth_insn(&tst, 6, 2)?
            .ok_or_else(|| anyhow::anyhow!("timeslot table has no second Enable Rx"))?;
        lines.push(Line {
            mask: 0xff,
            flags: (pc as u32 + 1) | (cnt << 0x14) | 0x0700_0000,
            data: hex::decode(cfg.type_info.calibration_blob)?,
            v0: 0xf,
            ..Default::default()
        });
        cnt += 1;

        // Factory trim values, bit-packed, anchored at the calibration register write.
        let (pc, _) = find_nth_regwrite(&tst, REG_CALIBRATION, 1)?
            .ok_or_else(|| anyhow::anyhow!("timeslot table has no calibration register write"))?;
        let (u, m, packed) = bitpack(&cfg.factory_calibration_values);
        lines.push(Line {
            mask: 0xff,
            flags: (pc as u32 + 1) | (cnt << 0x14) | 0x0700_0000,
            data: packed,
            v0: u.wrapping_sub(1) | 8,
            v1: m,
            ..Default::default()
        });

        // Accumulated per-pixel calibration, transposed into 4-column stripes.
        if !self.calib_data.is_empty() {
            let bpcl = self.calib_data.len() / cfg.type_info.lines_per_calibration_data as usize;
            for i in (0..112).step_by(4) {
                let mut data = Vec::with_capacity(112 * 4);
                for j in 0..112usize {
                    let p = 8 + j * bpcl + i;
                    let slice = self.calib_data.get(p..p + 4).unwrap_or(&[]);
                    data.extend_from_slice(slice);
                    data.resize(data.len() + (4 - slice.len()), 0);
                }
                lines.push(Line {
                    mask: 0xffff_ffff,
                    flags: i as u32 | (0x85 << 24),
                    data,
                    ..Default::default()
                });
            }
        }

        // The sensor reads these tables as dword arrays.
        for l in lines.iter_mut() {
            let pad = l.data.len() % 4;
            if pad > 0 {
                l.data.resize(l.data.len() + (4 - pad), 0);
            }
        }

        let mut line_update = (lines.len() as u32).to_le_bytes().to_vec();
        for l in &lines {
            line_update.extend_from_slice(&l.mask.to_le_bytes());
            line_update.extend_from_slice(&l.flags.to_le_bytes());
        }
        for l in lines.iter().filter(|l| l.transform_index() <= 1) {
            line_update.extend_from_slice(&l.data);
        }
        chunks.push(Chunk { kind: CHUNK_LINE_UPDATE, body: line_update });

        let mut transform = Vec::new();
        for l in lines.iter().filter(|l| l.transform_index() > 1) {
            transform.push(l.v0);
            transform.push(l.v1);
            transform.extend_from_slice(&l.v2.to_le_bytes());
            transform.extend_from_slice(&l.data);
        }
        chunks.push(Chunk { kind: CHUNK_LINE_UPDATE_TRANSFORM, body: transform });

        Ok(chunks)
    }

    /// Build the `0x02` capture command for a mode.
    pub fn build_cmd_02(&self, mode: CaptureMode, cfg: &SensorConfig) -> Result<Vec<u8>> {
        if !cfg.line_update_type1 {
            bail!("only the type-1 line update path is implemented");
        }
        let chunks = self.line_update_type_1(mode, split_chunks(&cfg.capture_prog)?, cfg)?;

        let req_lines: u16 = if mode == CaptureMode::Calibrate {
            (cfg.calibration_frames * cfg.lines_per_frame + 1) as u16
        } else {
            0
        };

        let mut cmd = vec![0x02];
        cmd.extend_from_slice(&(cfg.bytes_per_line as u16).to_le_bytes());
        cmd.extend_from_slice(&req_lines.to_le_bytes());
        cmd.extend_from_slice(&merge_chunks(&chunks));
        Ok(cmd)
    }

    /// Run the calibration iterations. This reads image data only; it does not
    /// write to the sensor.
    pub fn calibrate(&mut self, tls: &mut Tls<'_>, cfg: &SensorConfig) -> Result<()> {
        for i in 0..cfg.calibration_iterations {
            let cmd = self.build_cmd_02(CaptureMode::Calibrate, cfg)?;
            check_status(&tls.cmd(&cmd)?)
                .with_context(|| format!("calibration iteration {i}"))?;

            let raw = tls.usb().read_data(Duration::from_secs(10))?;
            let cooked = self.average(&raw, cfg)?;
            self.process_calibration_results(&cooked, cfg);
        }
        Ok(())
    }
}

/// Wait for an interrupt on EP83, polling until `deadline`.
fn wait_int(tls: &Tls<'_>, deadline: Instant) -> Result<Vec<u8>> {
    loop {
        if let Some(b) = tls.usb().poll_interrupt(Duration::from_millis(100))? {
            return Ok(b);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for a sensor interrupt");
        }
    }
}

/// Result of a successful capture.
#[derive(Debug, Clone, Copy)]
pub struct CaptureResult {
    pub x: u16,
    pub y: u16,
    pub w1: u16,
    pub w2: u16,
}

/// Run a capture, blocking until a finger is presented and scanned.
pub fn capture(
    tls: &mut Tls<'_>,
    calib: &Calibration,
    cfg: &SensorConfig,
    mode: CaptureMode,
    finger_timeout: Duration,
) -> Result<CaptureResult> {
    let cmd = calib.build_cmd_02(mode, cfg)?;
    let result = (|| -> Result<CaptureResult> {
        check_status(&tls.cmd(&cmd)?).context("starting capture")?;

        let start_deadline = Instant::now() + Duration::from_secs(5);
        let b = wait_int(tls, start_deadline)?;
        if b.first() != Some(&0) {
            bail!("unexpected interrupt while starting capture: {}", hex::encode(&b));
        }

        // Wait for the finger.
        let finger_deadline = Instant::now() + finger_timeout;
        loop {
            let b = wait_int(tls, finger_deadline)?;
            if b.first() == Some(&2) {
                break;
            }
        }

        // Wait for the scan to complete.
        let scan_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let b = wait_int(tls, scan_deadline)?;
            if b.first() != Some(&3) {
                bail!("unexpected interrupt during scan: {}", hex::encode(&b));
            }
            if b.get(2).is_some_and(|v| v & 4 != 0) {
                break;
            }
        }

        // 0x51 with the 0x20 selector returns capture geometry and error state.
        let res = tls.cmd(&hex::decode("5100200000")?)?;
        check_status(&res).context("reading capture status")?;
        let body = &res[2..];
        if body.len() < 4 + 12 {
            bail!("capture status reply too short ({} bytes)", body.len());
        }

        let l = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
        let payload = &body[4..];
        if l != payload.len() {
            bail!("capture status length mismatch: {l} != {}", payload.len());
        }

        let rd = |o: usize| u16::from_le_bytes([payload[o], payload[o + 1]]);
        let error = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
        if error != 0 {
            bail!("scan failed with error {error:#010x}");
        }

        Ok(CaptureResult { x: rd(0), y: rd(2), w1: rd(4), w2: rd(6) })
    })();

    // Always stop the capture, even on failure.
    let _ = tls.cmd(&[0x04]);
    result
}

/// Partition holding the "clean slate" blank reference image.
pub const PARTITION_CALIBRATION: u8 = 6;
/// Magic word at the head of a valid clean-slate record.
const CLEAN_SLATE_MAGIC: u16 = 0x5002;

/// Check whether the sensor already holds a valid blank reference image.
///
/// Read-only. Capture depends on this being present; without it the sensor
/// cannot subtract its own baseline.
pub fn check_clean_slate(tls: &mut Tls<'_>) -> Result<bool> {
    use crate::flash::{read_flash, read_flash_all};

    let head = read_flash(tls, PARTITION_CALIBRATION, 0, 0x44)?;
    if head.len() < 0x44 {
        return Ok(false);
    }

    let magic = u16::from_le_bytes([head[0], head[1]]);
    let len = u16::from_le_bytes([head[2], head[3]]) as u32;
    if magic != CLEAN_SLATE_MAGIC {
        return Ok(false);
    }

    let digest = &head[4..0x24];
    if head[0x24..0x44].iter().any(|b| *b != 0) {
        return Ok(false);
    }

    let img = read_flash_all(tls, PARTITION_CALIBRATION, 0x44, len)?;
    Ok(crate::crypto::sha256(&img) == digest)
}

// Front-panel LED patterns. Opaque parameter blocks from the Windows driver.
const GLOW_START_SCAN: &str = "3920bf0200ffff0000019900200000000099990000000000000000000000000020000000000000000000000000ffff000000990020000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
const GLOW_END_SCAN: &str = "39f4010000f401000001ff002000000000ffff0000000000000000000000000020000000000000000000000000f401000000ff0020000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

pub fn glow_start_scan(tls: &mut Tls<'_>) -> Result<()> {
    check_status(&tls.cmd(&hex::decode(GLOW_START_SCAN)?)?).context("glow start")
}

pub fn glow_end_scan(tls: &mut Tls<'_>) -> Result<()> {
    check_status(&tls.cmd(&hex::decode(GLOW_END_SCAN)?)?).context("glow end")
}

/// Parse a tag/length/value dictionary.
fn parse_dict(mut b: &[u8]) -> Result<std::collections::HashMap<u16, Vec<u8>>> {
    let mut out = std::collections::HashMap::new();
    while !b.is_empty() {
        if b.len() < 4 {
            bail!("truncated dictionary entry");
        }
        let t = u16::from_le_bytes([b[0], b[1]]);
        let l = u16::from_le_bytes([b[2], b[3]]) as usize;
        let v = b.get(4..4 + l).ok_or_else(|| anyhow::anyhow!("dictionary value truncated"))?;
        out.insert(t, v.to_vec());
        b = &b[4 + l..];
    }
    Ok(out)
}

/// A successful on-chip match.
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub user_dbid: u32,
    pub subtype: u16,
    pub hash: Vec<u8>,
}

/// Ask the sensor to match the last captured image against every enrolled
/// template. Matching happens on-chip; the host never sees image data.
pub fn match_finger(tls: &mut Tls<'_>) -> Result<MatchResult> {
    let mut cmd = vec![0x5e, 0x02, 0xff];
    cmd.extend_from_slice(&0u16.to_le_bytes()); // any storage
    cmd.extend_from_slice(&0u16.to_le_bytes()); // any user
    cmd.extend_from_slice(&1u16.to_le_bytes());
    cmd.extend_from_slice(&0u16.to_le_bytes());
    cmd.extend_from_slice(&0u16.to_le_bytes());

    let result = (|| -> Result<MatchResult> {
        check_status(&tls.cmd(&cmd)?).context("starting match")?;

        let b = wait_int(tls, Instant::now() + Duration::from_secs(10))?;
        if b.first() != Some(&3) {
            bail!("finger not recognised (interrupt {})", hex::encode(&b));
        }

        let rsp = tls.cmd(&hex::decode("6000000000")?)?;
        check_status(&rsp).context("reading match result")?;
        let body = &rsp[2..];
        if body.len() < 2 {
            bail!("match result reply too short");
        }

        let l = u16::from_le_bytes([body[0], body[1]]) as usize;
        let payload = &body[2..];
        if l != payload.len() {
            bail!("match result length mismatch: {l} != {}", payload.len());
        }

        let dict = parse_dict(payload)?;
        let user = dict.get(&1).ok_or_else(|| anyhow::anyhow!("match result has no user id"))?;
        let subtype =
            dict.get(&3).ok_or_else(|| anyhow::anyhow!("match result has no finger subtype"))?;
        let hash = dict.get(&4).cloned().unwrap_or_default();

        if user.len() < 4 || subtype.len() < 2 {
            bail!("match result fields are truncated");
        }

        Ok(MatchResult {
            user_dbid: u32::from_le_bytes([user[0], user[1], user[2], user[3]]),
            subtype: u16::from_le_bytes([subtype[0], subtype[1]]),
            hash,
        })
    })();

    // Release the match context regardless of outcome.
    let _ = tls.cmd(&hex::decode("6200000000").unwrap_or_default());
    result
}
