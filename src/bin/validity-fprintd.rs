//! An fprintd-compatible D-Bus daemon.
//!
//! Implements enough of `net.reactivated.Fprint` for the stock `pam_fprintd.so`
//! and the desktop settings panels to drive this sensor. It must replace the
//! stock fprintd, since both claim the same bus name.

use anyhow::Result;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::Mutex;
use validity_rs::device::{Sensor, VerifyOutcome};
use zbus::object_server::SignalContext;
use zbus::zvariant::OwnedObjectPath;

const DEVICE_PATH: &str = "/net/reactivated/Fprint/Device/0";
const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
const BUS_NAME: &str = "net.reactivated.Fprint";

/// How long to wait for the user to present a finger.
const FINGER_TIMEOUT: Duration = Duration::from_secs(30);

/// The sensor session, opened lazily and shared across D-Bus calls.
type SensorSlot = Arc<StdMutex<Option<Sensor>>>;

/// Run `f` against a live session, opening one if needed.
///
/// If the operation fails the session is dropped, so the next call reconnects
/// rather than reusing a session the sensor may have already torn down.
fn with_sensor<T>(
    slot: &SensorSlot,
    f: impl FnOnce(&mut Sensor) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut guard = slot.lock().expect("sensor mutex poisoned");
    if guard.is_none() {
        *guard = Some(Sensor::open(false)?);
    }

    let result = f(guard.as_mut().expect("session just opened"));
    if result.is_err() {
        *guard = None;
    }
    result
}

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
    /// Unique bus name of the client holding the claim, so an abandoned claim
    /// can be reclaimed once that client is gone.
    claim_owner: Option<String>,
    busy: bool,
}

struct Device {
    state: Arc<Mutex<DeviceState>>,
    sensor: SensorSlot,
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

    async fn claim(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        username: String,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().map(|s| s.to_string());
        let mut state = self.state.lock().await;

        // An existing claim only blocks a new one while its owner is still on
        // the bus. Clients that crashed or exited without calling Release
        // would otherwise wedge the device until the daemon restarts.
        if let (Some(existing_user), Some(owner)) = (&state.claimed_by, &state.claim_owner) {
            if sender.as_deref() != Some(owner.as_str()) && peer_is_alive(conn, owner).await {
                return Err(zbus::fdo::Error::Failed(format!(
                    "device already claimed by {existing_user}"
                )));
            }
        }

        state.claimed_by = Some(resolve_user(&username));
        state.claim_owner = sender;
        state.busy = false;
        Ok(())
    }

    async fn release(&self) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;
        state.claimed_by = None;
        state.claim_owner = None;
        state.busy = false;
        Ok(())
    }

    async fn list_enrolled_fingers(&self, username: String) -> zbus::fdo::Result<Vec<String>> {
        let user = resolve_user(&username);
        let slot = self.sensor.clone();
        tokio::task::spawn_blocking(move || {
            with_sensor(&slot, |s| s.list_enrolled_fingers(&user))
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))
    }

    async fn delete_enrolled_fingers(&self, username: String) -> zbus::fdo::Result<()> {
        let user = resolve_user(&username);
        let slot = self.sensor.clone();
        tokio::task::spawn_blocking(move || {
            with_sensor(&slot, |s| s.delete_enrolled_fingers(&user))
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map(|_| ())
        .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))
    }

    /// Same as `DeleteEnrolledFingers`, but for the claiming user.
    async fn delete_enrolled_fingers2(&self) -> zbus::fdo::Result<()> {
        let user = self
            .state
            .lock()
            .await
            .claimed_by
            .clone()
            .ok_or_else(|| zbus::fdo::Error::Failed("device is not claimed".into()))?;

        let slot = self.sensor.clone();
        tokio::task::spawn_blocking(move || {
            with_sensor(&slot, |s| s.delete_enrolled_fingers(&user))
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?
        .map(|_| ())
        .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))
    }

    /// Begin an enrolment. Progress arrives as `EnrollStatus` signals; the
    /// sensor decides how many scans it needs, so the count is not known here.
    async fn enroll_start(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
        finger_name: String,
    ) -> zbus::fdo::Result<()> {
        let mut state = self.state.lock().await;
        let Some(user) = state.claimed_by.clone() else {
            return Err(zbus::fdo::Error::Failed("device is not claimed".into()));
        };
        if state.busy {
            return Err(zbus::fdo::Error::Failed("an operation is already running".into()));
        }
        state.busy = true;
        drop(state);

        // Clients may ask for "any"; the sensor needs a concrete finger.
        let finger = if finger_name == "any" || finger_name.is_empty() {
            "right-index-finger".to_string()
        } else {
            finger_name
        };

        let ctxt = ctxt.to_owned();
        let shared = self.state.clone();
        let slot = self.sensor.clone();

        // The enrolment loop runs on a blocking thread, so stage events come
        // back over a channel to be emitted as signals.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<usize, String>>();

        let stage_ctxt = ctxt.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let result = match ev {
                    Ok(_) => "enroll-stage-passed",
                    Err(_) => "enroll-retry-scan",
                };
                let _ = Device::enroll_status(&stage_ctxt, result, false).await;
            }
        });

        tokio::spawn(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                with_sensor(&slot, |s| {
                    s.enroll(&user, &finger, FINGER_TIMEOUT, |stage, err| {
                        let _ = tx.send(match err {
                            Some(e) => Err(e.to_string()),
                            None => Ok(stage),
                        });
                    })
                })
            })
            .await;

            let result = match outcome {
                Ok(Ok(_)) => "enroll-completed",
                _ => "enroll-failed",
            };
            let _ = Device::enroll_status(&ctxt, result, true).await;
            shared.lock().await.busy = false;
        });

        Ok(())
    }

    async fn enroll_stop(&self) -> zbus::fdo::Result<()> {
        self.state.lock().await.busy = false;
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
        if state.busy {
            return Err(zbus::fdo::Error::Failed("an operation is already running".into()));
        }
        state.busy = true;
        drop(state);

        let ctxt = ctxt.to_owned();
        let shared = self.state.clone();
        let slot = self.sensor.clone();

        tokio::spawn(async move {
            let _ = Device::verify_finger_selected(&ctxt, &finger_name).await;

            let outcome = tokio::task::spawn_blocking(move || {
                with_sensor(&slot, |s| s.verify(&user, FINGER_TIMEOUT))
            })
            .await;

            let result = match outcome {
                Ok(Ok(VerifyOutcome::Match { .. })) => "verify-match",
                Ok(Ok(VerifyOutcome::NoMatch)) => "verify-no-match",
                Ok(Ok(VerifyOutcome::Retry(_))) => "verify-retry-scan",
                Ok(Err(_)) | Err(_) => "verify-unknown-error",
            };

            let _ = Device::verify_status(&ctxt, result, true).await;
            shared.lock().await.busy = false;
        });

        Ok(())
    }

    async fn verify_stop(&self) -> zbus::fdo::Result<()> {
        self.state.lock().await.busy = false;
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

/// Is `name` still present on the bus?
///
/// Errors are treated as "gone": failing open here means a stale claim can be
/// taken over, which is far better than wedging the device.
async fn peer_is_alive(conn: &zbus::Connection, name: &str) -> bool {
    let Ok(proxy) = zbus::fdo::DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(bus_name) = zbus::names::BusName::try_from(name.to_string()) else {
        return false;
    };
    proxy.name_has_owner(bus_name).await.unwrap_or(false)
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
        .serve_at(
            DEVICE_PATH,
            Device {
                state: Arc::new(Mutex::new(DeviceState::default())),
                sensor: Arc::new(StdMutex::new(None)),
            },
        )?
        .build()
        .await?;

    eprintln!("validity-fprintd ready");
    std::future::pending::<()>().await;
    Ok(())
}
