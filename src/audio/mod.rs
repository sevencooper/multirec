pub mod meter;
#[cfg(windows)]
mod wasapi;

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    /// A capture endpoint (microphone, line-in).
    Input,
    /// A render endpoint captured in loopback mode (what you hear).
    Loopback,
}

impl DeviceKind {
    pub fn prefix(self) -> &'static str {
        match self {
            DeviceKind::Input => "in:",
            DeviceKind::Loopback => "lb:",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// Unique key: kind prefix + endpoint id.
    pub key: String,
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub is_default: bool,
}

/// One buffer of captured audio, interleaved f32 at the device's native format.
pub struct Packet {
    pub key: Arc<str>,
    pub rate: u32,
    pub channels: u16,
    pub data: Vec<f32>,
    /// Capture time of the first frame on the `clock::now_hns` timeline.
    pub qpc_hns: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureStatus {
    Starting,
    Running,
    Lost(String),
    Stopped,
}

/// State shared between a capture thread and the UI.
pub struct StreamShared {
    pub meter: Mutex<meter::MeterState>,
    pub status: Mutex<CaptureStatus>,
    /// Native (rate, channels) once the device is open.
    pub format: Mutex<Option<(u32, u16)>>,
    gain_bits: AtomicU32,
}

impl StreamShared {
    pub fn new() -> Self {
        Self {
            meter: Mutex::new(meter::MeterState::default()),
            status: Mutex::new(CaptureStatus::Starting),
            format: Mutex::new(None),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
        }
    }
    pub fn set_gain_db(&self, db: f32) {
        self.gain_bits
            .store(10f32.powf(db / 20.0).to_bits(), Ordering::Relaxed);
    }
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }
}

/// Stops and joins the capture thread on drop.
pub struct CaptureHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Apply gain, update meters and forward a packet. Called from capture threads.
#[allow(dead_code)]
pub(crate) fn deliver(
    shared: &StreamShared,
    tx: &Sender<Packet>,
    key: &Arc<str>,
    rate: u32,
    channels: u16,
    mut data: Vec<f32>,
    qpc_hns: Option<i64>,
) {
    let g = shared.gain();
    if (g - 1.0).abs() > 1e-6 {
        for s in &mut data {
            *s *= g;
        }
    }
    shared.meter.lock().update(&data, channels as usize, rate);
    let _ = tx.send(Packet {
        key: key.clone(),
        rate,
        channels,
        data,
        qpc_hns,
    });
}

#[cfg(windows)]
pub fn list_devices() -> anyhow::Result<Vec<DeviceInfo>> {
    // Run on a fresh thread so COM can be initialised MTA regardless of the UI thread.
    std::thread::spawn(wasapi::list_devices)
        .join()
        .map_err(|_| anyhow::anyhow!("device enumeration panicked"))?
}

#[cfg(not(windows))]
pub fn list_devices() -> anyhow::Result<Vec<DeviceInfo>> {
    Ok(Vec::new())
}

pub fn spawn_capture(
    dev: &DeviceInfo,
    shared: Arc<StreamShared>,
    tx: Sender<Packet>,
    keep_alive: bool,
) -> CaptureHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let dev = dev.clone();
    let thread = std::thread::Builder::new()
        .name(format!("capture {}", dev.name))
        .spawn(move || {
            #[cfg(windows)]
            wasapi::capture_thread(dev, shared, tx, stop2, keep_alive);
            #[cfg(not(windows))]
            {
                let _ = (dev, tx, stop2, keep_alive);
                *shared.status.lock() = CaptureStatus::Lost("unsupported platform".into());
            }
        })
        .ok();
    CaptureHandle { stop, thread }
}
