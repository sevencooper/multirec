use super::flac::FlacEncoder;
use crate::config::{AudioFormat, BitDepth};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// An audio file being written. Samples are interleaved f32 in [-1, 1].
pub trait Sink: Send {
    fn write(&mut self, interleaved: &[f32]) -> Result<()>;
    fn frames(&self) -> u64;
    fn bytes(&self) -> u64;
    /// Make everything written so far durable/readable (crash safety).
    fn flush(&mut self) -> Result<()>;
    fn finalize(self: Box<Self>) -> Result<()>;
}

#[inline]
fn to_int(x: f32, bits: u32) -> i32 {
    let m = ((1i64 << (bits - 1)) - 1) as f32;
    (x.clamp(-1.0, 1.0) * m).round() as i32
}

struct WavSink {
    w: hound::WavWriter<BufWriter<File>>,
    depth: BitDepth,
    channels: u16,
    frames: u64,
}

impl Sink for WavSink {
    fn write(&mut self, data: &[f32]) -> Result<()> {
        match self.depth {
            BitDepth::S16 => {
                for &x in data {
                    self.w.write_sample(to_int(x, 16) as i16)?;
                }
            }
            BitDepth::S24 => {
                for &x in data {
                    self.w.write_sample(to_int(x, 24))?;
                }
            }
            BitDepth::F32 => {
                for &x in data {
                    self.w.write_sample(x)?;
                }
            }
        }
        self.frames += (data.len() / self.channels as usize) as u64;
        Ok(())
    }
    fn frames(&self) -> u64 {
        self.frames
    }
    fn bytes(&self) -> u64 {
        44 + self.frames * self.channels as u64 * self.depth.bytes() as u64
    }
    fn flush(&mut self) -> Result<()> {
        // hound rewrites the RIFF/data sizes on flush, so the file stays playable.
        self.w.flush()?;
        Ok(())
    }
    fn finalize(self: Box<Self>) -> Result<()> {
        self.w.finalize()?;
        Ok(())
    }
}

struct FlacSink {
    enc: FlacEncoder<BufWriter<File>>,
    bits: u32,
    channels: u16,
    frames: u64,
    buf: Vec<i32>,
}

impl Sink for FlacSink {
    fn write(&mut self, data: &[f32]) -> Result<()> {
        self.buf.clear();
        self.buf.extend(data.iter().map(|&x| to_int(x, self.bits)));
        self.enc.write(&self.buf)?;
        self.frames += (data.len() / self.channels as usize) as u64;
        Ok(())
    }
    fn frames(&self) -> u64 {
        self.frames
    }
    fn bytes(&self) -> u64 {
        self.enc.bytes_written()
    }
    fn flush(&mut self) -> Result<()> {
        self.enc.flush()?;
        Ok(())
    }
    fn finalize(self: Box<Self>) -> Result<()> {
        self.enc.finish()?;
        Ok(())
    }
}

pub fn create_sink(
    path: &Path,
    format: AudioFormat,
    depth: BitDepth,
    rate: u32,
    channels: u16,
) -> Result<Box<dyn Sink>> {
    match format {
        AudioFormat::Wav => {
            let spec = hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: depth.bits() as u16,
                sample_format: if depth == BitDepth::F32 {
                    hound::SampleFormat::Float
                } else {
                    hound::SampleFormat::Int
                },
            };
            let w = hound::WavWriter::create(path, spec)
                .with_context(|| format!("creating {}", path.display()))?;
            Ok(Box::new(WavSink {
                w,
                depth,
                channels,
                frames: 0,
            }))
        }
        AudioFormat::Flac => {
            let bits = if depth == BitDepth::S16 { 16 } else { 24 };
            let f = File::create(path).with_context(|| format!("creating {}", path.display()))?;
            let enc = FlacEncoder::new(
                BufWriter::with_capacity(256 * 1024, f),
                channels as usize,
                bits,
                rate,
            )?;
            Ok(Box::new(FlacSink {
                enc,
                bits,
                channels,
                frames: 0,
                buf: Vec::new(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_and_flac_roundtrip() {
        let dir = std::env::temp_dir().join(format!("multirec_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<f32> = (0..9000).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        for (fmt, depth) in [
            (AudioFormat::Wav, BitDepth::S16),
            (AudioFormat::Wav, BitDepth::S24),
            (AudioFormat::Wav, BitDepth::F32),
            (AudioFormat::Flac, BitDepth::S24),
        ] {
            let p = dir.join(format!("t_{:?}.{}", depth, fmt.ext()));
            let mut s = create_sink(&p, fmt, depth, 48_000, 2).unwrap();
            s.write(&data).unwrap();
            s.flush().unwrap();
            assert_eq!(s.frames(), 4500);
            s.finalize().unwrap();
            let back = crate::transcribe::audio_io::read_interleaved(&p).unwrap();
            assert_eq!(back.channels, 2);
            assert_eq!(back.samples.len(), data.len());
            for (a, b) in data.iter().zip(&back.samples) {
                assert!((a - b).abs() < 1e-4, "{fmt:?} {depth:?}: {a} vs {b}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
