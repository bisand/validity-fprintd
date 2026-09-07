//! Provisioning a blank or reset sensor.
//!
//! These paths format sensor flash and write a new pairing record. They only
//! apply to a sensor that has never been paired, or one that has been factory
//! reset — a sensor that has been used under Windows or by python-validity is
//! already provisioned and needs none of this.
//!
//! UNTESTED. Every function here was developed against a sensor that was
//! already provisioned, so none of it has been exercised end to end. Factory
//! reset in particular can leave a sensor unusable if provisioning then fails.

use crate::crypto::{
    aes_cbc_encrypt_raw, hmac_sha256, prf, sha256, HostKeys, PASSWORD_HARDCODED,
};
use crate::flash::{get_flash_info, read_flash, write_flash_all};
use crate::pairing::ecdh_public_from_block;
use crate::provision_blobs::{
    PartitionSpec, CRT_HARDCODED, FLASH_LAYOUT, FLASH_LAYOUT_0090, PARTITION_SIGNATURE,
    PARTITION_SIGNATURE_0090,
};
use crate::tables::flash_ic_lookup;
use crate::tls::{PairingMaterial, Tls};
use crate::usb::{check_status, Transport, Usb};
use anyhow::{bail, Context, Result};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use std::sync::Arc;

/// Partition holding the pairing record.
const PARTITION_CERT: u8 = 1;
/// Partitions wiped during provisioning.
const PARTITIONS_TO_ERASE: &[u8] = &[1, 2, 5, 6, 4];

/// Derive the key used to sign the host certificate.
///
/// Returns the private scalar big-endian. The PRF output is little-endian, so
/// it is reversed.
fn hs_key() -> [u8; 32] {
    let mut seed = b"HS_KEY_PAIR_GEN".to_vec();
    seed.extend_from_slice(&PASSWORD_HARDCODED[0x10..]);
    seed.extend_from_slice(&[0xaa, 0xaa]);

    let out = prf(&PASSWORD_HARDCODED[..0x10], &seed, 0x20);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&out);
    scalar.reverse();
    scalar
}

fn with_hdr(id: u16, buf: &[u8]) -> Vec<u8> {
    let mut out = id.to_le_bytes().to_vec();
    out.extend_from_slice(&(buf.len() as u16).to_le_bytes());
    out.extend_from_slice(buf);
    out
}

/// A 32-byte big-endian scalar or coordinate, as the sensor stores it.
fn to_le32(be: &[u8]) -> Vec<u8> {
    let mut v = be.to_vec();
    v.reverse();
    v
}

/// Build the host certificate the sensor stores, signed with the derived key.
fn make_cert(pub_x: &[u8], pub_y: &[u8]) -> Result<Vec<u8>> {
    use p256::ecdsa::{signature::Signer, Signature, SigningKey};

    let mut msg = 0x17u32.to_le_bytes().to_vec();
    msg.extend_from_slice(&0x20u32.to_le_bytes());
    msg.extend_from_slice(&to_le32(pub_x));
    msg.resize(msg.len() + 0x24, 0);
    msg.extend_from_slice(&to_le32(pub_y));
    msg.resize(msg.len() + 0x4c, 0);

    let signing_key =
        SigningKey::from_bytes((&hs_key()).into()).context("deriving certificate signing key")?;
    let sig: Signature = signing_key.sign(&msg);
    let der = sig.to_der();

    let mut out = msg;
    out.extend_from_slice(&(der.as_bytes().len() as u32).to_le_bytes());
    out.extend_from_slice(der.as_bytes());
    // The record is a fixed 444 bytes.
    if out.len() < 444 {
        out.resize(444, 0);
    }
    Ok(out)
}

/// Seal the host private key under the machine-derived pairing keys.
///
/// This is the record the driver later decrypts to open a session, so it binds
/// the sensor to this machine's DMI identity.
fn encrypt_key(keys: &HostKeys, pub_x: &[u8], pub_y: &[u8], private_d: &[u8]) -> Result<Vec<u8>> {
    let mut m = to_le32(pub_x);
    m.extend_from_slice(&to_le32(pub_y));
    m.extend_from_slice(&to_le32(private_d));

    // Standard PKCS#7 here, unlike the record padding used elsewhere.
    let pad = 16 - (m.len() % 16);
    m.extend(std::iter::repeat(pad as u8).take(pad));

    let mut iv = [0u8; 16];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut iv);

    let ct = aes_cbc_encrypt_raw(&keys.psk_encryption_key, &iv, &m)?;
    let mut c = iv.to_vec();
    c.extend_from_slice(&ct);

    let sig = hmac_sha256(&keys.psk_validation_key, &c);

    let mut out = vec![0x02];
    out.extend_from_slice(&c);
    out.extend_from_slice(&sig);
    Ok(out)
}

fn serialize_partition(p: &PartitionSpec) -> Vec<u8> {
    let mut b = vec![p.id, p.kind];
    b.extend_from_slice(&p.access_lvl.to_le_bytes());
    b.extend_from_slice(&p.offset.to_le_bytes());
    b.extend_from_slice(&p.size.to_le_bytes());

    let digest = sha256(&b);
    b.resize(b.len() + 4, 0);
    b.extend_from_slice(&digest);
    b
}

/// One block of the pairing record: id, length, SHA-256, body.
fn tls_flash_block(id: u16, body: &[u8]) -> Vec<u8> {
    let mut out = id.to_le_bytes().to_vec();
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(&sha256(body));
    out.extend_from_slice(body);
    out
}

/// Assemble the 4 KiB pairing record written to partition 1.
fn make_tls_flash(priv_blob: &[u8], tls_cert: &[u8], ecdh_blob: &[u8]) -> Result<Vec<u8>> {
    let mut b = tls_flash_block(0, &[0]);
    b.extend_from_slice(&tls_flash_block(4, priv_blob));
    b.extend_from_slice(&tls_flash_block(3, tls_cert));
    b.extend_from_slice(&tls_flash_block(5, &hex::decode(CRT_HARDCODED)?));
    b.extend_from_slice(&tls_flash_block(1, &[0u8; 0x100]));
    b.extend_from_slice(&tls_flash_block(2, &[0u8; 0x100]));
    b.extend_from_slice(&tls_flash_block(6, ecdh_blob));

    if b.len() > 0x1000 {
        bail!("pairing record is {} bytes, larger than the partition", b.len());
    }
    b.resize(0x1000, 0xff);
    Ok(b)
}

/// `0x10` plus the reset blob — return the sensor to factory state.
///
/// DESTRUCTIVE. This erases the pairing record, enrolled fingers and
/// calibration. The sensor is unusable until it is provisioned again, and if
/// provisioning fails it stays that way. UNTESTED.
pub fn factory_reset(usb: &Usb, reset_blob: &[u8]) -> Result<()> {
    check_status(&usb.cmd(reset_blob)?).context("sending the reset blob")?;

    let mut cmd = vec![0x10];
    cmd.resize(1 + 0x61, 0);
    check_status(&usb.cmd(&cmd)?).context("factory reset command (0x10)")?;

    // The sensor drops off the bus as it restarts, so no reply is expected.
    let _ = usb.cmd(&[0x05, 0x02, 0x00]);
    Ok(())
}

/// `0x4f` — write the flash partition table and receive the host certificate.
fn partition_flash(
    usb: &Usb,
    layout: &[PartitionSpec],
    signature: &[u8],
    ic_size: u32,
    ic_sector_size: u32,
    ic_erase_cmd: u32,
    pub_x: &[u8],
    pub_y: &[u8],
) -> Result<Vec<u8>> {
    let mut params = ic_size.to_le_bytes().to_vec();
    params.extend_from_slice(&ic_sector_size.to_le_bytes());
    params.extend_from_slice(&[0, 0]);
    params.push(ic_erase_cmd as u8);
    params.push(0);

    let mut table: Vec<u8> = layout.iter().flat_map(|p| serialize_partition(p)).collect();
    table.extend_from_slice(signature);

    let mut cmd = vec![0x4f, 0, 0, 0, 0];
    cmd.extend_from_slice(&with_hdr(0, &params));
    cmd.extend_from_slice(&with_hdr(1, &table));
    cmd.extend_from_slice(&with_hdr(5, &make_cert(pub_x, pub_y)?));
    cmd.extend_from_slice(&with_hdr(3, &hex::decode(CRT_HARDCODED)?));

    let rsp = usb.cmd(&cmd)?;
    check_status(&rsp).context("partitioning flash (0x4f)")?;
    if rsp.len() < 6 {
        bail!("partition reply too short");
    }

    let crt_len = u32::from_le_bytes([rsp[2], rsp[3], rsp[4], rsp[5]]) as usize;
    let cert = rsp
        .get(6..6 + crt_len)
        .ok_or_else(|| anyhow::anyhow!("partition reply does not contain the certificate"))?;
    Ok(cert.to_vec())
}

/// `0x50` — retrieve the sensor's static ECDH key block.
fn read_ecdh_block(usb: &Usb) -> Result<Vec<u8>> {
    let rsp = usb.cmd(&[0x50])?;
    check_status(&rsp).context("reading the ECDH block (0x50)")?;
    let body = &rsp[2..];

    if body.len() < 4 {
        bail!("ECDH reply too short");
    }
    let l = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    if l != body.len() {
        bail!("ECDH reply length mismatch: {l} != {}", body.len());
    }
    if body.len() < 404 {
        bail!("ECDH reply is missing the key block");
    }

    let block = &body[body.len() - 400..];
    if body[4..body.len() - 400].iter().any(|b| *b != 0) {
        bail!("unexpected non-zero padding in the ECDH reply");
    }
    Ok(block.to_vec())
}

/// Format a blank sensor and write a fresh pairing record bound to this host.
///
/// Does nothing if the flash already has a partition table, which is the case
/// for every sensor shipped in a laptop. UNTESTED.
pub fn init_flash(
    usb: Arc<Usb>,
    write_enable_blob: &[u8],
    reset_blob: &[u8],
    product_name: &str,
    product_serial: &str,
) -> Result<bool> {
    let mut probe = ProbeTransport(usb.clone());
    let info = get_flash_info(&mut probe)?;

    if !info.partitions.is_empty() {
        eprintln!("provision: flash already has {} partitions", info.partitions.len());
        return Ok(false);
    }
    eprintln!("provision: flash is unformatted, partitioning");

    let ic = flash_ic_lookup(
        info.jedec_id.0 as u32,
        info.jedec_id.1 as u32,
        info.blocks as u32 * info.blocksize as u32,
    )
    .ok_or_else(|| anyhow::anyhow!("unrecognised flash IC; refusing to partition"))?;
    eprintln!("provision: flash IC {} ({} bytes)", ic.name, ic.size);

    check_status(&usb.cmd(reset_blob)?).context("sending the reset blob")?;

    // A fresh host keypair; the private half is sealed into the pairing record.
    let secret = p256::SecretKey::random(&mut rand::thread_rng());
    let public = secret.public_key();
    let encoded = public.to_encoded_point(false);
    let pub_x = encoded.x().ok_or_else(|| anyhow::anyhow!("no x coordinate"))?.to_vec();
    let pub_y = encoded.y().ok_or_else(|| anyhow::anyhow!("no y coordinate"))?.to_vec();
    let private_d = secret.to_bytes().to_vec();

    let (layout, signature) = if (usb.vid, usb.pid) == (0x138a, 0x0090) {
        (FLASH_LAYOUT_0090, hex::decode(PARTITION_SIGNATURE_0090)?)
    } else {
        (FLASH_LAYOUT, hex::decode(PARTITION_SIGNATURE)?)
    };

    let tls_cert = partition_flash(
        &usb,
        layout,
        &signature,
        ic.size,
        ic.sector_size,
        ic.sector_erase_cmd,
        &pub_x,
        &pub_y,
    )?;
    eprintln!("provision: received a {} byte host certificate", tls_cert.len());

    let ecdh_blob = read_ecdh_block(&usb)?;
    let ecdh_public = ecdh_public_from_block(&ecdh_blob)?;

    let keys = HostKeys::derive(product_name, product_serial);
    let priv_blob = encrypt_key(&keys, &pub_x, &pub_y, &private_d)?;

    let mut private_key_d = [0u8; 32];
    private_key_d.copy_from_slice(&private_d);

    let mut tls = Tls::new(
        usb.clone(),
        PairingMaterial { private_key_d, tls_cert: tls_cert.clone(), ecdh_public },
    );
    tls.open().context("opening a session with the new keys")?;
    eprintln!("provision: session established with the new pairing material");

    // Wipe every partition before storing anything.
    for p in PARTITIONS_TO_ERASE {
        crate::flash::erase_flash(&mut tls, write_enable_blob, *p)
            .with_context(|| format!("erasing partition {p}"))?;
    }

    let record = make_tls_flash(&priv_blob, &tls_cert, &ecdh_blob)?;
    write_flash_all(&mut tls, write_enable_blob, PARTITION_CERT, 0, &record)?;
    eprintln!("provision: pairing record written; rebooting the sensor");

    // Confirm the record reads back before declaring success.
    let check = read_flash(&mut tls, PARTITION_CERT, 0, 0x40)?;
    if check.iter().all(|b| *b == 0xff) {
        bail!("pairing record did not persist");
    }

    crate::init::reboot(&mut tls)?;
    Ok(true)
}

/// Adapts a shared `Usb` to `Transport` for pre-session commands.
struct ProbeTransport(Arc<Usb>);

impl Transport for ProbeTransport {
    fn cmd(&mut self, out: &[u8]) -> Result<Vec<u8>> {
        self.0.cmd(out)
    }
}
