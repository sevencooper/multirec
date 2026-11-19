//! Post-processing: per-stream speech-to-text, prosody analysis, bleed
//! suppression and merging into one speaker-labelled conversation.

pub mod audio_io;
pub mod bleed;
#[cfg(feature = "whisper")]
mod engine_whisper;
pub mod export;
pub mod merge;
pub mod models;
pub mod prosody;

use crate::audio::DeviceKind;
use crate::recorder::manifest::Manifest;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize)]
pub struct WordProsody {
    /// Median pitch over the word (Hz), if voiced.
    pub f0_hz: Option<f32>,
    /// RMS loudness over the word (dBFS).
    pub loud_db: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub prob: f32,
    /// Non-speech annotation from the model, e.g. "[laughs]".
    pub is_tag: bool,
    pub prosody: Option<WordProsody>,
    pub emphasis: bool,
    /// Silence after this word inside an utterance (s), if notable.
    pub pause_after: Option<f64>,
    /// Pitch samples (absolute time s, Hz) used for contour analysis.
    #[serde(skip)]
    pub pitch: Vec<(f32, f32)>,
}

impl Word {
    pub fn new(text: String, start: f64, end: f64, prob: f32) -> Self {
        let t = text.trim();
        let is_tag = (t.starts_with('[') && t.ends_with(']'))
            || (t.starts_with('(') && t.ends_with(')'))
            || (t.len() > 2 && t.starts_with('*') && t.ends_with('*'))
            || t.chars().all(|c| c == '♪' || c.is_whitespace());
        Self {
            text: t.to_string(),
            start,
            end,
            prob,
            is_tag,
            prosody: None,
            emphasis: false,
            pause_after: None,
            pitch: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Track {
    pub label: String,
    pub kind: DeviceKind,
    pub file: String,
    pub words: Vec<Word>,
    pub baseline_f0: Option<f32>,
    pub baseline_db: Option<f32>,
    pub median_wpm: Option<f32>,
}

pub struct JobOptions {
    pub session_dir: PathBuf,
    pub model_id: String,
    pub language: String,
    pub threads: usize,
    pub bleed: bool,
    pub prosody: bool,
}

#[derive(Default)]
pub struct JobProgress {
    pub stage: String,
    pub fraction: f32,
    pub log: Vec<String>,
    pub done: bool,
    pub error: Option<String>,
    pub outputs: Vec<PathBuf>,
    pub cancel: bool,
}

pub type Progress = Arc<Mutex<JobProgress>>;

pub fn engine_available() -> bool {
    cfg!(feature = "whisper")
}

pub fn run_job(opts: JobOptions, p: Progress) {
    let r = run_inner(&opts, &p);
    let mut g = p.lock();
    g.done = true;
    match r {
        Ok(outs) => {
            g.outputs = outs;
            g.stage = "Done".into();
            g.fraction = 1.0;
        }
        Err(e) => {
            g.stage = "Failed".into();
            g.error = Some(format!("{e:#}"));
        }
    }
}

fn log(p: &Progress, msg: impl Into<String>) {
    p.lock().log.push(msg.into());
}

fn check_cancel(p: &Progress) -> Result<()> {
    if p.lock().cancel {
        bail!("cancelled");
    }
    Ok(())
}

/// Split audio into chunks of ~`target_s` seconds, cutting at the quietest point
/// within ±`search_s` of each boundary.
pub fn plan_chunks(
    audio: &[f32],
    sr: usize,
    target_s: usize,
    search_s: usize,
) -> Vec<(usize, usize)> {
    let target = target_s * sr;
    let search = search_s * sr;
    let win = sr / 5;
    let len = audio.len();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < len {
        let mut end = (start + target).min(len);
        if end < len && end > start + search && end + win < len {
            let lo = end - search;
            let hi = (end + search).min(len - win);
            let mut best = end;
            let mut best_e = f32::MAX;
            let mut i = lo;
            while i < hi {
                let e: f32 = audio[i..i + win].iter().map(|v| v * v).sum();
                if e < best_e {
                    best_e = e;
                    best = i + win / 2;
                }
                i += win / 2;
            }
            end = best;
        }
        if len - end < sr / 2 {
            end = len;
        }
        chunks.push((start, end));
        start = end;
    }
    chunks
}

/// Energy-based voice activity detection → slices (≤ 28 s) that contain speech.
/// Whisper then never sees long silences (no hallucinations, tight timestamps).
pub fn plan_speech_batches(audio: &[f32], sr: usize) -> Vec<(usize, usize)> {
    const MAX_BATCH_S: f64 = 28.0;
    let hop = sr * 30 / 1000;
    let floor = prosody::noise_floor_db(audio, sr);
    let thr = (floor + 10.0).max(-55.0);
    let voiced: Vec<bool> = audio
        .chunks(hop)
        .map(|c| prosody::rms_db(c) > thr)
        .collect();
    // Raw regions in frames.
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < voiced.len() {
        if voiced[i] {
            let s = i;
            while i < voiced.len() && voiced[i] {
                i += 1;
            }
            regions.push((s, i));
        } else {
            i += 1;
        }
    }
    let to_s = |f: usize| f as f64 * hop as f64 / sr as f64;
    // Merge regions separated by short gaps, drop tiny blips, pad.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in regions {
        let (a, b) = (to_s(a), to_s(b));
        match merged.last_mut() {
            Some(last) if a - last.1 < 0.6 => last.1 = b,
            _ => merged.push((a, b)),
        }
    }
    let total = audio.len() as f64 / sr as f64;
    let merged: Vec<(f64, f64)> = merged
        .into_iter()
        .filter(|(a, b)| b - a >= 0.12)
        .map(|(a, b)| ((a - 0.3).max(0.0), (b + 0.3).min(total)))
        .collect();
    // Group into batches; split over-long regions at quiet points.
    let mut batches: Vec<(f64, f64)> = Vec::new();
    for (a, b) in merged {
        if b - a > MAX_BATCH_S {
            let sa = (a * sr as f64) as usize;
            let sb = ((b * sr as f64) as usize).min(audio.len());
            for (x, y) in plan_chunks(&audio[sa..sb], sr, 25, 3) {
                batches.push(((sa + x) as f64 / sr as f64, (sa + y) as f64 / sr as f64));
            }
            continue;
        }
        match batches.last_mut() {
            Some(last) if b - last.0 <= MAX_BATCH_S && a <= last.1 + 8.0 => last.1 = b,
            _ => batches.push((a, b)),
        }
    }
    batches
        .into_iter()
        .map(|(a, b)| {
            (
                (a * sr as f64) as usize,
                ((b * sr as f64) as usize).min(audio.len()),
            )
        })
        .filter(|(a, b)| b > a)
        .collect()
}

fn run_inner(opts: &JobOptions, p: &Progress) -> Result<Vec<PathBuf>> {
    let manifest = Manifest::load(&opts.session_dir)?;
    if manifest.streams.is_empty() {
        bail!("session has no streams");
    }
    let model = models::find(&opts.model_id)?;
    if !models::is_downloaded(model) {
        log(
            p,
            format!("Downloading model {} (~{} MB)…", model.id, model.size_mb),
        );
    }
    let model_path = models::ensure(model, |done, total| {
        let mut g = p.lock();
        g.stage = format!(
            "Downloading model: {:.0} / {} MB",
            done as f64 / 1e6,
            total.map_or("?".into(), |t| format!("{:.0}", t as f64 / 1e6))
        );
        g.fraction = total.map_or(0.0, |t| done as f32 / t as f32);
        !g.cancel
    })?;

    #[cfg(feature = "whisper")]
    let engine = {
        p.lock().stage = "Loading model…".into();
        engine_whisper::Engine::load(&model_path, &opts.model_id, opts.threads, &opts.language)?
    };
    #[cfg(not(feature = "whisper"))]
    {
        let _ = model_path;
        bail!("this build has no speech-to-text engine (compiled without the `whisper` feature)");
    }

    let n = manifest.streams.len() as f32;
    let mut tracks = Vec::new();
    #[allow(unreachable_code)]
    for (si, s) in manifest.streams.iter().enumerate() {
        check_cancel(p)?;
        {
            let mut g = p.lock();
            g.stage = format!("Loading {}…", s.label);
            g.fraction = si as f32 / n;
        }
        let audio = audio_io::load_mono_16k(&opts.session_dir.join(&s.file))?;
        let chunks = plan_speech_batches(&audio, audio_io::SR);
        let speech_s: f64 =
            chunks.iter().map(|(a, b)| (b - a) as f64).sum::<f64>() / audio_io::SR as f64;
        log(
            p,
            format!(
                "{}: {:.0} s of speech activity in {} part(s)",
                s.label,
                speech_s,
                chunks.len()
            ),
        );
        let mut words = Vec::new();
        for (ci, &(a, b)) in chunks.iter().enumerate() {
            check_cancel(p)?;
            {
                let mut g = p.lock();
                g.stage = format!(
                    "Transcribing {} — part {}/{} ({})",
                    s.label,
                    ci + 1,
                    chunks.len(),
                    export::fmt_ts(a as f64 / audio_io::SR as f64)
                );
                g.fraction = (si as f32 + 0.9 * ci as f32 / chunks.len() as f32) / n;
            }
            let seg = &audio[a..b];
            #[cfg(feature = "whisper")]
            words.extend(engine.transcribe(seg, a as f64 / audio_io::SR as f64)?);
        }
        p.lock().stage = format!("Analysing prosody for {}…", s.label);
        let dropped = prosody::annotate_words(&mut words, &audio, audio_io::SR, opts.prosody);
        if dropped > 0 {
            log(
                p,
                format!(
                    "{}: removed {dropped} word(s) detected over silence (likely hallucinations)",
                    s.label
                ),
            );
        }
        log(p, format!("{}: {} words", s.label, words.len()));
        tracks.push(Track {
            label: s.label.clone(),
            kind: s.kind,
            file: s.file.clone(),
            words,
            baseline_f0: None,
            baseline_db: None,
            median_wpm: None,
        });
    }

    if opts.bleed {
        let d = bleed::suppress(&mut tracks);
        if d > 0 {
            log(
                p,
                format!("Removed {d} mic word(s) that were speaker bleed of the system audio"),
            );
        }
    }
    p.lock().stage = "Building conversation…".into();
    let conv = merge::build(&mut tracks, opts.prosody);
    let outs = export::write_all(
        &opts.session_dir,
        &manifest,
        &tracks,
        &conv,
        &opts.model_id,
        &opts.language,
    )?;
    for o in &outs {
        log(p, format!("Wrote {}", o.display()));
    }
    Ok(outs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_everything() {
        let sr = 1000;
        let audio: Vec<f32> = (0..sr * 1000)
            .map(|i| ((i / 37) % 7) as f32 * 0.1)
            .collect();
        let c = plan_chunks(&audio, sr, 300, 15);
        assert_eq!(c.first().unwrap().0, 0);
        assert_eq!(c.last().unwrap().1, audio.len());
        for w in c.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
        assert!(c.len() >= 3 && c.len() <= 4);
    }

    #[test]
    fn vad_finds_speech_regions() {
        let sr = 16_000;
        let mut audio = vec![0.0005f32; sr * 120];
        // Speech bursts at 10-14 s and 60-90 s (the latter longer than one batch).
        for (a, b) in [(10, 14), (60, 90)] {
            for i in a * sr..b * sr {
                audio[i] = (i as f32 * 0.05).sin() * 0.3;
            }
        }
        let bs = plan_speech_batches(&audio, sr);
        assert!(bs.len() >= 3, "{bs:?}");
        let secs: Vec<(f64, f64)> = bs
            .iter()
            .map(|&(a, b)| (a as f64 / sr as f64, b as f64 / sr as f64))
            .collect();
        assert!(
            (secs[0].0 - 9.7).abs() < 0.2 && (secs[0].1 - 14.3).abs() < 0.2,
            "{secs:?}"
        );
        assert!(secs.iter().all(|(a, b)| b - a <= 28.5));
        assert!((secs.last().unwrap().1 - 90.3).abs() < 0.2);
    }

    #[test]
    fn tags_detected() {
        assert!(Word::new(" [laughs]".into(), 0.0, 1.0, 1.0).is_tag);
        assert!(Word::new("(music)".into(), 0.0, 1.0, 1.0).is_tag);
        assert!(!Word::new(" hello".into(), 0.0, 1.0, 1.0).is_tag);
    }
}

/// End-to-end transcription on a synthetic two-speaker session (needs network
/// for the model on first run). Point MULTIREC_TTS_DIR at me1/me2/other1/other2.wav:
/// `cargo test --release -- --ignored --nocapture transcribes_synthetic_session`
#[cfg(test)]
mod e2e {
    use super::*;
    use crate::recorder::manifest::{ManifestStream, Marker};

    fn load(p: &std::path::Path) -> Vec<f32> {
        audio_io::read_interleaved(p).unwrap().samples
    }

    #[test]
    #[ignore]
    fn transcribes_synthetic_session() {
        let src = PathBuf::from(std::env::var("MULTIREC_TTS_DIR").expect("MULTIREC_TTS_DIR"));
        let sr = 48_000usize;
        let me1 = load(&src.join("me1.wav"));
        let me2 = load(&src.join("me2.wav"));
        let o1 = load(&src.join("other1.wav"));
        let o2 = load(&src.join("other2.wav"));
        let at = |t: f64| (t * sr as f64) as usize;
        let t_me1 = 0.5;
        let t_o1 = t_me1 + me1.len() as f64 / sr as f64 + 0.4;
        let t_me2 = t_o1 + o1.len() as f64 / sr as f64 + 0.6;
        let t_o2 = t_me2 + me2.len() as f64 / sr as f64 + 2.5;
        let total = at(t_o2) + o2.len() + sr;
        let mut me = vec![0f32; total];
        let mut others = vec![0f32; total];
        let mut place = |dst: &mut Vec<f32>, clip: &[f32], t: f64, g: f32| {
            for (i, &v) in clip.iter().enumerate() {
                dst[at(t) + i] += v * g;
            }
        };
        place(&mut me, &me1, t_me1, 1.0);
        place(&mut me, &me2, t_me2, 1.0);
        place(&mut others, &o1, t_o1, 1.0);
        place(&mut others, &o2, t_o2, 1.0);
        // Speaker bleed: remote voice leaking into the mic at -12 dB.
        place(&mut me, &o1, t_o1, 0.25);

        let dir = std::env::temp_dir().join("multirec_tr_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, data) in [("Me.wav", &me), ("Others.wav", &others)] {
            let mut s = crate::recorder::writer::create_sink(
                &dir.join(name),
                crate::config::AudioFormat::Wav,
                crate::config::BitDepth::S16,
                sr as u32,
                1,
            )
            .unwrap();
            s.write(data).unwrap();
            s.finalize().unwrap();
        }
        let stream = |label: &str, kind| ManifestStream {
            label: label.into(),
            device_name: "test".into(),
            kind,
            file: format!("{label}.wav"),
            channels: 1,
            native_rate: 48_000,
            native_channels: 1,
            silence_inserted_s: 0.0,
            dropped_s: 0.0,
            drift_ppm: 0.0,
        };
        Manifest {
            app: "test".into(),
            version: 1,
            started_at: "2026-01-01T12:00:00Z".into(),
            duration_s: total as f64 / sr as f64,
            sample_rate: 48_000,
            format: "wav".into(),
            bit_depth: "16-bit".into(),
            aligned: true,
            streams: vec![
                stream("Me", DeviceKind::Input),
                stream("Others", DeviceKind::Loopback),
            ],
            mix_file: None,
            markers: vec![Marker {
                t_s: t_me2 - 0.1,
                label: "Marker 1".into(),
            }],
            complete: true,
        }
        .save(&dir)
        .unwrap();

        let p: Progress = Arc::new(Mutex::new(JobProgress::default()));
        run_job(
            JobOptions {
                session_dir: dir.clone(),
                model_id: std::env::var("MULTIREC_TEST_MODEL").unwrap_or_else(|_| "base.en".into()),
                language: "en".into(),
                threads: 8,
                bleed: true,
                prosody: true,
            },
            p.clone(),
        );
        let g = p.lock();
        for l in &g.log {
            println!("log: {l}");
        }
        assert!(g.error.is_none(), "{:?}", g.error);
        let md = std::fs::read_to_string(dir.join("transcript.md")).unwrap();
        println!("{md}");
        println!(
            "{}",
            std::fs::read_to_string(dir.join("transcript.srt")).unwrap()
        );
        let lower = md.to_lowercase();
        assert!(lower.contains("hear me"), "missing speaker 1 text");
        assert!(lower.contains("drift"), "missing speaker 2 text");
        // Remote words must be attributed to Others only (bleed removed from Me).
        let me_lines: String = md
            .lines()
            .filter(|l| l.contains("] Me:**"))
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        assert!(
            !me_lines.contains("perfectly"),
            "bleed not removed: {me_lines}"
        );
    }
}
