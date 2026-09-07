//! A Rust driver for Synaptics/Validity match-on-chip fingerprint sensors.
//!
//! These sensors perform enrolment and matching on the chip itself and speak a
//! bespoke TLS 1.2 dialect over bulk USB. The protocol is undocumented by the
//! vendor; the wire format implemented here was established by the
//! reverse-engineering work of the python-validity project (MIT).

pub mod baseline;
pub mod blobs;
pub mod capture;
pub mod crypto;
pub mod db;
pub mod device;
pub mod enroll;
pub mod init;
pub mod firmware;
pub mod flash;
pub mod pairing;
pub mod provision;
pub mod provision_blobs;
pub mod sensor;
pub mod sid;
pub mod tables;
pub mod timeslot;
pub mod tls;
pub mod usb;

/// Restore the default disposition for `SIGPIPE`.
///
/// Rust ignores `SIGPIPE`, so writing to a closed pipe returns `EPIPE` and
/// `println!` panics with "failed printing to stdout". A command line tool is
/// expected to die quietly when its reader goes away, which is what happens
/// with `| head` or quitting out of `less`.
///
/// Not for the daemon, which should not be killed by a closed stream.
pub fn restore_sigpipe() {
    // SAFETY: restoring a signal to its default disposition is
    // async-signal-safe and does not disturb any other thread's invariants.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
