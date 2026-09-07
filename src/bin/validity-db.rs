//! Dump the sensor's on-chip enrolment database.
//!
//! Read-only: lists storage, users and enrolled fingers. Nothing is written.

use anyhow::Result;
use validity_fprintd::db::{
    db_info, finger_name, get_record_children, get_user_storage, list_users, STORAGE_NAME,
};
use validity_fprintd::init::{open_session, reboot};
use validity_fprintd::usb::{device_name, Usb};

fn main() -> Result<()> {
    // Die quietly when piped into head, or into a less that is quit early.
    validity_fprintd::restore_sigpipe();
    let trace = std::env::args().any(|a| a == "--trace");
    println!("validity-fprintd database dump (read-only)\n");

    let mut usb = Usb::open_first()?;
    usb.trace = trace;
    println!(
        "Device      : {:04x}:{:04x} ({})",
        usb.vid,
        usb.pid,
        device_name(usb.vid, usb.pid).unwrap_or("unknown")
    );

    let (mut tls, sig_ok) = open_session(std::sync::Arc::new(usb))?;
    println!("Session     : established{}", if sig_ok { "" } else { " (FIRMWARE SIGNATURE INVALID)" });

    let info = db_info(&mut tls)?;
    println!(
        "\nDatabase    : {} bytes total, {} used, {} free, {} records",
        info.total, info.used, info.free, info.records
    );
    println!("Roots       : {:?}", info.roots);

    match get_user_storage(&mut tls, STORAGE_NAME)? {
        None => {
            println!("\nNo '{STORAGE_NAME}' storage object exists yet.");
            println!("The sensor has never been enrolled against by this driver or the Windows one.");
        }
        Some(storage) => {
            println!(
                "\nStorage     : '{}' (dbid {}), {} user(s)",
                storage.name,
                storage.dbid,
                storage.users.len()
            );

            let children = get_record_children(&mut tls, storage.dbid)?;
            println!("Children    : {} record(s)", children.children.len());
            for c in &children.children {
                println!("  dbid {:4}  type {}", c.dbid, c.kind);
            }

            let users = list_users(&mut tls)?;
            if users.is_empty() {
                println!("\nNo users enrolled.");
            }
            for u in &users {
                println!("\nUser {} — identity {}", u.dbid, u.identity);
                if u.fingers.is_empty() {
                    println!("  (no fingers enrolled)");
                }
                for f in &u.fingers {
                    println!(
                        "  finger dbid {:4}  subtype {:#04x} ({})  {} bytes",
                        f.dbid,
                        f.subtype,
                        finger_name(f.subtype),
                        f.value_size
                    );
                }
            }
        }
    }

    println!("\nRebooting sensor to release the session context.");
    reboot(&mut tls)?;
    Ok(())
}
