//! Key-derivation and symmetric primitives used by the Validity session layer.
//!
//! The sensor speaks a bespoke dialect of TLS 1.2. Its PRF is the standard
//! TLS 1.2 P_SHA256 construction, and the pre-TLS "pairing" keys are derived
//! from a hardcoded password mixed with host DMI identifiers.

use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{bail, Result};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// Password baked into `synaWudfBioUsb.dll`; identical across all supported sensors.
pub const PASSWORD_HARDCODED: [u8; 32] = [
    0x71, 0x7c, 0xd7, 0x2d, 0x09, 0x62, 0xbc, 0x4a, 0x28, 0x46, 0x13, 0x8d, 0xbb, 0x2c, 0x24, 0x19,
    0x25, 0x12, 0xa7, 0x64, 0x07, 0x06, 0x5f, 0x38, 0x38, 0x46, 0x13, 0x9d, 0x4b, 0xec, 0x20, 0x33,
];

/// Salt for the pairing-blob validation key.
pub const GWK_SIGN_HARDCODED: [u8; 32] = [
    0x3a, 0x4c, 0x76, 0xb7, 0x6a, 0x97, 0x98, 0x1d, 0x12, 0x74, 0x24, 0x7e, 0x16, 0x66, 0x10, 0xe7,
    0x7f, 0x4d, 0x9c, 0x9d, 0x07, 0xd3, 0xc7, 0x28, 0xe5, 0x32, 0x91, 0x6b, 0xdd, 0x28, 0xb4, 0x54,
];

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(data);
    m.finalize().into_bytes().into()
}

/// TLS 1.2 P_SHA256 pseudo-random function.
///
/// Note this follows the reference implementation's iteration order, which
/// advances `a` *after* emitting each block.
pub fn prf(secret: &[u8], seed: &[u8], length: usize) -> Vec<u8> {
    let blocks = length.div_ceil(0x20);
    let mut out = Vec::with_capacity(blocks * 0x20);
    let mut a = hmac(secret, seed);

    for _ in 0..blocks {
        let mut chained = Vec::with_capacity(a.len() + seed.len());
        chained.extend_from_slice(&a);
        chained.extend_from_slice(seed);
        out.extend_from_slice(&hmac(secret, &chained));
        a = hmac(secret, &a);
    }

    out.truncate(length);
    out
}

/// The two pre-TLS keys that unlock the on-flash pairing record.
///
/// Both are bound to the host via `product_name` and `product_serial`, which is
/// why a sensor paired under Windows will not open here.
pub struct HostKeys {
    pub psk_encryption_key: Vec<u8>,
    pub psk_validation_key: Vec<u8>,
}

impl HostKeys {
    pub fn derive(product_name: &str, serial_number: &str) -> Self {
        let mut hw_key = Vec::new();
        hw_key.extend_from_slice(product_name.as_bytes());
        hw_key.push(0);
        hw_key.extend_from_slice(serial_number.as_bytes());
        hw_key.push(0);

        let mut gwk_seed = b"GWK".to_vec();
        gwk_seed.extend_from_slice(&hw_key);
        let psk_encryption_key = prf(&PASSWORD_HARDCODED, &gwk_seed, 0x20);

        let mut sign_seed = b"GWK_SIGN".to_vec();
        sign_seed.extend_from_slice(&GWK_SIGN_HARDCODED);
        let psk_validation_key = prf(&psk_encryption_key, &sign_seed, 0x20);

        Self { psk_encryption_key, psk_validation_key }
    }
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    hmac(key, data)
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

/// AES-256-CBC decrypt. Padding is left intact; callers strip it, because the
/// sensor uses two different padding conventions depending on the record type.
pub fn aes_cbc_decrypt_raw(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() % 16 != 0 {
        bail!("ciphertext length {} is not a multiple of the AES block size", data.len());
    }
    let mut buf = data.to_vec();
    let dec = Aes256CbcDec::new_from_slices(key, iv)?;
    dec.decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|e| anyhow::anyhow!("AES-CBC decrypt failed: {e}"))?;
    Ok(buf)
}

pub fn aes_cbc_encrypt_raw(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut buf = data.to_vec();
    let len = buf.len();
    buf.resize(len + 16, 0);
    let enc = Aes256CbcEnc::new_from_slices(key, iv)?;
    let ct = enc
        .encrypt_padded_mut::<NoPadding>(&mut buf, len)
        .map_err(|e| anyhow::anyhow!("AES-CBC encrypt failed: {e}"))?;
    Ok(ct.to_vec())
}

/// The sensor's own padding: `n` bytes each holding the value `n - 1`.
pub fn pad_validity(data: &[u8]) -> Vec<u8> {
    let n = 16 - (data.len() % 16);
    let mut out = data.to_vec();
    out.extend(std::iter::repeat((n - 1) as u8).take(n));
    out
}

pub fn unpad_validity(data: &[u8]) -> Result<Vec<u8>> {
    let last = *data.last().ok_or_else(|| anyhow::anyhow!("cannot unpad empty buffer"))? as usize;
    let keep = data
        .len()
        .checked_sub(last + 1)
        .ok_or_else(|| anyhow::anyhow!("invalid validity padding byte {last}"))?;
    Ok(data[..keep].to_vec())
}

/// Standard PKCS#7-style padding, used for the pairing blob only.
pub fn unpad_pkcs7(data: &[u8]) -> Result<Vec<u8>> {
    let last = *data.last().ok_or_else(|| anyhow::anyhow!("cannot unpad empty buffer"))? as usize;
    if last == 0 || last > data.len() {
        bail!("invalid PKCS#7 padding byte {last}");
    }
    Ok(data[..data.len() - last].to_vec())
}
