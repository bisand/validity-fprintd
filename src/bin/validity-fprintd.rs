//! An fprintd-compatible D-Bus daemon.
//!
//! Implements enough of `net.reactivated.Fprint` for the stock `pam_fprintd.so`
//! and the desktop settings panels to drive this sensor. It must replace the
//! stock fprintd, since both claim the same bus name.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use validity_rs::device::{list_enrolled_fingers, verify, VerifyOutcome};
use zbus::object_server::SignalContext;
use zbus::zvariant::OwnedObjectPath;

const DEVICE_PATH: &str = "/net/reactivated/Fprint/Device/0";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const BUS_NAME: &str = "net.reactivated.Fprint";

/// How long to wait for the user to present a finger.
const FINGER_TIMEOUT: Duration = Duration::from_secs(30);

struct Manager;

#[zbus::interface(name = "net.reactivated.Fprint.Manager")]
impl Manager {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from(DEVICE_PATH).expect("device path is valid")]
    }

    fn get_default_device(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(DEVICE_PATH).expect("device path is valid")
    }
}

#[derive(Default)]
struct DeviceState {
    /// The user whose fingerprints this claim is for.
    claimed_by: Option<String>,
    verifying: bool,
}

struct Device {
    state: Arc<Mutex<DeviceState>>,
}

#[zbus::interface(name = "net.reactivated.Fprint.Device")]
impl Device {
    #[zbus(property)]
    fn name(&self) -> String {
        "Synaptics Metallica MIS".to_string()
    }

    #[zbus(property, name = "num-enroll-stages")]
    fn num_enroll_stages(&self) -> i32 {
        // Enrolment is not implemented yet; report the usual stage count so
        // clients render sensibly rather than dividing by zero.
        5
    }

    #[zbus(property, name = "scan-type")]
    fn scan_type(&self) -> String {
        "press".to_string()
    }

    async fn claim(&self, username: String) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;
        if let Some(existing) = &state.claimed_by {
            if existing != &username {
                return Err(zbus::fdo::Error::Failed(format!(
                    "device already claimed by {existing}"
                )));
            }
        }
        state.claimed_by = Some(resolve_user(&username));
        Ok(())
    }

    async fn release(&self) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;
        state.claimed_by = None;
        state.verifying = false;
        Ok(())
    }

    async fn list_enrolled_fingers(&self, username: String) -> zbus::fdo::Result<Vec<String>> {
        let user = resolve_user(&username);
        tokio::task::spawn_blocking(move || list_enrolled_fingers(&user))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
            .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))
    }

    async fn delete_enrolled_fingers(&self, _username: String) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("deleting enrolments is not implemented".into()))
    }

    async fn delete_enrolled_fingers2(&self) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("deleting enrolments is not implemented".into()))
    }

    async fn enroll_start(&self, _finger_name: String) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported("enrolment is not implemented".into()))
    }

    async fn enroll_stop(&self) -> zbus::fdo::Result<()> {
        Ok(())
    }

    /// Begin a verification. Returns immediately; the result arrives as a
    /// `VerifyStatus` signal, which is what pam_fprintd waits on.
    async fn verify_start(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        finger_name: String,
    ) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;
        let Some(user) = state.claimed_by.clone() else {
            return Err(zbus::fdo::Error::Failed("device is not claimed".into()));
        };
        if state.verifying {
            return Err(zbus::fdo::Error::Failed("a verification is already running".into()));
        }
        state.verifying = true;
        drop(state);

        let ctxt = ctxt.to_owned();
        let shared = self.state.clone();

        tokio::spawn(async move {
            let _ = Device::verify_finger_selected(&ctxt, &finger_name).await;

            let outcome =
                tokio::task::spawn_blocking(move || verify(&user, FINGER_TIMEOUT)).await;

            let result = match outcome {
                Ok(Ok(VerifyOutcome::Match { .. })) => "verify-match",
                Ok(Ok(VerifyOutcome::NoMatch)) => "verify-no-match",
                Ok(Ok(VerifyOutcome::Retry(_))) => "verify-retry-scan",
                Ok(Err(_)) | Err(_) => "verify-unknown-error",
            };

            let _ = Device::verify_status(&ctxt, result, true).await;
            shared.lock().await.verifying = false;
        });

        Ok(())
    }

    async fn verify_stop(&self) -> zbus::fdo::Result<()> {
        self.state.lock().await.verifying = false;
        Ok(())
    }

    #[zbus(signal)]
    async fn verify_status(ctxt: &SignalContext<'_>, result: &str, done: bool)
        -> zbus::Result<()>;

    #[zbus(signal)]
    async fn verify_finger_selected(ctxt: &SignalContext<'_>, finger: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn enroll_status(ctxt: &SignalContext<'_>, result: &str, done: bool)
        -> zbus::Result<()>;
}

/// fprintd clients pass an empty username to mean "the calling user".
fn resolve_user(username: &str) -> String {
    if !username.is_empty() {
        return username.to_string();
    }
    std::env::var("SUDO_USER").unwrap_or_else(|_| "root".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("validity-fprintd starting on {BUS_NAME}");

    let _conn = zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(MANAGER_PATH, Manager)?
        .serve_at(DEVICE_PATH, Device { state: Arc::new(Mutex::new(DeviceState::default())) })?
        .build()
        .await?;

    eprintln!("validity-fprintd ready");
    std::future::pending::<()>().await;
    Ok(())
}
