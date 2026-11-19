//! Recording orchestration thread: receives packets from every capture thread,
//! aligns them onto one timeline and writes the per-stream and mix files.

use super::aligner::Aligner;
use super::manifest::{Manifest, ManifestStream, Marker};
use super::writer::{create_sink, Sink, SinkSpec};
use crate::audio::{DeviceKind, Packet, StreamShared};
use crate::clock::{now_hns, HNS_PER_SEC};
use crate::config::{AudioFormat, BitDepth};
use anyhow::Result;
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A stream with no packets for this long is considered idle and padded with silence.
const IDLE_HNS: i64 = HNS_PER_SEC / 2;
/// Idle padding stays this far behind "now" so late packets can still land.
const IDLE_MARGIN_HNS: i64 = HNS_PER_SEC / 5;

pub enum Cmd {
    Start(StartSpec),
    Stop,
    TogglePause,
    Marker,
    Shutdown,
}

pub struct StreamSpec {
    pub key: String,
    pub label: String,
    pub device_name: String,
    pub kind: DeviceKind,
    pub channels: u16,
    pub file_name: String,
    pub shared: Arc<StreamShared>,
}

pub struct StartSpec {
    pub dir: PathBuf,
    pub streams: Vec<StreamSpec>,
    pub format: AudioFormat,
    pub depth: BitDepth,
    pub rate: u32,
    pub opus_kbps: u32,
    pub write_mix: bool,
    pub started_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Recording,
    Paused,
}

#[derive(Clone, Debug, Default)]
pub struct StreamStat {
    pub key: String,
    #[allow(dead_code)]
    pub frames: u64,
    pub bytes: u64,
    pub silence_s: f64,
    pub dropped_s: f64,
    pub drift_ppm: f64,
    pub err_ms: f64,
}

#[derive(Clone, Debug)]
pub struct SessionView {
    pub phase: Phase,
    pub elapsed_s: f64,
    pub dir: Option<PathBuf>,
    pub streams: Vec<StreamStat>,
    pub mix_bytes: u64,
    pub total_bytes: u64,
    pub markers: Vec<Marker>,
    pub last_result: Option<Result<PathBuf, String>>,
}

impl Default for SessionView {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            elapsed_s: 0.0,
            dir: None,
            streams: Vec::new(),
            mix_bytes: 0,
            total_bytes: 0,
            markers: Vec::new(),
            last_result: None,
        }
    }
}

pub struct SessionShared {
    pub view: Mutex<SessionView>,
}

pub fn spawn(rx_pkt: Receiver<Packet>) -> (Sender<Cmd>, Arc<SessionShared>, JoinHandle<()>) {
    let (tx, rx) = unbounded();
    let shared = Arc::new(SessionShared {
        view: Mutex::new(SessionView::default()),
    });
    let s2 = shared.clone();
    let th = std::thread::Builder::new()
        .name("session".into())
        .spawn(move || run(rx, rx_pkt, s2))
        .expect("spawn session thread");
    (tx, shared, th)
}

fn run(rx_cmd: Receiver<Cmd>, rx_pkt: Receiver<Packet>, shared: Arc<SessionShared>) {
    let mut rec: Option<Recording> = None;
    let mut last_ui = Instant::now() - Duration::from_secs(1);
    loop {
        let mut shutdown = false;
        select! {
            recv(rx_cmd) -> cmd => match cmd {
                Ok(Cmd::Start(spec)) => {
                    if rec.is_none() {
                        // Discard monitoring packets queued before the start.
                        while rx_pkt.try_recv().is_ok() {}
                        match Recording::start(spec) {
                            Ok(r) => {
                                let mut v = shared.view.lock();
                                v.last_result = None;
                                v.markers.clear();
                                rec = Some(r);
                            }
                            Err(e) => shared.view.lock().last_result = Some(Err(format!("{e:#}"))),
                        }
                    }
                }
                Ok(Cmd::Stop) => {
                    if let Some(r) = rec.take() {
                        finish(r, &rx_pkt, &shared);
                    }
                }
                Ok(Cmd::TogglePause) => {
                    if let Some(r) = rec.as_mut() {
                        r.toggle_pause();
                    }
                }
                Ok(Cmd::Marker) => {
                    if let Some(r) = rec.as_mut() {
                        r.marker();
                    }
                }
                Ok(Cmd::Shutdown) | Err(_) => shutdown = true,
            },
            recv(rx_pkt) -> p => {
                if let (Ok(p), Some(r)) = (p, rec.as_mut()) {
                    if let Err(e) = r.on_packet(p) {
                        let r = rec.take().unwrap();
                        let dir = r.dir.clone();
                        let _ = r.stop();
                        let mut v = shared.view.lock();
                        v.phase = Phase::Idle;
                        v.last_result = Some(Err(format!(
                            "Recording stopped: {e:#} (partial files in {})", dir.display()
                        )));
                    }
                }
            },
            default(Duration::from_millis(50)) => {}
        }
        if shutdown {
            if let Some(r) = rec.take() {
                finish(r, &rx_pkt, &shared);
            }
            break;
        }
        if let Some(r) = rec.as_mut() {
            if let Err(e) = r.tick() {
                let r = rec.take().unwrap();
                let _ = r.stop();
                let mut v = shared.view.lock();
                v.phase = Phase::Idle;
                v.last_result = Some(Err(format!("Recording stopped: {e:#}")));
            }
        }
        if last_ui.elapsed() >= Duration::from_millis(100) {
            last_ui = Instant::now();
            let mut v = shared.view.lock();
            match &rec {
                Some(r) => r.fill_view(&mut v),
                None => {
                    v.phase = Phase::Idle;
                }
            }
        }
    }
}

fn finish(mut r: Recording, rx_pkt: &Receiver<Packet>, shared: &SessionShared) {
    // Write out everything already captured before closing the files.
    while let Ok(p) = rx_pkt.try_recv() {
        if r.on_packet(p).is_err() {
            break;
        }
    }
    r.fill_view(&mut shared.view.lock());
    let res = r.stop().map_err(|e| format!("{e:#}"));
    let mut v = shared.view.lock();
    v.phase = Phase::Idle;
    v.last_result = Some(res);
}

struct RecStream {
    spec: StreamSpec,
    aligner: Aligner,
    sink: Box<dyn Sink>,
    last_packet: Option<i64>,
    mixq: VecDeque<f32>,
    native: Option<(u32, u16)>,
}

struct Recording {
    dir: PathBuf,
    rate: u32,
    format: AudioFormat,
    depth: BitDepth,
    opus_kbps: u32,
    started_at: String,
    t0: i64,
    paused_at: Option<i64>,
    streams: Vec<RecStream>,
    mix: Option<Box<dyn Sink>>,
    mix_file: Option<String>,
    markers: Vec<Marker>,
    out: Vec<f32>,
    last_tick: Instant,
    last_flush: Instant,
    last_manifest: Instant,
}

impl Recording {
    fn start(spec: StartSpec) -> Result<Self> {
        std::fs::create_dir_all(&spec.dir)?;
        let sink_spec = SinkSpec {
            format: spec.format,
            depth: spec.depth,
            rate: spec.rate,
            opus_kbps: spec.opus_kbps,
        };
        let t0 = now_hns();
        let mut streams = Vec::new();
        for s in spec.streams {
            let sink = create_sink(&spec.dir.join(&s.file_name), &sink_spec, s.channels)?;
            let native = *s.shared.format.lock();
            streams.push(RecStream {
                aligner: Aligner::new(spec.rate, s.channels as usize, t0),
                sink,
                last_packet: None,
                mixq: VecDeque::new(),
                native,
                spec: s,
            });
        }
        let (mix, mix_file) = if spec.write_mix && !streams.is_empty() {
            let name = format!("mix.{}", spec.format.ext());
            (
                Some(create_sink(&spec.dir.join(&name), &sink_spec, 2)?),
                Some(name),
            )
        } else {
            (None, None)
        };
        let r = Self {
            dir: spec.dir,
            rate: spec.rate,
            format: spec.format,
            depth: spec.depth,
            opus_kbps: spec.opus_kbps,
            started_at: spec.started_at,
            t0,
            paused_at: None,
            streams,
            mix,
            mix_file,
            markers: Vec::new(),
            out: Vec::new(),
            last_tick: Instant::now(),
            last_flush: Instant::now(),
            last_manifest: Instant::now(),
        };
        r.manifest(false, 0.0).save(&r.dir)?;
        Ok(r)
    }

    fn elapsed_s(&self) -> f64 {
        let end = self.paused_at.unwrap_or_else(now_hns);
        (end - self.t0).max(0) as f64 / HNS_PER_SEC as f64
    }

    fn on_packet(&mut self, p: Packet) -> Result<()> {
        if self.paused_at.is_some() {
            return Ok(());
        }
        let Some(i) = self.streams.iter().position(|s| *s.spec.key == *p.key) else {
            return Ok(());
        };
        let mut out = std::mem::take(&mut self.out);
        out.clear();
        {
            let s = &mut self.streams[i];
            s.native = Some((p.rate, p.channels));
            s.last_packet = Some(p.qpc_hns.unwrap_or_else(now_hns));
            s.aligner
                .push(p.rate, p.channels, &p.data, p.qpc_hns, &mut out);
        }
        let r = self.write_out(i, &out);
        self.out = out;
        r
    }

    fn write_out(&mut self, i: usize, data: &[f32]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let s = &mut self.streams[i];
        s.sink.write(data)?;
        if self.mix.is_some() {
            let ch = s.aligner.channels();
            if ch == 1 {
                s.mixq.extend(data.iter().copied());
            } else {
                s.mixq.extend(
                    data.chunks_exact(ch)
                        .map(|f| f.iter().sum::<f32>() / ch as f32),
                );
            }
            self.drain_mix()?;
        }
        Ok(())
    }

    /// Mix file: left = all inputs (mics), right = all loopbacks. If only one kind
    /// is recorded, both channels carry it.
    fn drain_mix(&mut self) -> Result<()> {
        let Some(mix) = self.mix.as_mut() else {
            return Ok(());
        };
        let n = self.streams.iter().map(|s| s.mixq.len()).min().unwrap_or(0);
        if n == 0 {
            return Ok(());
        }
        let has_in = self
            .streams
            .iter()
            .any(|s| s.spec.kind == DeviceKind::Input);
        let has_lb = self
            .streams
            .iter()
            .any(|s| s.spec.kind == DeviceKind::Loopback);
        let mut buf = Vec::with_capacity(n * 2);
        for _ in 0..n {
            let (mut l, mut r) = (0f32, 0f32);
            for s in &mut self.streams {
                let v = s.mixq.pop_front().unwrap_or(0.0);
                match s.spec.kind {
                    DeviceKind::Input => l += v,
                    DeviceKind::Loopback => r += v,
                }
            }
            if !has_lb {
                r = l;
            }
            if !has_in {
                l = r;
            }
            buf.push(l.clamp(-1.0, 1.0));
            buf.push(r.clamp(-1.0, 1.0));
        }
        mix.write(&buf)
    }

    fn tick(&mut self) -> Result<()> {
        if self.last_tick.elapsed() < Duration::from_millis(50) {
            return Ok(());
        }
        self.last_tick = Instant::now();
        if self.paused_at.is_none() {
            let now = now_hns();
            let target = (((now - IDLE_MARGIN_HNS - self.t0).max(0)) as f64 / HNS_PER_SEC as f64
                * self.rate as f64) as u64;
            for i in 0..self.streams.len() {
                let idle = match self.streams[i].last_packet {
                    Some(t) => now - t > IDLE_HNS,
                    None => now - self.t0 > IDLE_HNS,
                };
                if idle {
                    let mut out = std::mem::take(&mut self.out);
                    out.clear();
                    self.streams[i].aligner.pad_to(target, &mut out);
                    let r = self.write_out(i, &out);
                    self.out = out;
                    r?;
                }
            }
        }
        if self.last_flush.elapsed() >= Duration::from_secs(5) {
            self.last_flush = Instant::now();
            for s in &mut self.streams {
                s.sink.flush()?;
            }
            if let Some(m) = self.mix.as_mut() {
                m.flush()?;
            }
        }
        if self.last_manifest.elapsed() >= Duration::from_secs(30) {
            self.last_manifest = Instant::now();
            let _ = self.manifest(false, self.elapsed_s()).save(&self.dir);
        }
        Ok(())
    }

    fn toggle_pause(&mut self) {
        let now = now_hns();
        match self.paused_at.take() {
            Some(p) => {
                let d = now - p;
                self.t0 += d;
                for s in &mut self.streams {
                    s.aligner.shift_t0(d);
                }
                let t = self.elapsed_s();
                self.markers.push(Marker {
                    t_s: t,
                    label: format!("Resumed (paused {:.1} s)", d as f64 / HNS_PER_SEC as f64),
                });
            }
            None => {
                let t = self.elapsed_s();
                self.paused_at = Some(now);
                self.markers.push(Marker {
                    t_s: t,
                    label: "Paused".into(),
                });
            }
        }
    }

    fn marker(&mut self) {
        let n = self
            .markers
            .iter()
            .filter(|m| m.label.starts_with("Marker"))
            .count()
            + 1;
        let t = self.elapsed_s();
        self.markers.push(Marker {
            t_s: t,
            label: format!("Marker {n}"),
        });
    }

    fn manifest(&self, complete: bool, duration_s: f64) -> Manifest {
        Manifest {
            app: format!("MultiRec {}", env!("CARGO_PKG_VERSION")),
            version: 1,
            started_at: self.started_at.clone(),
            duration_s,
            sample_rate: self.rate,
            format: self.format.ext().into(),
            bit_depth: if self.format == AudioFormat::Opus {
                format!("Opus {} kbps", self.opus_kbps)
            } else {
                self.depth.label().into()
            },
            aligned: true,
            streams: self
                .streams
                .iter()
                .map(|s| ManifestStream {
                    label: s.spec.label.clone(),
                    device_name: s.spec.device_name.clone(),
                    kind: s.spec.kind,
                    file: s.spec.file_name.clone(),
                    channels: s.spec.channels,
                    native_rate: s.native.map_or(0, |n| n.0),
                    native_channels: s.native.map_or(0, |n| n.1),
                    silence_inserted_s: s.aligner.stats.silence_s,
                    dropped_s: s.aligner.stats.dropped_s,
                    drift_ppm: s.aligner.stats.drift_ppm,
                })
                .collect(),
            mix_file: self.mix_file.clone(),
            markers: self.markers.clone(),
            complete,
        }
    }

    fn fill_view(&self, v: &mut SessionView) {
        v.phase = if self.paused_at.is_some() {
            Phase::Paused
        } else {
            Phase::Recording
        };
        v.elapsed_s = self.elapsed_s();
        v.dir = Some(self.dir.clone());
        v.streams = self
            .streams
            .iter()
            .map(|s| StreamStat {
                key: s.spec.key.clone(),
                frames: s.sink.frames(),
                bytes: s.sink.bytes(),
                silence_s: s.aligner.stats.silence_s,
                dropped_s: s.aligner.stats.dropped_s,
                drift_ppm: s.aligner.stats.drift_ppm,
                err_ms: s.aligner.stats.err_ms,
            })
            .collect();
        v.mix_bytes = self.mix.as_ref().map_or(0, |m| m.bytes());
        v.total_bytes = v.streams.iter().map(|s| s.bytes).sum::<u64>() + v.mix_bytes;
        v.markers = self.markers.clone();
    }

    /// Pad every stream to the stop instant, equalise lengths and close the files.
    fn stop(mut self) -> Result<PathBuf> {
        let end = self.paused_at.unwrap_or_else(now_hns);
        let final_frames =
            ((end - self.t0).max(0) as f64 / HNS_PER_SEC as f64 * self.rate as f64) as u64;
        let mut first_err: Option<anyhow::Error> = None;
        for i in 0..self.streams.len() {
            let mut out = Vec::new();
            self.streams[i].aligner.pad_to(final_frames, &mut out);
            self.streams[i].aligner.flush(&mut out);
            if let Err(e) = self.write_out(i, &out) {
                first_err.get_or_insert(e);
            }
        }
        let max = self
            .streams
            .iter()
            .map(|s| s.sink.frames())
            .max()
            .unwrap_or(0);
        for i in 0..self.streams.len() {
            let missing = max - self.streams[i].sink.frames();
            if missing > 0 {
                let zeros = vec![0f32; missing as usize * self.streams[i].aligner.channels()];
                if let Err(e) = self.write_out(i, &zeros) {
                    first_err.get_or_insert(e);
                }
            }
        }
        let duration = max as f64 / self.rate as f64;
        let manifest = self.manifest(true, duration);
        for s in self.streams.drain(..) {
            if let Err(e) = s.sink.finalize() {
                first_err.get_or_insert(e);
            }
        }
        if let Some(m) = self.mix.take() {
            if let Err(e) = m.finalize() {
                first_err.get_or_insert(e);
            }
        }
        manifest.save(&self.dir)?;
        match first_err {
            Some(e) => Err(e),
            None => Ok(self.dir),
        }
    }
}

/// Real-hardware end-to-end test (Windows only):
/// `cargo test --release -- --ignored --nocapture records_default_devices`
#[cfg(all(test, windows))]
mod hw_tests {
    use super::*;
    use crate::audio::{list_devices, spawn_capture};
    use crate::recorder::manifest::Manifest;

    #[test]
    #[ignore]
    fn records_default_devices() {
        let devs = list_devices().unwrap();
        for d in &devs {
            println!("{:?} default={} {}", d.kind, d.is_default, d.name);
        }
        let pick: Vec<_> = [DeviceKind::Input, DeviceKind::Loopback]
            .iter()
            .filter_map(|k| devs.iter().find(|d| d.kind == *k && d.is_default))
            .cloned()
            .collect();
        assert!(!pick.is_empty(), "no default devices");
        let (pkt_tx, pkt_rx) = unbounded();
        let (cmd, shared, th) = spawn(pkt_rx);
        let mut caps = Vec::new();
        let mut specs = Vec::new();
        for (i, d) in pick.iter().enumerate() {
            let sh = Arc::new(StreamShared::new());
            caps.push(spawn_capture(d, sh.clone(), pkt_tx.clone(), true));
            specs.push((d.clone(), sh, format!("s{i}")));
        }
        std::thread::sleep(Duration::from_millis(700));
        for (d, sh, _) in &specs {
            println!(
                "{} status={:?} format={:?}",
                d.name,
                *sh.status.lock(),
                *sh.format.lock()
            );
        }
        for format in [AudioFormat::Wav, AudioFormat::Flac, AudioFormat::Opus] {
            // A fresh session per format; reuse the running captures.
            let dir = std::env::temp_dir().join(format!("multirec_hw_test_{}", format.ext()));
            let _ = std::fs::remove_dir_all(&dir);
            cmd.send(Cmd::Start(StartSpec {
                dir: dir.clone(),
                streams: specs
                    .iter()
                    .map(|(d, sh, f)| StreamSpec {
                        key: d.key.clone(),
                        label: d.name.clone(),
                        device_name: d.name.clone(),
                        kind: d.kind,
                        channels: 1,
                        file_name: format!("{f}.{}", format.ext()),
                        shared: sh.clone(),
                    })
                    .collect(),
                format,
                depth: BitDepth::S16,
                rate: 48_000,
                opus_kbps: 32,
                write_mix: true,
                started_at: "test".into(),
            }))
            .unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            cmd.send(Cmd::Marker).unwrap();
            std::thread::sleep(Duration::from_millis(1500));
            {
                let v = shared.view.lock();
                for s in &v.streams {
                    println!(
                        "live: {} frames={} drift={:.1}ppm err={:.2}ms silence={:.3}s dropped={:.3}s",
                        s.key, s.frames, s.drift_ppm, s.err_ms, s.silence_s, s.dropped_s
                    );
                }
            }
            cmd.send(Cmd::Stop).unwrap();
            let mut res = None;
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(50));
                if let Some(r) = shared.view.lock().last_result.clone() {
                    res = Some(r);
                    break;
                }
            }
            println!("result: {res:?}");
            assert!(matches!(res, Some(Ok(_))));
            let m = Manifest::load(&dir).unwrap();
            println!(
                "== {format:?}: {} streams, {:.2} s",
                m.streams.len(),
                m.duration_s
            );
            assert!(m.complete);
            assert!(
                (m.duration_s - 3.0).abs() < 0.3,
                "duration {}",
                m.duration_s
            );
            let mut lens = Vec::new();
            for s in &m.streams {
                let a = crate::transcribe::audio_io::read_interleaved(&dir.join(&s.file)).unwrap();
                let peak = a.samples.iter().fold(0f32, |p, v| p.max(v.abs()));
                let bytes = std::fs::metadata(dir.join(&s.file))
                    .map(|m| m.len())
                    .unwrap_or(0);
                println!(
                    "{}: {} frames, peak {:.4}, {} bytes",
                    s.file,
                    a.samples.len(),
                    peak,
                    bytes
                );
                lens.push(a.samples.len());
            }
            let mix = crate::transcribe::audio_io::read_interleaved(
                &dir.join(format!("mix.{}", format.ext())),
            )
            .unwrap();
            lens.push(mix.samples.len() / 2);
            assert!(
                lens.windows(2).all(|w| w[0] == w[1]),
                "lengths differ: {lens:?}"
            );
            assert_eq!(m.markers.len(), 1);
        }
        drop(caps);
        cmd.send(Cmd::Shutdown).unwrap();
        th.join().unwrap();
    }
}
