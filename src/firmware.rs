//! Firmware extension handling.
//!
//! These sensors need a firmware extension ("fwext") resident in flash
//! partition 2 before they can capture. The blob is proprietary and cannot be
//! redistributed: it is extracted from Lenovo's Windows driver installer, and
//! `scripts/fetch-firmware.sh` automates that.
//!
//! A sensor that has been used under Windows already has firmware, in which
//! case none of this runs.

use crate::flash::{get_fw_info, write_flash_all, write_fw_signature, FirmwareInfo};
use crate::sensor::identify_sensor;
use crate::tls::Tls;
use crate::usb::check_status;
use anyhow::{bail, Context, Result};

/// Partition holding the firmware extension.
pub const PARTITION_FIRMWARE: u8 = 2;

/// The trailing bytes of an xpfwext file are its vendor signature.
const SIGNATURE_LEN: usize = 0x100;

/// Registers poked before an upload. Their meaning is not understood; these
/// are the values the Windows driver writes.
const REG_UPLOAD_ENABLE: u32 = 0x8000_205c;
const REG_UPLOAD_STATUS: u32 = 0x8000_2080;

/// Where firmware files are looked for, in order.
pub const FIRMWARE_DIRS: &[&str] = &["/usr/share/validity-fprintd", "/var/lib/validity-fprintd"];

/// The firmware file name for a device, and the installer it comes from.
pub struct FirmwareSource {
    pub file_name: &'static str,
    pub installer_url: &'static str,
    pub installer_sha512: &'static str,
}

/// The firmware file must match the driver rather than the hardware: both the
/// DLL and the xpfwext are universal, and device-specific code is loaded
/// dynamically through the signed blobs.
pub fn firmware_source(vid: u16, pid: u16) -> Option<FirmwareSource> {
    match (vid, pid) {
        (0x138a, 0x0090) => Some(FirmwareSource {
            file_name: "6_07f_Lenovo.xpfwext",
            installer_url: "https://download.lenovo.com/pccbbs/mobiles/n1cgn08w.exe",
            installer_sha512: "d839fa65adf4c952ecb4a5c4b2fc5b5bdedd8e02a421564bdc7fae1d281be4ea26fcde2333f2ab78d56cef0fdccce0a3cf429300b89544cdc9cfee6d0fe0db55",
        }),
        (0x138a, 0x0097) | (0x138a, 0x009d) | (0x06cb, 0x009a) => Some(FirmwareSource {
            file_name: "6_07f_lenovo_mis_qm.xpfwext",
            installer_url: "https://download.lenovo.com/pccbbs/mobiles/nz3gf07w.exe",
            installer_sha512: "a4a4e6058b1ea8ab721953d2cfd775a1e7bc589863d160e5ebbb90344858f147d695103677a8df0b2de0c95345df108bda97196245b067f45630038fb7c807cd",
        }),
        _ => None,
    }
}

/// Find an already-extracted firmware file for this device.
pub fn find_firmware_file(vid: u16, pid: u16) -> Option<std::path::PathBuf> {
    let source = firmware_source(vid, pid)?;
    FIRMWARE_DIRS
        .iter()
        .map(|d| std::path::Path::new(d).join(source.file_name))
        .find(|p| p.is_file())
}

/// `0x07` — read a 32-bit hardware register.
pub fn read_hw_reg32(tls: &mut Tls, addr: u32) -> Result<u32> {
    let mut cmd = vec![0x07];
    cmd.extend_from_slice(&addr.to_le_bytes());
    cmd.push(4);

    let rsp = tls.cmd(&cmd)?;
    check_status(&rsp).context("reading hardware register")?;
    if rsp.len() < 6 {
        bail!("register read reply too short");
    }
    Ok(u32::from_le_bytes([rsp[2], rsp[3], rsp[4], rsp[5]]))
}

/// `0x08` — write a 32-bit hardware register.
pub fn write_hw_reg32(tls: &mut Tls, addr: u32, val: u32) -> Result<()> {
    let mut cmd = vec![0x08];
    cmd.extend_from_slice(&addr.to_le_bytes());
    cmd.extend_from_slice(&val.to_le_bytes());
    cmd.push(4);
    check_status(&tls.cmd(&cmd)?).context("writing hardware register")
}

/// Current firmware extension, if any.
pub fn current(tls: &mut Tls) -> Result<Option<FirmwareInfo>> {
    get_fw_info(tls, PARTITION_FIRMWARE)
}

/// Split an xpfwext file into its payload and trailing signature.
///
/// The file begins with a text header terminated by a 0x1a byte; everything
/// after that is the payload, and its last 256 bytes are the signature.
pub fn parse_xpfwext(raw: &[u8]) -> Result<(&[u8], &[u8])> {
    let start = raw
        .iter()
        .position(|b| *b == 0x1a)
        .ok_or_else(|| anyhow::anyhow!("not an xpfwext file: no 0x1a header terminator"))?
        + 1;

    let body = raw
        .get(start..)
        .filter(|b| b.len() > SIGNATURE_LEN)
        .ok_or_else(|| anyhow::anyhow!("xpfwext file is too short"))?;

    Ok(body.split_at(body.len() - SIGNATURE_LEN))
}

/// Upload a firmware extension to the sensor.
///
/// Writes flash partition 2 and then reboots the sensor, which drops it off the
/// USB bus. Callers must reopen the device afterwards. Does nothing if firmware
/// is already present.
pub fn upload(tls: &mut Tls, write_enable_blob: &[u8], fwext: &[u8]) -> Result<bool> {
    if let Some(fw) = current(tls)? {
        eprintln!("firmware: v{}.{} already present, nothing to do", fw.major, fw.minor);
        return Ok(false);
    }

    let (payload, signature) = parse_xpfwext(fwext)?;
    eprintln!("firmware: uploading {} bytes plus signature", payload.len());

    write_hw_reg32(tls, REG_UPLOAD_ENABLE, 7)?;
    let status = read_hw_reg32(tls, REG_UPLOAD_STATUS)?;
    if !matches!(status, 2 | 3) {
        bail!("sensor is not ready for a firmware upload (status register {status:#x})");
    }

    // The Windows driver identifies the sensor here; the reason is unclear, but
    // it is cheap and keeps the sequence faithful.
    let _ = identify_sensor(tls);

    write_flash_all(tls, write_enable_blob, PARTITION_FIRMWARE, 0, payload)?;
    write_fw_signature(tls, PARTITION_FIRMWARE, signature)?;

    let fw = current(tls)?
        .ok_or_else(|| anyhow::anyhow!("firmware still not detected after upload"))?;
    eprintln!(
        "firmware: loaded v{}.{}, {} modules; rebooting the sensor",
        fw.major,
        fw.minor,
        fw.modules.len()
    );

    crate::init::reboot(tls)?;
    Ok(true)
}
