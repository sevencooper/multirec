use crate::dsp::Resampler;
use anyhow::{bail, Context, Result};
use std::path::Path;

pub const SR: usize = 16_000;

#[allow(dead_code)]
pub struct Interleaved {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

/// Streams samples of a WAV/FLAC file as f32 to `f(sample)`; returns (rate, channels).
fn for_each_sample(path: &Path, mut f: impl FnMut(f32)) -> Result<(u32, usize)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wav" => {
            let mut r = hound::WavReader::open(path)
                .with_context(|| format!("opening {}", path.display()))?;
            let spec = r.spec();
            match spec.sample_format {
                hound::SampleFormat::Float => {
                    for s in r.samples::<f32>() {
                        f(s?);
                    }
                }
                hound::SampleFormat::Int => {
                    let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
                    for s in r.samples::<i32>() {
                        f(s? as f32 * scale);
                    }
                }
            }
            Ok((spec.sample_rate, spec.channels as usize))
        }
        "flac" => {
            let mut r = claxon::FlacReader::open(path)
                .with_context(|| format!("opening {}", path.display()))?;
            let info = r.streaminfo();
            let scale = 1.0 / (1u64 << (info.bits_per_sample - 1)) as f32;
            for s in r.samples() {
                f(s? as f32 * scale);
            }
            Ok((info.sample_rate, info.channels as usize))
        }
        "opus" | "ogg" => {
            let file =
                std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
            let ch = crate::recorder::opus::decode(std::io::BufReader::new(file), f)?;
            Ok((crate::recorder::opus::RATE, ch))
        }
        _ => bail!("unsupported audio file: {}", path.display()),
    }
}

#[allow(dead_code)]
pub fn read_interleaved(path: &Path) -> Result<Interleaved> {
    let mut samples = Vec::new();
    let (rate, channels) = for_each_sample(path, |s| samples.push(s))?;
    Ok(Interleaved {
        rate,
        channels,
        samples,
    })
}

/// Load any recorded file as mono 16 kHz f32 (Whisper's input format).
pub fn load_mono_16k(path: &Path) -> Result<Vec<f32>> {
    // Channel count/rate are only known after opening; peek first.
    let (rate, ch) = probe(path)?;
    let mut rs = Resampler::new(rate, SR as u32);
    let mut out = Vec::new();
    let mut inbuf = Vec::with_capacity(16_384);
    let (mut sum, mut n) = (0f32, 0usize);
    for_each_sample(path, |s| {
        sum += s;
        n += 1;
        if n == ch {
            inbuf.push(sum / ch as f32);
            sum = 0.0;
            n = 0;
            if inbuf.len() >= 16_384 {
                rs.push(&inbuf);
                inbuf.clear();
                rs.process(&mut out);
            }
        }
    })?;
    rs.push(&inbuf);
    rs.flush(&mut out);
    Ok(out)
}

fn probe(path: &Path) -> Result<(u32, usize)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "flac" {
        let r = claxon::FlacReader::open(path)?;
        let i = r.streaminfo();
        Ok((i.sample_rate, i.channels as usize))
    } else if ext == "opus" || ext == "ogg" {
        let f = std::io::BufReader::new(std::fs::File::open(path)?);
        Ok((
            crate::recorder::opus::RATE,
            crate::recorder::opus::probe(f)?,
        ))
    } else {
        let r = hound::WavReader::open(path)?;
        let s = r.spec();
        Ok((s.sample_rate, s.channels as usize))
    }
}
