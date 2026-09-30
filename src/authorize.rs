//! Who may ask the daemon to do what.
//!
//! The daemon runs as root and anyone on the system bus can call it, so it has
//! to decide for itself, from the caller's uid, whose fingerprints a call may
//! touch. Stock fprintd makes the same decisions through polkit; these are the
//! parts that need no polkit at all.

/// Whose fingerprints a call is about: `requested` (`""` meaning the caller's
/// own), asked for by a caller running as `caller_uid`, whose login name is
/// `caller_name`.
///
/// A caller may always name itself. Only root may name anyone else: that is
/// PAM, checking a finger for whoever is logging in, and it runs as root.
/// Anyone else naming another account could enrol their own finger under that
/// account's name, and then pass as it wherever a fingerprint is accepted.
///
/// # Errors
/// The caller named an account that is not its own and is not root.
pub fn whose(caller_uid: u32, caller_name: &str, requested: &str) -> Result<String, String> {
    if requested.is_empty() || requested == caller_name {
        return Ok(caller_name.to_owned());
    }
    if caller_uid == 0 {
        return Ok(requested.to_owned());
    }
    Err(format!(
        "{caller_name} may not act for {requested}: only root can name another account"
    ))
}

/// The polkit action for adding or removing fingerprints, as stock fprintd
/// names it, so a policy written for fprintd applies here as well.
pub const ENROLL: &str = "net.reactivated.fprint.device.enroll";

#[cfg(test)]
mod tests {
    use super::whose;

    #[test]
    fn a_caller_may_name_itself_and_only_root_anyone_else() {
        assert_eq!(whose(1000, "andre", "").unwrap(), "andre");
        assert_eq!(whose(1000, "andre", "andre").unwrap(), "andre");
        assert!(whose(1000, "andre", "root").is_err());
        assert!(whose(1000, "andre", "kari").is_err());
        assert_eq!(whose(0, "root", "andre").unwrap(), "andre", "PAM, as root");
        assert_eq!(whose(0, "root", "").unwrap(), "root");
    }
}
