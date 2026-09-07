//! The sensor's bespoke TLS 1.2 session layer.
//!
//! This is TLS-shaped but not TLS-conformant: the certificate message carries
//! duplicated length prefixes, the ClientHello extension block is written two
//! bytes short, record padding is `n-1`-filled rather than PKCS#7, and the
//! client Finished is excluded from the handshake transcript. Every one of
//! those quirks is required — the firmware rejects a standards-clean stream.

use crate::crypto::{
    aes_cbc_decrypt_raw, aes_cbc_encrypt_raw, hmac_sha256, pad_validity, prf, unpad_validity,
};
use crate::usb::Usb;
use anyhow::{bail, Context, Result};
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand::RngCore;
use sha2::{Digest, Sha256};

const RECORD_HANDSHAKE: u8 = 0x16;
const RECORD_CHANGE_CIPHER_SPEC: u8 = 0x14;
const RECORD_APP_DATA: u8 = 0x17;

/// TLS_ECDH_ECDSA_WITH_AES_256_CBC_SHA — the only suite the firmware accepts.
const SUITE_ECDH_ECDSA_AES256_CBC_SHA: u16 = 0xc005;

fn with_1byte_size(b: &[u8]) -> Vec<u8> {
    let mut o = vec![b.len() as u8];
    o.extend_from_slice(b);
    o
}

fn with_2bytes_size(b: &[u8]) -> Vec<u8> {
    let mut o = (b.len() as u16).to_be_bytes().to_vec();
    o.extend_from_slice(b);
    o
}

fn with_3bytes_size(b: &[u8]) -> Vec<u8> {
    let l = b.len();
    let mut o = vec![(l >> 16) as u8];
    o.extend_from_slice(&(l as u16).to_be_bytes());
    o.extend_from_slice(b);
    o
}

/// Material recovered from the on-flash pairing record.
pub struct PairingMaterial {
    /// Host ECDSA private scalar (big-endian), from block 4.
    pub private_key_d: [u8; 32],
    /// Host certificate blob, from block 3.
    pub tls_cert: Vec<u8>,
    /// Sensor's static ECDH public point, from block 6.
    pub ecdh_public: p256::PublicKey,
}

pub struct Tls<'a> {
    usb: &'a Usb,
    material: PairingMaterial,
    handshake_hash: Sha256,
    client_random: [u8; 32],
    server_random: [u8; 32],
    master_secret: Vec<u8>,
    sign_key: Vec<u8>,
    validation_key: Vec<u8>,
    encryption_key: Vec<u8>,
    decryption_key: Vec<u8>,
    session_public: Vec<u8>,
    pub secure_tx: bool,
    pub secure_rx: bool,
}

impl<'a> Tls<'a> {
    pub fn new(usb: &'a Usb, material: PairingMaterial) -> Self {
        Self {
            usb,
            material,
            handshake_hash: Sha256::new(),
            client_random: [0u8; 32],
            server_random: [0u8; 32],
            master_secret: Vec::new(),
            sign_key: Vec::new(),
            validation_key: Vec::new(),
            encryption_key: Vec::new(),
            decryption_key: Vec::new(),
            session_public: Vec::new(),
            secure_tx: false,
            secure_rx: false,
        }
    }

    /// The underlying transport, for bulk and interrupt endpoint access.
    pub fn usb(&self) -> &'a Usb {
        self.usb
    }

    /// Send a command, encrypting it once the session is up.
    pub fn cmd(&mut self, cmd: &[u8]) -> Result<Vec<u8>> {
        if self.secure_tx && self.secure_rx {
            self.app(cmd)
        } else {
            self.usb.cmd(cmd)
        }
    }

    fn update_neg(&mut self, b: &[u8]) {
        self.handshake_hash.update(b);
    }

    fn transcript(&self) -> [u8; 32] {
        self.handshake_hash.clone().finalize().into()
    }

    fn with_neg_hdr(&mut self, t: u8, b: &[u8]) -> Vec<u8> {
        let mut o = vec![t];
        o.extend_from_slice(&with_3bytes_size(b));
        self.update_neg(&o);
        o
    }

    /// Perform the two-flight handshake.
    pub fn open(&mut self) -> Result<()> {
        self.secure_tx = false;
        self.secure_rx = false;
        self.handshake_hash = Sha256::new();

        let hello = self.make_client_hello();
        let mut pkt = vec![0x44, 0x00, 0x00, 0x00];
        pkt.extend_from_slice(&self.make_handshake(&hello)?);
        let rsp = self.usb.cmd(&pkt).context("sending ClientHello")?;
        self.parse_tls_response(&rsp).context("parsing ServerHello flight")?;

        self.make_keys()?;

        // Evaluation order matters: this flight is built while secure_tx is
        // still false, so it goes out in the clear. make_finish() then flips
        // secure_tx, which encrypts the Finished record that follows.
        let mut flight = self.make_certs();
        flight.extend_from_slice(&self.make_client_kex());
        flight.extend_from_slice(&self.make_cert_verify()?);

        let mut pkt = vec![0x44, 0x00, 0x00, 0x00];
        pkt.extend_from_slice(&self.make_handshake(&flight)?);
        pkt.extend_from_slice(&[0x14, 0x03, 0x03, 0x00, 0x01, 0x01]);
        let finish = self.make_finish();
        pkt.extend_from_slice(&self.make_handshake(&finish)?);

        let rsp = self.usb.cmd(&pkt).context("sending client Finished")?;
        self.parse_tls_response(&rsp).context("parsing server Finished")?;

        if !(self.secure_tx && self.secure_rx) {
            bail!("handshake completed without establishing a secure channel");
        }
        Ok(())
    }

    fn make_client_hello(&mut self) -> Vec<u8> {
        let mut h = vec![0x03, 0x03];
        rand::thread_rng().fill_bytes(&mut self.client_random);
        h.extend_from_slice(&self.client_random);
        h.extend_from_slice(&with_1byte_size(&[0u8; 7]));

        let mut suites = Vec::new();
        suites.extend_from_slice(&SUITE_ECDH_ECDSA_AES256_CBC_SHA.to_be_bytes());
        suites.extend_from_slice(&0x003du16.to_be_bytes());
        suites.extend_from_slice(&0x008du16.to_be_bytes());
        h.extend_from_slice(&with_2bytes_size(&suites));

        h.extend_from_slice(&with_1byte_size(&[]));

        let mut exts = Vec::new();
        exts.extend_from_slice(&make_ext(0x0004, &0x0017u16.to_be_bytes()));
        exts.extend_from_slice(&make_ext(0x000b, &with_1byte_size(&[0x00])));

        // The firmware expects this length field two bytes short of the real
        // extension block length. Writing the correct value breaks the handshake.
        h.extend_from_slice(&((exts.len() - 2) as u16).to_be_bytes());
        h.extend_from_slice(&exts);

        self.with_neg_hdr(0x01, &h)
    }

    fn make_certs(&mut self) -> Vec<u8> {
        let cert_len = self.material.tls_cert.len();
        let mut cert = vec![0xac, 0x16];
        cert.extend_from_slice(&self.material.tls_cert);

        // Both prefixes carry the bare certificate length rather than the
        // length of the structure they introduce.
        for _ in 0..2 {
            let mut wrapped = vec![0u8];
            wrapped.extend_from_slice(&(cert_len as u16).to_be_bytes());
            wrapped.extend_from_slice(&cert);
            cert = wrapped;
        }

        self.with_neg_hdr(0x0b, &cert)
    }

    fn make_client_kex(&mut self) -> Vec<u8> {
        let point = self.session_public.clone();
        self.with_neg_hdr(0x10, &point)
    }

    fn make_cert_verify(&mut self) -> Result<Vec<u8>> {
        let digest = self.transcript();
        let signing_key = SigningKey::from_bytes((&self.material.private_key_d).into())
            .context("loading host ECDSA private key")?;
        let sig: Signature =
            signing_key.sign_prehash(&digest).context("signing handshake transcript")?;
        Ok(self.with_neg_hdr(0x0f, sig.to_der().as_bytes()))
    }

    fn make_finish(&mut self) -> Vec<u8> {
        self.secure_tx = true;
        let hs_hash = self.transcript();
        let mut seed = b"client finished".to_vec();
        seed.extend_from_slice(&hs_hash);
        let verify_data = prf(&self.master_secret, &seed, 0x0c);

        // Deliberately not folded into the transcript.
        let mut o = vec![0x14];
        o.extend_from_slice(&with_3bytes_size(&verify_data));
        o
    }

    fn make_keys(&mut self) -> Result<()> {
        let secret = p256::ecdh::EphemeralSecret::random(&mut rand::thread_rng());
        let public = secret.public_key();
        self.session_public = public.to_encoded_point(false).as_bytes().to_vec();

        let shared = secret.diffie_hellman(&self.material.ecdh_public);
        let pre_master_secret = shared.raw_secret_bytes().to_vec();

        let mut seed = Vec::with_capacity(0x40);
        seed.extend_from_slice(&self.client_random);
        seed.extend_from_slice(&self.server_random);

        let mut ms_seed = b"master secret".to_vec();
        ms_seed.extend_from_slice(&seed);
        self.master_secret = prf(&pre_master_secret, &ms_seed, 0x30);

        let mut ke_seed = b"key expansion".to_vec();
        ke_seed.extend_from_slice(&seed);
        let key_block = prf(&self.master_secret, &ke_seed, 0x120);

        self.sign_key = key_block[0x00..0x20].to_vec();
        self.validation_key = key_block[0x20..0x40].to_vec();
        self.encryption_key = key_block[0x40..0x60].to_vec();
        self.decryption_key = key_block[0x60..0x80].to_vec();
        Ok(())
    }

    fn sign(&self, t: u8, b: &[u8]) -> Vec<u8> {
        let mut hdr = vec![t, 3, 3];
        hdr.extend_from_slice(&(b.len() as u16).to_be_bytes());
        hdr.extend_from_slice(b);
        let mac = hmac_sha256(&self.sign_key, &hdr);

        let mut out = b.to_vec();
        out.extend_from_slice(&mac);
        out
    }

    fn validate(&self, t: u8, b: &[u8]) -> Result<Vec<u8>> {
        if b.len() < 0x20 {
            bail!("record too short to carry a MAC ({} bytes)", b.len());
        }
        let (body, mac) = b.split_at(b.len() - 0x20);

        let mut hdr = vec![t, 3, 3];
        hdr.extend_from_slice(&(body.len() as u16).to_be_bytes());
        hdr.extend_from_slice(body);

        if hmac_sha256(&self.validation_key, &hdr) != mac {
            bail!("record MAC validation failed");
        }
        Ok(body.to_vec())
    }

    fn encrypt(&self, b: &[u8]) -> Result<Vec<u8>> {
        let mut iv = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut iv);
        let ct = aes_cbc_encrypt_raw(&self.encryption_key, &iv, &pad_validity(b))?;
        let mut out = iv.to_vec();
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn decrypt(&self, c: &[u8]) -> Result<Vec<u8>> {
        if c.len() < 0x10 {
            bail!("encrypted record too short ({} bytes)", c.len());
        }
        let (iv, ct) = c.split_at(0x10);
        unpad_validity(&aes_cbc_decrypt_raw(&self.decryption_key, iv, ct)?)
    }

    fn make_handshake(&mut self, b: &[u8]) -> Result<Vec<u8>> {
        let body =
            if self.secure_tx { self.encrypt(&self.sign(RECORD_HANDSHAKE, b))? } else { b.to_vec() };
        let mut o = vec![0x16, 0x03, 0x03];
        o.extend_from_slice(&with_2bytes_size(&body));
        Ok(o)
    }

    fn make_app_data(&mut self, b: &[u8]) -> Result<Vec<u8>> {
        if !self.secure_tx {
            bail!("refusing to send application data before the channel is secure");
        }
        let body = self.encrypt(&self.sign(RECORD_APP_DATA, b))?;
        let mut o = vec![0x17, 0x03, 0x03];
        o.extend_from_slice(&with_2bytes_size(&body));
        Ok(o)
    }

    fn app(&mut self, b: &[u8]) -> Result<Vec<u8>> {
        let pkt = self.make_app_data(b)?;
        let rsp = self.usb.cmd(&pkt)?;
        self.parse_tls_response(&rsp)
    }

    fn parse_tls_response(&mut self, rsp: &[u8]) -> Result<Vec<u8>> {
        let mut app_data = Vec::new();
        let mut rest = rsp.to_vec();

        while !rest.is_empty() {
            // The firmware may end a transfer mid-header; pad it out.
            while rest.len() < 5 {
                rest.push(0);
            }
            let (t, mj, mn) = (rest[0], rest[1], rest[2]);
            let sz = u16::from_be_bytes([rest[3], rest[4]]) as usize;
            rest.drain(..5);

            if mj != 3 || mn != 3 {
                bail!("unexpected TLS version {mj}.{mn}");
            }
            if rest.len() < sz {
                bail!("record claims {sz} bytes but only {} remain", rest.len());
            }
            let pkt: Vec<u8> = rest.drain(..sz).collect();

            match t {
                RECORD_HANDSHAKE => self.handle_handshake(&pkt)?,
                RECORD_CHANGE_CIPHER_SPEC => {
                    if pkt != [0x01] {
                        bail!("unexpected ChangeCipherSpec payload {}", hex::encode(&pkt));
                    }
                    self.secure_rx = true;
                }
                RECORD_APP_DATA => {
                    if !self.secure_rx {
                        bail!("application data arrived before the channel was secure");
                    }
                    app_data.extend_from_slice(&self.validate(RECORD_APP_DATA, &self.decrypt(&pkt)?)?);
                }
                other => bail!("unhandled record type {other:#04x}"),
            }
        }

        Ok(app_data)
    }

    fn handle_handshake(&mut self, handshake: &[u8]) -> Result<()> {
        let mut hs = if self.secure_rx {
            self.validate(RECORD_HANDSHAKE, &self.decrypt(handshake)?)?
        } else {
            handshake.to_vec()
        };

        while !hs.is_empty() {
            while hs.len() < 4 {
                hs.push(0);
            }
            let hdr: Vec<u8> = hs.drain(..4).collect();
            let t = hdr[0];
            let l = ((u16::from_be_bytes([hdr[1], hdr[2]]) as usize) << 8) | hdr[3] as usize;

            if hs.len() < l {
                bail!("handshake message claims {l} bytes but only {} remain", hs.len());
            }
            let p: Vec<u8> = hs.drain(..l).collect();

            match t {
                0x02 => self.handle_server_hello(&p)?,
                0x0d => {}  // CertificateRequest; parameters are fixed and ignored.
                0x0e => {}  // ServerHelloDone; no body.
                0x14 => self.handle_server_finish(&p)?,
                other => bail!("unknown handshake message type {other:#04x}"),
            }

            let mut fold = hdr;
            fold.extend_from_slice(&p);
            self.update_neg(&fold);
        }
        Ok(())
    }

    fn handle_server_hello(&mut self, p: &[u8]) -> Result<()> {
        if p.len() < 2 + 0x20 + 1 {
            bail!("ServerHello too short ({} bytes)", p.len());
        }
        if p[0] != 3 || p[1] != 3 {
            bail!("unexpected TLS version in ServerHello {}.{}", p[0], p[1]);
        }
        self.server_random.copy_from_slice(&p[2..2 + 0x20]);

        let sid_len = p[0x22] as usize;
        let after_sid = 0x23 + sid_len;
        if p.len() < after_sid + 3 {
            bail!("ServerHello truncated after session ID");
        }

        let suite = u16::from_be_bytes([p[after_sid], p[after_sid + 1]]);
        if suite != SUITE_ECDH_ECDSA_AES256_CBC_SHA {
            bail!("sensor selected unsupported cipher suite {suite:#06x}");
        }
        if p[after_sid + 2] != 0 {
            bail!("sensor selected compression, which is not supported");
        }
        Ok(())
    }

    fn handle_server_finish(&mut self, b: &[u8]) -> Result<()> {
        let hs_hash = self.transcript();
        let mut seed = b"server finished".to_vec();
        seed.extend_from_slice(&hs_hash);
        let verify_data = prf(&self.master_secret, &seed, 0x0c);
        if verify_data != b {
            bail!("server Finished verify_data mismatch; the session is not trustworthy");
        }
        Ok(())
    }
}

fn make_ext(id: u16, b: &[u8]) -> Vec<u8> {
    let mut o = id.to_be_bytes().to_vec();
    o.extend_from_slice(&with_2bytes_size(b));
    o
}

impl crate::usb::Transport for Tls<'_> {
    fn cmd(&mut self, out: &[u8]) -> Result<Vec<u8>> {
        Tls::cmd(self, out)
    }
}
