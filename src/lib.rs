//! A Rust driver for Synaptics/Validity match-on-chip fingerprint sensors.
//!
//! These sensors perform enrolment and matching on the chip itself and speak a
//! bespoke TLS 1.2 dialect over bulk USB. The protocol is undocumented by the
//! vendor; the wire format implemented here was established by the
//! reverse-engineering work of the python-validity project (MIT).

pub mod blobs;
pub mod crypto;
pub mod db;
pub mod init;
pub mod flash;
pub mod pairing;
pub mod sid;
pub mod tls;
pub mod usb;
