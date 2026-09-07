//! Device bring-up prior to opening a session.

use crate::blobs::blobs_for;
use crate::usb::{check_status, Usb};
use anyhow::{Context, Result};

/// Run the pre-session init handshake.
///
/// This replays the vendor's signed init blob, which the sensor requires before
/// it will negotiate a session. If the firmware extension is absent the sensor
/// needs the "clean slate" variant instead.
pub fn send_init(usb: &Usb) -> Result<()> {
    check_status(&usb.cmd(&[0x01])?).context("ROM info command (0x01)")?;
    check_status(&usb.cmd(&[0x19])?).context("init command (0x19)")?;

    // Firmware-extension status; interpreted after the init blob is accepted.
    let fw_rsp = usb.cmd(&[0x43, 0x02])?;

    let blobs = blobs_for(usb.vid, usb.pid)
        .ok_or_else(|| anyhow::anyhow!("no init blobs for {:04x}:{:04x}", usb.vid, usb.pid))?;

    check_status(&usb.cmd(&blobs.init_hardcoded())?).context("signed init blob")?;

    if fw_rsp.len() >= 2 && u16::from_le_bytes([fw_rsp[0], fw_rsp[1]]) != 0 {
        // No firmware extension loaded yet.
        usb.cmd(&blobs.init_hardcoded_clean_slate())?;
    }

    Ok(())
}

/// Ask the sensor to reboot.
///
/// The sensor allocates a fresh context per session and does not reclaim them,
/// so a long-running host should reboot it on the way out. The link drops as it
/// restarts, which means a missing or truncated reply is the normal case.
pub fn reboot(t: &mut impl crate::usb::Transport) -> Result<()> {
    let _ = t.cmd(&[0x05, 0x02, 0x00]);
    Ok(())
}

/// Bring the device up and open an encrypted session in one step.
pub fn open_session(usb: &mut Usb) -> Result<(crate::tls::Tls<'_>, bool)> {
    use crate::crypto::HostKeys;
    use crate::flash::read_tls_flash;
    use crate::pairing::{build_material, host_identity, parse_flash_blocks};
    use crate::tls::{PairingMaterial, Tls};

    send_init(usb)?;
    let blocks = parse_flash_blocks(&read_tls_flash(usb)?)?;
    let (product_name, product_serial) = host_identity()?;
    let keys = HostKeys::derive(&product_name, &product_serial);
    let material = build_material(&blocks, &keys)?;
    let signature_valid = material.firmware_signature_valid;

    let mut tls = Tls::new(
        &*usb,
        PairingMaterial {
            private_key_d: material.private_key_d,
            tls_cert: material.tls_cert,
            ecdh_public: material.ecdh_public,
        },
    );
    tls.open()?;
    Ok((tls, signature_valid))
}
