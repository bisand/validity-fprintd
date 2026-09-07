//! Sensor identification, factory calibration data and capture-program setup.

use crate::tables::{
    capture_prog, dev_info_lookup, sensor_type_info, RomInfo, SensorTypeInfo,
};
use crate::timeslot::{split_chunks, CHUNK_2D};
use crate::usb::{check_status, Transport};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;

/// Sensors whose capture program is patched by the "type 1" line-update path.
pub const LINE_UPDATE_TYPE1_DEVICES: &[u32] =
    &[0xb5, 0x885, 0xb3, 0x143b, 0x1055, 0xe1, 0x8b1, 0xea, 0xe4, 0xed, 0x1825, 0x1ff5, 0x199];

/// Capture-program selectors. Their origin in the Windows driver is unknown;
/// these values are what it passes for this sensor family.
const PROG_A0: u32 = 0x18;
const PROG_A1: u32 = 0x19;

/// Factory calibration tag group holding sensor trim values.
pub const FACTORY_TAG_CALIBRATION: u16 = 0x0e00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Calibrate = 1,
    Identify = 2,
    Enroll = 3,
}

/// `0x01` — ROM build information.
pub fn rom_info(t: &mut impl Transport) -> Result<RomInfo> {
    let rsp = t.cmd(&[0x01])?;
    check_status(&rsp).context("ROM info command (0x01)")?;
    let b = &rsp[2..];
    if b.len() < 0x10 {
        bail!("ROM info reply too short ({} bytes)", b.len());
    }

    Ok(RomInfo {
        timestamp: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        build: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        major: b[8] as u32,
        minor: b[9] as u32,
        product: b[11] as u32,
        u1: b[15] as u32,
    })
}

/// `0x75` — sensor model identification.
pub fn identify_sensor(t: &mut impl Transport) -> Result<(&'static str, u32)> {
    let rsp = t.cmd(&[0x75])?;
    check_status(&rsp).context("identify sensor command (0x75)")?;
    let b = &rsp[2..];
    if b.len() < 8 {
        bail!("identify reply too short ({} bytes)", b.len());
    }

    let zeroes = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    if zeroes != 0 {
        bail!("unexpected leading field {zeroes:#x} in identify reply");
    }
    let minor = u16::from_le_bytes([b[4], b[5]]) as u32;
    let major = u16::from_le_bytes([b[6], b[7]]) as u32;

    let info = dev_info_lookup(major, minor)
        .ok_or_else(|| anyhow::anyhow!("unknown sensor: major {major:#x}, version {minor:#x}"))?;
    Ok((info.name, info.dev_type))
}

/// `0x6f` — read a group of factory-programmed calibration values, keyed by subtag.
pub fn get_factory_bits(t: &mut impl Transport, tag: u16) -> Result<HashMap<u16, Vec<u8>>> {
    let mut cmd = vec![0x6f];
    cmd.extend_from_slice(&tag.to_le_bytes());
    cmd.extend_from_slice(&0u16.to_le_bytes());
    cmd.extend_from_slice(&0u32.to_le_bytes());

    let rsp = t.cmd(&cmd)?;
    check_status(&rsp).context("factory bits command (0x6f)")?;
    let mut b = &rsp[2..];
    if b.len() < 8 {
        bail!("factory bits reply too short ({} bytes)", b.len());
    }

    let entries = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    b = &b[8..];

    let mut out = HashMap::new();
    for _ in 0..entries {
        if b.len() < 12 {
            bail!("factory bits entry header truncated");
        }
        let l = u16::from_le_bytes([b[4], b[5]]) as usize;
        let subtag = u16::from_le_bytes([b[8], b[9]]);
        b = &b[12..];

        let value = b.get(..l).ok_or_else(|| anyhow::anyhow!("factory bits value truncated"))?;
        out.insert(subtag, value.to_vec());
        b = &b[l..];
    }

    Ok(out)
}

/// Everything the capture path needs to know about this particular sensor.
pub struct SensorConfig {
    pub device_name: &'static str,
    pub sensor_type: u32,
    pub rom: RomInfo,
    pub type_info: &'static SensorTypeInfo,
    pub capture_prog: Vec<u8>,
    pub lines_per_frame: u32,
    pub bytes_per_line: u32,
    pub key_calibration_line: u32,
    pub calibration_frames: u32,
    pub calibration_iterations: u32,
    pub factory_calibration_values: Vec<u8>,
    pub factory_calib_data: Option<Vec<u8>>,
    pub line_update_type1: bool,
}

impl SensorConfig {
    pub fn probe(t: &mut impl Transport) -> Result<Self> {
        let (device_name, sensor_type) = identify_sensor(t)?;

        // These constants are per sensor family and are not discoverable.
        let (key_calibration_line, calibration_frames, calibration_iterations) = match sensor_type {
            0x199 => (0x38, 3, 3),
            0xdb => (0x48, 6, 0),
            other => bail!("sensor type {other:#x} ({device_name}) is not supported"),
        };

        let type_info = sensor_type_info(sensor_type)
            .ok_or_else(|| anyhow::anyhow!("no geometry table for sensor type {sensor_type:#x}"))?;

        let rom = rom_info(t)?;
        if rom.product != 0x30 {
            bail!("ROM product {:#x} is not supported", rom.product);
        }

        let prog = capture_prog(&rom, sensor_type, PROG_A0, PROG_A1).ok_or_else(|| {
            anyhow::anyhow!(
                "no capture program for ROM major {:#x} build {:#x} and sensor {sensor_type:#x}",
                rom.major,
                rom.build
            )
        })?;

        // The "2D" chunk carries lines-per-frame as a little-endian u32.
        let lines_2d = split_chunks(&prog)?
            .into_iter()
            .find(|c| c.kind == CHUNK_2D)
            .and_then(|c| c.body.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            .ok_or_else(|| anyhow::anyhow!("capture program has no 2D chunk"))?;

        let factory_bits = get_factory_bits(t, FACTORY_TAG_CALIBRATION)?;
        let factory_calibration_values = factory_bits
            .get(&3)
            .ok_or_else(|| anyhow::anyhow!("factory calibration subtag 3 is missing"))?
            .get(4..)
            .unwrap_or_default()
            .to_vec();
        let factory_calib_data =
            factory_bits.get(&7).map(|v| v.get(4..).unwrap_or_default().to_vec());

        Ok(Self {
            device_name,
            sensor_type,
            rom,
            type_info,
            lines_per_frame: lines_2d * type_info.repeat_multiplier,
            bytes_per_line: type_info.bytes_per_line,
            capture_prog: prog,
            key_calibration_line,
            calibration_frames,
            calibration_iterations,
            factory_calibration_values,
            factory_calib_data,
            line_update_type1: LINE_UPDATE_TYPE1_DEVICES.contains(&sensor_type),
        })
    }
}
