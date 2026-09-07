//! Bulk-endpoint transport for Validity sensors.
//!
//! The device exposes a single vendor-specific interface with four endpoints:
//! commands go out on EP1, replies come back on EP81, bulk image data arrives
//! on EP82, and EP83 carries finger-presence interrupts.

use anyhow::{anyhow, bail, Context, Result};
use rusb::{Direction, GlobalContext, TransferType};
use std::time::Duration;

pub const EP_CMD_OUT: u8 = 0x01;
pub const EP_CMD_IN: u8 = 0x81;
pub const EP_DATA_IN: u8 = 0x82;
pub const EP_INTERRUPT_IN: u8 = 0x83;

const REPLY_BUF: usize = 100 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(15000);

/// USB IDs known to speak this protocol.
pub const SUPPORTED_DEVICES: &[(u16, u16, &str)] = &[
    (0x138a, 0x0090, "Validity VFS7500"),
    (0x138a, 0x0097, "Validity VFS7552"),
    (0x138a, 0x009d, "Validity VFS7552"),
    (0x06cb, 0x009a, "Synaptics Metallica MIS"),
];

pub fn device_name(vid: u16, pid: u16) -> Option<&'static str> {
    SUPPORTED_DEVICES.iter().find(|(v, p, _)| *v == vid && *p == pid).map(|(_, _, n)| *n)
}

pub struct Usb {
    handle: rusb::DeviceHandle<GlobalContext>,
    interface: u8,
    detached_kernel_driver: bool,
    pub vid: u16,
    pub pid: u16,
    pub trace: bool,
}

impl Usb {
    /// Open the first supported sensor, waiting for it to appear.
    ///
    /// The sensor leaves the USB bus for a few seconds whenever it reboots,
    /// which the daemon asks it to do on shutdown. A tool started straight
    /// after `systemctl stop` would otherwise find no device at all.
    pub fn open_first() -> Result<Self> {
        Self::open_first_within(Duration::from_secs(15))
    }

    /// Open the first supported sensor, retrying until `timeout` elapses.
    pub fn open_first_within(timeout: Duration) -> Result<Self> {
        let deadline = std::time::Instant::now() + timeout;
        let mut announced = false;

        loop {
            match Self::try_open_first() {
                Ok(usb) => return Ok(usb),
                Err(e) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(e);
                    }
                    if !announced {
                        eprintln!("waiting for the sensor to appear on the USB bus...");
                        announced = true;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    fn try_open_first() -> Result<Self> {
        for dev in rusb::devices()?.iter() {
            let desc = match dev.device_descriptor() {
                Ok(d) => d,
                Err(_) => continue,
            };
            let (vid, pid) = (desc.vendor_id(), desc.product_id());
            if device_name(vid, pid).is_some() {
                return Self::open_device(dev, vid, pid);
            }
        }
        bail!(
            "no supported Validity sensor found. Known IDs: {}",
            SUPPORTED_DEVICES
                .iter()
                .map(|(v, p, n)| format!("{v:04x}:{p:04x} ({n})"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn open_device(dev: rusb::Device<GlobalContext>, vid: u16, pid: u16) -> Result<Self> {
        let config = dev.active_config_descriptor().context("reading config descriptor")?;
        // Pick the vendor-specific interface that carries our bulk endpoints.
        let interface = config
            .interfaces()
            .flat_map(|i| i.descriptors().collect::<Vec<_>>())
            .find(|d| {
                d.endpoint_descriptors().any(|e| {
                    e.transfer_type() == TransferType::Bulk
                        && e.direction() == Direction::Out
                        && e.address() == EP_CMD_OUT
                })
            })
            .map(|d| d.interface_number())
            .ok_or_else(|| anyhow!("device {vid:04x}:{pid:04x} has no bulk OUT endpoint 0x01"))?;

        let handle = dev.open().with_context(|| {
            format!(
                "opening {vid:04x}:{pid:04x}. If this is a permission error, run as root or \
                 install the udev rule shipped with this project."
            )
        })?;

        // Reset before claiming. A previous user that exited abruptly can leave
        // a session context open, after which even plain commands fail; a reset
        // clears that, so every tool starts from a known state.
        let _ = handle.reset();

        let mut detached_kernel_driver = false;
        if handle.kernel_driver_active(interface).unwrap_or(false) {
            handle.detach_kernel_driver(interface).context("detaching kernel driver")?;
            detached_kernel_driver = true;
        }

        handle.claim_interface(interface).context("claiming interface")?;

        Ok(Self { handle, interface, detached_kernel_driver, vid, pid, trace: false })
    }

    fn trace(&self, dir: &str, buf: &[u8]) {
        if self.trace {
            eprintln!("{dir} {}", hex::encode(buf));
        }
    }

    /// Send a command on EP1 and read its reply from EP81.
    pub fn cmd(&self, out: &[u8]) -> Result<Vec<u8>> {
        self.trace(">cmd>", out);
        self.handle
            .write_bulk(EP_CMD_OUT, out, DEFAULT_TIMEOUT)
            .context("writing command to EP1")?;

        let mut buf = vec![0u8; REPLY_BUF];
        let n = self
            .handle
            .read_bulk(EP_CMD_IN, &mut buf, DEFAULT_TIMEOUT)
            .context("reading reply from EP81")?;
        buf.truncate(n);
        self.trace("<cmd<", &buf);
        Ok(buf)
    }

    /// Read a bulk image transfer from EP82.
    pub fn read_data(&self, timeout: Duration) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; 1024 * 1024];
        let n = self.handle.read_bulk(EP_DATA_IN, &mut buf, timeout)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Poll EP83 once. `Ok(None)` means the poll timed out, which is normal
    /// while waiting for a finger and must not be treated as an error.
    pub fn poll_interrupt(&self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; 1024];
        match self.handle.read_interrupt(EP_INTERRUPT_IN, &mut buf, timeout) {
            Ok(n) => {
                buf.truncate(n);
                self.trace("<int<", &buf);
                Ok(Some(buf))
            }
            Err(rusb::Error::Timeout) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Wait for a finger-presence interrupt on EP83.
    pub fn wait_interrupt(&self, timeout: Duration) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; 1024];
        let n = self.handle.read_interrupt(EP_INTERRUPT_IN, &mut buf, timeout)?;
        buf.truncate(n);
        self.trace("<int<", &buf);
        Ok(buf)
    }
}

impl Drop for Usb {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface);
        if self.detached_kernel_driver {
            let _ = self.handle.attach_kernel_driver(self.interface);
        }
    }
}

/// Sensor replies begin with a little-endian u16 status word.
pub fn check_status(rsp: &[u8]) -> Result<()> {
    if rsp.len() < 2 {
        bail!("short reply: {} bytes", rsp.len());
    }
    let status = u16::from_le_bytes([rsp[0], rsp[1]]);
    match status {
        0 => Ok(()),
        0x044f => bail!("signature validation failed (0x044f)"),
        s => bail!("sensor returned error status 0x{s:04x}"),
    }
}

/// A command channel to the sensor: plain bulk USB before the session is up,
/// encrypted records afterwards.
pub trait Transport {
    fn cmd(&mut self, out: &[u8]) -> Result<Vec<u8>>;
}

impl Transport for Usb {
    fn cmd(&mut self, out: &[u8]) -> Result<Vec<u8>> {
        Usb::cmd(self, out)
    }
}
