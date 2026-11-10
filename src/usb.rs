//! Connecting an Android phone by USB cable, two ways:
//!
//! 1. adb (USB debugging on): `adb reverse` lets the phone reach this
//!    computer's sender at 127.0.0.1:<port>, and `adb forward` lets this
//!    computer listen to a sender running on the phone at 127.0.0.1:<port+10>.
//! 2. Android Open Accessory mode (no USB debugging needed): this computer
//!    asks the phone to switch into accessory mode, then streams the same
//!    audio format (header + 16-bit samples) over the USB bulk endpoint.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use nusb::transfer::{ControlIn, ControlOut, ControlType, Direction, EndpointType, Recipient};

use crate::engine::Tap;

pub const FORWARD_OFFSET: u16 = 10;

const AOA_GET_PROTOCOL: u8 = 51;
const AOA_SEND_STRING: u8 = 52;
const AOA_START: u8 = 53;
const GOOGLE_VID: u16 = 0x18D1;

#[derive(Clone, Debug)]
pub struct AdbDevice {
    pub serial: String,
    pub model: String,
    pub state: String,
}

#[derive(Clone, Debug)]
pub struct UsbPhone {
    pub bus: u8,
    pub address: u8,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    pub accessory: bool,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub error: bool,
    pub text: String,
}

impl Note {
    fn ok(t: impl Into<String>) -> Option<Note> {
        Some(Note {
            error: false,
            text: t.into(),
        })
    }
    fn err(t: impl Into<String>) -> Option<Note> {
        Some(Note {
            error: true,
            text: t.into(),
        })
    }
}

#[derive(Default)]
pub struct UsbState {
    pub adb_path: Option<PathBuf>,
    pub adb_devices: Vec<AdbDevice>,
    pub adb_linked: Vec<String>,
    pub adb_note: Option<Note>,
    pub phones: Vec<UsbPhone>,
    /// Phones (bus, address) currently receiving sound over USB.
    pub aoa_running: Vec<(u8, u8)>,
    pub aoa_note: Option<Note>,
    pub busy: bool,
    pub scanned: bool,
}

pub struct Usb {
    pub state: Arc<Mutex<UsbState>>,
    aoa_stops: Vec<((u8, u8), Arc<AtomicBool>)>,
}

impl Usb {
    pub fn new() -> Self {
        Usb {
            state: Arc::new(Mutex::new(UsbState::default())),
            aoa_stops: Vec::new(),
        }
    }

    /// Look for adb phones and USB phones again (in the background).
    pub fn refresh(&self) {
        let state = self.state.clone();
        state.lock().unwrap().busy = true;
        thread::spawn(move || {
            let adb = find_adb();
            let (devices, adb_err) = match &adb {
                Some(p) => match adb_devices(p) {
                    Ok(d) => (d, None),
                    Err(e) => (Vec::new(), Note::err(format!("{e:#}"))),
                },
                None => (Vec::new(), None),
            };
            let phones = list_phones();
            let mut st = state.lock().unwrap();
            st.adb_path = adb;
            st.adb_linked.retain(|s| devices.iter().any(|d| &d.serial == s));
            st.adb_devices = devices;
            if adb_err.is_some() {
                st.adb_note = adb_err;
            }
            st.phones = phones;
            st.busy = false;
            st.scanned = true;
        });
    }

    pub fn adb_link(&self, serial: String, port: u16) {
        let state = self.state.clone();
        thread::spawn(move || {
            let Some(adb) = state.lock().unwrap().adb_path.clone() else {
                return;
            };
            let res = (|| -> Result<()> {
                run_adb(&adb, &["-s", &serial, "reverse", &format!("tcp:{port}"), &format!("tcp:{port}")])?;
                let local = port + FORWARD_OFFSET;
                run_adb(&adb, &["-s", &serial, "forward", &format!("tcp:{local}"), &format!("tcp:{port}")])?;
                Ok(())
            })();
            let mut st = state.lock().unwrap();
            match res {
                Ok(()) => {
                    if !st.adb_linked.contains(&serial) {
                        st.adb_linked.push(serial.clone());
                    }
                    st.adb_note = Note::ok(format!(
                        "Linked. On the phone, connect to 127.0.0.1:{port}. To hear the phone here, press \"Listen to phone\"."
                    ));
                }
                Err(e) => st.adb_note = Note::err(format!("{e:#}")),
            }
        });
    }

    pub fn adb_unlink(&self, serial: String, port: u16) {
        let state = self.state.clone();
        thread::spawn(move || {
            let Some(adb) = state.lock().unwrap().adb_path.clone() else {
                return;
            };
            let _ = run_adb(&adb, &["-s", &serial, "reverse", "--remove", &format!("tcp:{port}")]);
            let local = port + FORWARD_OFFSET;
            let _ = run_adb(&adb, &["-s", &serial, "forward", "--remove", &format!("tcp:{local}")]);
            let mut st = state.lock().unwrap();
            st.adb_linked.retain(|s| s != &serial);
            st.adb_note = Note::ok("Unlinked.");
        });
    }

    /// Ask a phone to switch into Android accessory mode.
    pub fn switch_to_accessory(&self, bus: u8, address: u8) {
        let state = self.state.clone();
        let me = self.state.clone();
        thread::spawn(move || {
            let res = aoa_handshake(bus, address);
            {
                let mut st = state.lock().unwrap();
                st.aoa_note = match res {
                    Ok(v) => Note::ok(format!(
                        "The phone is switching to accessory mode (version {v}). Accept the message on the phone if one appears."
                    )),
                    Err(e) => Note::err(format!("{e:#}")),
                };
            }
            // The phone reconnects with a new identity; look again shortly.
            thread::sleep(Duration::from_millis(2500));
            let phones = list_phones();
            me.lock().unwrap().phones = phones;
        });
    }

    /// Stream the sender's audio to one phone in accessory mode. Several
    /// phones can receive at the same time.
    pub fn start_accessory_stream(&mut self, bus: u8, address: u8, taps: Arc<Mutex<Vec<Tap>>>) {
        self.stop_accessory_stream(bus, address);
        let stop = Arc::new(AtomicBool::new(false));
        self.aoa_stops.push(((bus, address), stop.clone()));
        let state = self.state.clone();
        {
            let mut st = state.lock().unwrap();
            st.aoa_running.push((bus, address));
            st.aoa_note = Note::ok("Sending sound over USB…");
        }
        thread::spawn(move || {
            let res = run_accessory(bus, address, &stop, &taps);
            let mut st = state.lock().unwrap();
            st.aoa_running.retain(|d| *d != (bus, address));
            st.aoa_note = match res {
                Ok(()) => Note::ok("Stopped sending over USB."),
                Err(e) => Note::err(format!("{e:#}")),
            };
        });
    }

    pub fn stop_accessory_stream(&mut self, bus: u8, address: u8) {
        self.aoa_stops.retain(|(d, s)| {
            if *d == (bus, address) {
                s.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
    }

    pub fn stop_all_accessory_streams(&mut self) {
        for (_, s) in self.aoa_stops.drain(..) {
            s.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for Usb {
    fn drop(&mut self) {
        self.stop_all_accessory_streams();
    }
}

// ----------------------------------------------------------------------- adb

fn find_adb() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "adb.exe" } else { "adb" };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(h) = std::env::var_os(var) {
            dirs.push(PathBuf::from(h).join("platform-tools"));
        }
    }
    dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
}

fn run_adb(adb: &PathBuf, args: &[&str]) -> Result<String> {
    let out = Command::new(adb)
        .args(args)
        .output()
        .context("Could not run adb")?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("adb: {}", err.trim());
    }
    Ok(text)
}

fn adb_devices(adb: &PathBuf) -> Result<Vec<AdbDevice>> {
    let text = run_adb(adb, &["devices", "-l"])?;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(serial), Some(state)) = (parts.next(), parts.next()) else {
            continue;
        };
        let model = parts
            .find_map(|p| p.strip_prefix("model:"))
            .map(|m| m.replace('_', " "))
            .unwrap_or_else(|| serial.to_string());
        out.push(AdbDevice {
            serial: serial.to_string(),
            model,
            state: state.to_string(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------- accessory mode

fn is_accessory(vid: u16, pid: u16) -> bool {
    vid == GOOGLE_VID && (0x2D00..=0x2D05).contains(&pid)
}

/// USB devices that look like Android phones (or are already accessories).
fn list_phones() -> Vec<UsbPhone> {
    let Ok(devs) = nusb::list_devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for d in devs {
        let accessory = is_accessory(d.vendor_id(), d.product_id());
        // ADB (ff/42/01) or MTP/PTP (06/01/01 or ff/ff/00 named "MTP") interfaces.
        let androidish = d.interfaces().any(|i| {
            (i.class() == 0xff && i.subclass() == 0x42)
                || i.class() == 0x06
                || i.interface_string().is_some_and(|s| s.contains("MTP"))
        });
        if !(accessory || androidish) || d.class() == 0x09 {
            continue;
        }
        let name = match (d.manufacturer_string(), d.product_string()) {
            (Some(m), Some(p)) => format!("{m} {p}"),
            (None, Some(p)) => p.to_string(),
            (Some(m), None) => m.to_string(),
            _ => format!("USB device {:04x}:{:04x}", d.vendor_id(), d.product_id()),
        };
        out.push(UsbPhone {
            bus: d.bus_number(),
            address: d.device_address(),
            vid: d.vendor_id(),
            pid: d.product_id(),
            name,
            accessory,
        });
    }
    out
}

fn open_device(bus: u8, address: u8) -> Result<nusb::Device> {
    let info = nusb::list_devices()?
        .find(|d| d.bus_number() == bus && d.device_address() == address)
        .ok_or_else(|| anyhow!("The phone was unplugged"))?;
    info.open().map_err(|e| {
        anyhow!("Could not open the phone over USB ({e}). On Linux this usually needs a udev rule giving your user access.")
    })
}

#[cfg(not(target_os = "windows"))]
fn aoa_handshake(bus: u8, address: u8) -> Result<u16> {
    use futures_lite::future::block_on;
    let dev = open_device(bus, address)?;
    let reply = block_on(dev.control_in(ControlIn {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: AOA_GET_PROTOCOL,
        value: 0,
        index: 0,
        length: 2,
    }))
    .into_result()
    .context("The phone did not answer the accessory request")?;
    if reply.len() < 2 {
        bail!("The phone does not support accessory mode");
    }
    let version = u16::from_le_bytes([reply[0], reply[1]]);
    if version == 0 {
        bail!("The phone does not support accessory mode");
    }
    let strings = [
        "Airwave",                         // manufacturer
        "Airwave Audio",                   // model
        "Sound from your computer",        // description
        "1.0",                             // version
        "",                                // URI
        "AIRWAVE0001",                     // serial
    ];
    for (i, s) in strings.iter().enumerate() {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        block_on(dev.control_out(ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: AOA_SEND_STRING,
            value: 0,
            index: i as u16,
            data: &data,
        }))
        .into_result()
        .context("The phone stopped answering")?;
    }
    block_on(dev.control_out(ControlOut {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: AOA_START,
        value: 0,
        index: 0,
        data: &[],
    }))
    .into_result()
    .context("The phone refused to start accessory mode")?;
    Ok(version)
}

#[cfg(target_os = "windows")]
fn aoa_handshake(_bus: u8, _address: u8) -> Result<u16> {
    let _ = (AOA_GET_PROTOCOL, AOA_SEND_STRING, AOA_START);
    let _ = std::marker::PhantomData::<(ControlIn, ControlOut, ControlType, Recipient)>;
    bail!("Switching to accessory mode is not available on Windows in this version; use adb instead")
}

fn run_accessory(
    bus: u8,
    address: u8,
    stop: &Arc<AtomicBool>,
    taps: &Arc<Mutex<Vec<Tap>>>,
) -> Result<()> {
    use futures_lite::future::{block_on, poll_once};

    let info = nusb::list_devices()?
        .find(|d| d.bus_number() == bus && d.device_address() == address)
        .filter(|d| is_accessory(d.vendor_id(), d.product_id()))
        .ok_or_else(|| anyhow!("That phone is not in accessory mode any more. Press \"Use accessory mode\" again."))?;
    let name = info
        .product_string()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Phone".to_string());
    let dev = open_device(bus, address)?;
    let cfg = dev.active_configuration()?;
    let mut ep_out = None;
    for group in cfg.interfaces() {
        if group.interface_number() != 0 {
            continue;
        }
        for alt in group.alt_settings() {
            for ep in alt.endpoints() {
                if ep.transfer_type() == EndpointType::Bulk && ep.direction() == Direction::Out {
                    ep_out = Some(ep.address());
                }
            }
        }
    }
    let ep_out = ep_out.ok_or_else(|| anyhow!("The phone's accessory connection has no audio channel"))?;
    let iface = dev
        .detach_and_claim_interface(0)
        .context("Could not take over the phone's accessory connection")?;

    // Packets are already self-contained and at most 16 KB (see codec.rs),
    // which is what a phone in accessory mode can read in one go.
    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    let label = format!("USB: {name} ({bus}-{address})");
    taps.lock().unwrap().push(Tap {
        name: label.clone(),
        tx,
        since: Instant::now(),
    });
    let mut queue = iface.bulk_out_queue(ep_out);
    let mut stalled_since: Option<Instant> = None;

    let res = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            // Collect finished transfers without waiting.
            while queue.pending() > 0 {
                match block_on(poll_once(queue.next_complete())) {
                    Some(c) => {
                        c.status.map_err(|e| anyhow!("USB connection lost ({e})"))?;
                        stalled_since = None;
                    }
                    None => break,
                }
            }
            if queue.pending() >= 16 {
                let t = *stalled_since.get_or_insert_with(Instant::now);
                if t.elapsed() > Duration::from_secs(5) {
                    bail!("The phone is not taking the sound (is the Airwave phone app open?)");
                }
                // Drop whole packets while the phone catches up.
                while rx.try_recv().is_ok() {}
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(packet) => queue.submit(packet),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    })();
    queue.cancel_all();
    taps.lock().unwrap().retain(|t| t.name != label);
    res
}
