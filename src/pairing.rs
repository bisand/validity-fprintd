//! The on-flash pairing record, and what it tells us about sensor ownership.
//!
//! Partition 1 stores a sequence of length-prefixed, SHA-256-checked blocks.
//! Block 4 holds the host's ECDSA private key, encrypted under a key derived
//! from this machine's DMI identity. If that block fails to authenticate, the
//! sensor belongs to a different host install — typically Windows Hello.

use crate::crypto::{aes_cbc_decrypt_raw, hmac_sha256, sha256, unpad_pkcs7, HostKeys};
use anyhow::{bail, Context, Result};
use std::fs;

pub const BLOCK_EMPTY_0: u16 = 0;
pub const BLOCK_CERT: u16 = 3;
pub const BLOCK_PRIV: u16 = 4;
pub const BLOCK_ECDH: u16 = 6;
pub const BLOCK_END: u16 = 0xffff;

#[derive(Debug, Clone)]
pub struct FlashBlock {
    pub id: u16,
    pub body: Vec<u8>,
}

/// Split the 4 KiB pairing record into verified blocks.
pub fn parse_flash_blocks(data: &[u8]) -> Result<Vec<FlashBlock>> {
    let mut blocks = Vec::new();
    let mut rest = data;

    while rest.len() >= 4 + 32 {
        let id = u16::from_le_bytes([rest[0], rest[1]]);
        let sz = u16::from_le_bytes([rest[2], rest[3]]) as usize;
        if id == BLOCK_END {
            break;
        }

        let digest = &rest[4..4 + 32];
        let after_hdr = &rest[4 + 32..];
        if after_hdr.len() < sz {
            bail!("block {id:#06x} claims {sz} bytes but only {} remain", after_hdr.len());
        }
        let body = &after_hdr[..sz];

        if sha256(body) != digest {
            bail!("block {id:#06x} failed its SHA-256 integrity check");
        }

        blocks.push(FlashBlock { id, body: body.to_vec() });
        rest = &after_hdr[sz..];
    }

    Ok(blocks)
}

/// What state the sensor's pairing record is in.
#[derive(Debug)]
pub enum PairingState {
    /// Paired to this host; the private key decrypted and authenticated.
    PairedToThisHost { private_key_d: [u8; 32] },
    /// A pairing record exists but was sealed by a different host.
    PairedToAnotherHost,
    /// No pairing record present; the sensor is unprovisioned.
    Unpaired,
}

/// Read this machine's DMI identity, which the pairing keys are bound to.
///
/// `product_serial` is root-only; without it the derived keys cannot match a
/// record written by a privileged process, so we surface that rather than
/// silently deriving the wrong key.
pub fn host_identity() -> Result<(String, String)> {
    let read = |p: &str| -> Result<String> {
        Ok(fs::read_to_string(p).with_context(|| format!("reading {p}"))?.trim().to_string())
    };
    let product_name = read("/sys/class/dmi/id/product_name")?;
    let product_serial = read("/sys/class/dmi/id/product_serial")
        .context("product_serial is readable only by root; re-run with sudo")?;
    Ok((product_name, product_serial))
}

/// Decode block 4 and decide who owns this sensor.
pub fn inspect_pairing(blocks: &[FlashBlock], keys: &HostKeys) -> Result<PairingState> {
    let Some(priv_block) = blocks.iter().find(|b| b.id == BLOCK_PRIV) else {
        return Ok(PairingState::Unpaired);
    };

    // An all-zero or absent body means the slot was never provisioned.
    if priv_block.body.iter().all(|b| *b == 0) {
        return Ok(PairingState::Unpaired);
    }

    let body = &priv_block.body;
    if body.is_empty() {
        return Ok(PairingState::Unpaired);
    }
    if body[0] != 2 {
        bail!("unknown private-key record prefix {:#04x}", body[0]);
    }

    let rest = &body[1..];
    if rest.len() < 32 + 16 {
        bail!("private-key record is truncated ({} bytes)", rest.len());
    }
    let (ciphertext, mac) = rest.split_at(rest.len() - 32);

    if hmac_sha256(&keys.psk_validation_key, ciphertext) != mac {
        return Ok(PairingState::PairedToAnotherHost);
    }

    let (iv, ct) = ciphertext.split_at(16);
    let plain = aes_cbc_decrypt_raw(&keys.psk_encryption_key, iv, ct)?;
    let plain = unpad_pkcs7(&plain)?;

    if plain.len() < 0x60 {
        bail!("decrypted private-key record is too short ({} bytes)", plain.len());
    }

    // x, y, d are each 32 bytes, stored little-endian.
    let mut private_key_d = [0u8; 32];
    private_key_d.copy_from_slice(&plain[0x40..0x60]);
    private_key_d.reverse();

    Ok(PairingState::PairedToThisHost { private_key_d })
}

/// Synaptics' firmware signing key, hardcoded in `synaWudfBioUsb.dll`.
///
/// The matching private key should only exist inside genuine Synaptics
/// hardware, so a valid signature over block 6 authenticates the sensor.
const FW_PUBKEY_X: [u8; 32] = [
    0xf7, 0x27, 0x65, 0x3b, 0x4e, 0x16, 0xce, 0x06, 0x65, 0xa6, 0x89, 0x4d, 0x7f, 0x3a, 0x30, 0xd7,
    0xd0, 0xa0, 0xbe, 0x31, 0x0d, 0x12, 0x92, 0xa7, 0x43, 0x67, 0x1f, 0xdf, 0x69, 0xf6, 0xa8, 0xd3,
];
const FW_PUBKEY_Y: [u8; 32] = [
    0xa8, 0x55, 0x38, 0xf8, 0xb6, 0xbe, 0xc5, 0x0d, 0x6e, 0xef, 0x8b, 0xd5, 0xf4, 0xd0, 0x7a, 0x88,
    0x62, 0x43, 0xc5, 0x8b, 0x23, 0x93, 0x94, 0x8d, 0xf7, 0x61, 0xa8, 0x47, 0x21, 0xa6, 0xca, 0x94,
];

/// Everything needed to open a session, recovered from the pairing record.
pub struct Material {
    pub private_key_d: [u8; 32],
    pub tls_cert: Vec<u8>,
    pub ecdh_public: p256::PublicKey,
    /// Whether block 6 carried a valid Synaptics signature.
    pub firmware_signature_valid: bool,
}

use p256::elliptic_curve::sec1::FromEncodedPoint;

fn point_from_le(x_le: &[u8], y_le: &[u8]) -> Result<p256::EncodedPoint> {
    let mut x = [0u8; 32];
    let mut y = [0u8; 32];
    x.copy_from_slice(x_le);
    y.copy_from_slice(y_le);
    x.reverse();
    y.reverse();
    Ok(p256::EncodedPoint::from_affine_coordinates(&x.into(), &y.into(), false))
}

/// Decode blocks 3, 4 and 6 into usable session material.
pub fn build_material(blocks: &[FlashBlock], keys: &HostKeys) -> Result<Material> {
    let private_key_d = match inspect_pairing(blocks, keys)? {
        PairingState::PairedToThisHost { private_key_d } => private_key_d,
        PairingState::PairedToAnotherHost => {
            bail!("sensor is paired to a different host; cannot open a session")
        }
        PairingState::Unpaired => bail!("sensor is unpaired; cannot open a session"),
    };

    let tls_cert = blocks
        .iter()
        .find(|b| b.id == BLOCK_CERT)
        .ok_or_else(|| anyhow::anyhow!("pairing record has no certificate block"))?
        .body
        .clone();

    let ecdh = blocks
        .iter()
        .find(|b| b.id == BLOCK_ECDH)
        .ok_or_else(|| anyhow::anyhow!("pairing record has no ECDH block"))?;

    let ecdh_public = ecdh_public_from_block(&ecdh.body)?;

    if ecdh.body.len() < 0x90 {
        bail!("ECDH block is truncated ({} bytes)", ecdh.body.len());
    }
    let (key_blob, sig_blob) = ecdh.body.split_at(0x90);
    let firmware_signature_valid = verify_fw_signature(key_blob, sig_blob).unwrap_or(false);

    Ok(Material { private_key_d, tls_cert, ecdh_public, firmware_signature_valid })
}

fn verify_fw_signature(key_blob: &[u8], sig_blob: &[u8]) -> Result<bool> {
    use p256::ecdsa::signature::Verifier;

    if sig_blob.len() < 4 {
        return Ok(false);
    }
    let len = u32::from_le_bytes([sig_blob[0], sig_blob[1], sig_blob[2], sig_blob[3]]) as usize;
    let Some(der) = sig_blob.get(4..4 + len) else { return Ok(false) };

    let point = p256::EncodedPoint::from_affine_coordinates(
        &FW_PUBKEY_X.into(),
        &FW_PUBKEY_Y.into(),
        false,
    );
    let Ok(vk) = p256::ecdsa::VerifyingKey::from_encoded_point(&point) else {
        return Ok(false);
    };

    let Ok(sig) = p256::ecdsa::Signature::from_der(der) else { return Ok(false) };
    Ok(vk.verify(key_blob, &sig).is_ok())
}

/// Extract the sensor's static ECDH public point from a block-6 body.
///
/// Coordinates sit at fixed offsets inside the key structure, little-endian.
pub fn ecdh_public_from_block(body: &[u8]) -> Result<p256::PublicKey> {
    if body.len() < 0x90 {
        bail!("ECDH block is truncated ({} bytes)", body.len());
    }
    let point = point_from_le(&body[0x08..0x28], &body[0x4c..0x6c])?;
    Option::<p256::PublicKey>::from(p256::PublicKey::from_encoded_point(&point))
        .ok_or_else(|| anyhow::anyhow!("ECDH block does not describe a point on P-256"))
}
