//! Thin WASAPI wrapper: endpoint enumeration, event-driven shared-mode capture
//! (microphones and render-endpoint loopback) with QPC timestamps, and an
//! inaudible keep-alive render stream so loopback keeps producing packets
//! during silence.

use super::{deliver, CaptureStatus, DeviceInfo, DeviceKind, Packet, StreamShared};
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use windows::core::{GUID, HSTRING, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioCaptureClient, IAudioClient, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, SetThreadPriority, WaitForSingleObject,
    THREAD_PRIORITY_TIME_CRITICAL,
};

type WinResult<T> = windows::core::Result<T>;

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
/// 200 ms shared-mode buffer: plenty of headroom against scheduling hiccups.
const BUFFER_HNS: i64 = 2_000_000;

pub fn list_devices() -> anyhow::Result<Vec<DeviceInfo>> {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED);
        let r = list_inner();
        if com.is_ok() {
            CoUninitialize();
        }
        Ok(r?)
    }
}

unsafe fn list_inner() -> WinResult<Vec<DeviceInfo>> {
    let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let mut out = Vec::new();
    let flows: [(EDataFlow, DeviceKind); 2] = [
        (eCapture, DeviceKind::Input),
        (eRender, DeviceKind::Loopback),
    ];
    for (flow, kind) in flows {
        let default_id = en
            .GetDefaultAudioEndpoint(flow, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let coll = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        for i in 0..coll.GetCount()? {
            let dev = coll.Item(i)?;
            let id = match device_id(&dev) {
                Ok(id) => id,
                Err(_) => continue,
            };
            let name = device_name(&dev).unwrap_or_else(|_| "Unknown device".into());
            out.push(DeviceInfo {
                key: format!("{}{}", kind.prefix(), id),
                is_default: default_id.as_deref() == Some(id.as_str()),
                id,
                name,
                kind,
            });
        }
    }
    Ok(out)
}

unsafe fn device_id(d: &IMMDevice) -> WinResult<String> {
    let p = d.GetId()?;
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    Ok(s)
}

unsafe fn device_name(d: &IMMDevice) -> WinResult<String> {
    let store = d.OpenPropertyStore(STGM_READ)?;
    let v = store.GetValue(&PKEY_Device_FriendlyName)?;
    Ok(v.to_string())
}

fn float_format(rate: u32, channels: u16, mask: u32) -> WAVEFORMATEXTENSIBLE {
    let block = channels * 4;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: mask,
        SubFormat: SUBTYPE_IEEE_FLOAT,
    }
}

pub fn capture_thread(
    dev: DeviceInfo,
    shared: Arc<StreamShared>,
    tx: Sender<Packet>,
    stop: Arc<AtomicBool>,
    keep_alive: bool,
) {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let key: Arc<str> = Arc::from(dev.key.as_str());
        while !stop.load(Ordering::Relaxed) {
            *shared.status.lock() = CaptureStatus::Starting;
            match capture_session(&dev, &shared, &tx, &stop, keep_alive, &key) {
                Ok(()) => break,
                Err(e) => {
                    *shared.status.lock() = CaptureStatus::Lost(format!("{e}"));
                    // Device unplugged / invalidated: retry once a second.
                    for _ in 0..10 {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
        *shared.status.lock() = CaptureStatus::Stopped;
        if com.is_ok() {
            CoUninitialize();
        }
    }
}

struct EventHandle(HANDLE);
impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

unsafe fn capture_session(
    dev: &DeviceInfo,
    shared: &StreamShared,
    tx: &Sender<Packet>,
    stop: &AtomicBool,
    keep_alive: bool,
    key: &Arc<str>,
) -> WinResult<()> {
    let loopback = dev.kind == DeviceKind::Loopback;
    let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = en.GetDevice(&HSTRING::from(dev.id.as_str()))?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

    let mix = client.GetMixFormat()?;
    let mixf: WAVEFORMATEX = std::ptr::read_unaligned(mix);
    let rate = mixf.nSamplesPerSec;
    let channels = mixf.nChannels;
    let mask = if mixf.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
        let ext: WAVEFORMATEXTENSIBLE =
            std::ptr::read_unaligned(mix as *const WAVEFORMATEXTENSIBLE);
        ext.dwChannelMask
    } else {
        0
    };
    CoTaskMemFree(Some(mix as *const _));

    let wfx = float_format(rate, channels, mask);
    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        flags,
        BUFFER_HNS,
        0,
        &wfx as *const WAVEFORMATEXTENSIBLE as *const WAVEFORMATEX,
        None,
    )?;
    let event = EventHandle(CreateEventW(None, false, false, PCWSTR::null())?);
    client.SetEventHandle(event.0)?;
    let capture: IAudioCaptureClient = client.GetService()?;

    let render = if loopback && keep_alive {
        SilentRender::start(&device).ok()
    } else {
        None
    };

    client.Start()?;
    *shared.status.lock() = CaptureStatus::Running;
    *shared.format.lock() = Some((rate, channels));

    let res = pump(
        &client,
        &capture,
        render.as_ref(),
        &event,
        shared,
        tx,
        stop,
        key,
        rate,
        channels,
    );
    let _ = client.Stop();
    if let Some(r) = &render {
        let _ = r.client.Stop();
    }
    res
}

#[allow(clippy::too_many_arguments)]
unsafe fn pump(
    _client: &IAudioClient,
    capture: &IAudioCaptureClient,
    render: Option<&SilentRender>,
    event: &EventHandle,
    shared: &StreamShared,
    tx: &Sender<Packet>,
    stop: &AtomicBool,
    key: &Arc<str>,
    rate: u32,
    channels: u16,
) -> WinResult<()> {
    let silent_flag = AUDCLNT_BUFFERFLAGS_SILENT.0 as u32;
    let ts_err_flag = AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32;
    while !stop.load(Ordering::Relaxed) {
        // Event-driven, with a timeout so loopback without events still gets drained.
        let _ = WaitForSingleObject(event.0, 20);
        if let Some(r) = render {
            let _ = r.feed();
        }
        loop {
            let n = capture.GetNextPacketSize()?;
            if n == 0 {
                break;
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
            let count = frames as usize * channels as usize;
            let mut buf = vec![0f32; count];
            if flags & silent_flag == 0 && count > 0 && !data.is_null() {
                buf.copy_from_slice(std::slice::from_raw_parts(data as *const f32, count));
            }
            capture.ReleaseBuffer(frames)?;
            let ts = if flags & ts_err_flag != 0 || qpc == 0 {
                None
            } else {
                Some(qpc as i64)
            };
            if count > 0 {
                deliver(shared, tx, key, rate, channels, buf, ts);
            }
        }
    }
    Ok(())
}

/// Plays digital silence on a render endpoint. Loopback capture only produces
/// packets while the endpoint's engine is running, so this keeps timestamps
/// flowing (and files growing) during quiet periods.
struct SilentRender {
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
}

impl SilentRender {
    unsafe fn start(device: &IMMDevice) -> WinResult<Self> {
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let mix = client.GetMixFormat()?;
        let r = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, BUFFER_HNS, 0, mix, None);
        CoTaskMemFree(Some(mix as *const _));
        r?;
        let buffer_frames = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService()?;
        let s = Self {
            client,
            render,
            buffer_frames,
        };
        s.feed()?;
        s.client.Start()?;
        Ok(s)
    }

    unsafe fn feed(&self) -> WinResult<()> {
        let pad = self.client.GetCurrentPadding()?;
        let avail = self.buffer_frames.saturating_sub(pad);
        if avail > 0 {
            let _ = self.render.GetBuffer(avail)?;
            self.render
                .ReleaseBuffer(avail, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
        }
        Ok(())
    }
}
