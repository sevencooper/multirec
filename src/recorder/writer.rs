use super::flac::FlacEncoder;
use super::opus::OpusWriter;
use crate::config::{AudioFormat, BitDepth};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// Everything needed to create a file of the session's format.
#[derive(Clone, Copy, Debug)]
pub struct SinkSpec {
    pub format: AudioFormat,
    pub depth: BitDepth,
    pub rate: u32,
    /// Opus bitrate per channel.
    pub opus_kbps: u32,
}

/// An audio file being written. Samples are interleaved f32 in [-1, 1].
pub trait Sink: Send {
    fn write(&mut self, interleaved: &[f32]) -> Result<()>;
    fn frames(&self) -> u64;
    fn bytes(&self) -> u64;
    /// Make everything written so far durable/readable (crash safety).
    fn flush(&mut self) -> Result<()>;
    fn finalize(self: Box<Self>) -> Result<()>;
}

/// Float → integer PCM. 16-bit output gets TPDF dither (±1 LSB), which turns
/// quantization distortion on quiet speech into benign noise. Exact zeros are
/// left untouched so digital silence stays silent (and compresses to nothing).
struct Quantizer {
    bits: u32,
    rng: u32,
}

impl Quantizer {
    fn new(bits: u32) -> Self {
        Self {
            bits,
            rng: 0x9E37_79B9,
        }
    }

    #[inline]
    fn uniform(&mut self) -> f32 {
        // xorshift32
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    #[inline]
    fn q(&mut self, x: f32) -> i32 {
        let m = ((1i64 << (self.bits - 1)) - 1) as f32;
        let mut v = x.clamp(-1.0, 1.0) * m;
        if self.bits <= 16 && x != 0.0 {
            v += self.uniform() - self.uniform();
        }
        (v.round() as i32).clamp(-(m as i32) - 1, m as i32)
    }
}

struct WavSink {
    w: hound::WavWriter<BufWriter<File>>,
    depth: BitDepth,
    q: Quantizer,
    channels: u16,
    frames: u64,
}

impl Sink for WavSink {
    fn write(&mut self, data: &[f32]) -> Result<()> {
        match self.depth {
            BitDepth::S16 => {
                for &x in data {
                    let v = self.q.q(x) as i16;
                    self.w.write_sample(v)?;
                }
            }
            BitDepth::S24 => {
                for &x in data {
                    let v = self.q.q(x);
                    self.w.write_sample(v)?;
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
    q: Quantizer,
    channels: u16,
    frames: u64,
    buf: Vec<i32>,
}

impl Sink for FlacSink {
    fn write(&mut self, data: &[f32]) -> Result<()> {
        self.buf.clear();
        for &x in data {
            let v = self.q.q(x);
            self.buf.push(v);
        }
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

struct OpusSink {
    w: OpusWriter<BufWriter<File>>,
    channels: u16,
    frames: u64,
}

impl Sink for OpusSink {
    fn write(&mut self, data: &[f32]) -> Result<()> {
        self.w.write(data)?;
        self.frames += (data.len() / self.channels as usize) as u64;
        Ok(())
    }
    fn frames(&self) -> u64 {
        self.frames
    }
    fn bytes(&self) -> u64 {
        self.w.bytes_written()
    }
    fn flush(&mut self) -> Result<()> {
        self.w.flush()
    }
    fn finalize(self: Box<Self>) -> Result<()> {
        self.w.finish()?;
        Ok(())
    }
}

pub fn create_sink(path: &Path, spec: &SinkSpec, channels: u16) -> Result<Box<dyn Sink>> {
    let create = || File::create(path).with_context(|| format!("creating {}", path.display()));
    match spec.format {
        AudioFormat::Wav => {
            let depth = spec.depth;
            let wspec = hound::WavSpec {
                channels,
                sample_rate: spec.rate,
                bits_per_sample: depth.bits() as u16,
                sample_format: if depth == BitDepth::F32 {
                    hound::SampleFormat::Float
                } else {
                    hound::SampleFormat::Int
                },
            };
            let w = hound::WavWriter::create(path, wspec)
                .with_context(|| format!("creating {}", path.display()))?;
            Ok(Box::new(WavSink {
                w,
                depth,
                q: Quantizer::new(depth.bits()),
                channels,
                frames: 0,
            }))
        }
        AudioFormat::Flac => {
            let bits = if spec.depth == BitDepth::S16 { 16 } else { 24 };
            let enc = FlacEncoder::new(
                BufWriter::with_capacity(256 * 1024, create()?),
                channels as usize,
                bits,
                spec.rate,
            )?;
            Ok(Box::new(FlacSink {
                enc,
                q: Quantizer::new(bits),
                channels,
                frames: 0,
                buf: Vec::new(),
            }))
        }
        AudioFormat::Opus => {
            anyhow::ensure!(spec.rate == super::opus::RATE, "Opus requires 48 kHz");
            let w = OpusWriter::new(
                BufWriter::with_capacity(64 * 1024, create()?),
                channels as usize,
                spec.opus_kbps,
            )?;
            Ok(Box::new(OpusSink {
                w,
                channels,
                frames: 0,
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
            (AudioFormat::Flac, BitDepth::S16),
        ] {
            let p = dir.join(format!("t_{:?}.{}", depth, fmt.ext()));
            let spec = SinkSpec {
                format: fmt,
                depth,
                rate: 48_000,
                opus_kbps: 32,
            };
            let mut s = create_sink(&p, &spec, 2).unwrap();
            s.write(&data).unwrap();
            s.flush().unwrap();
            assert_eq!(s.frames(), 4500);
            s.finalize().unwrap();
            let back = crate::transcribe::audio_io::read_interleaved(&p).unwrap();
            assert_eq!(back.channels, 2);
            assert_eq!(back.samples.len(), data.len());
            // 16-bit carries ±1 LSB of dither.
            let tol = if depth == BitDepth::S16 {
                2.0 / 32768.0
            } else {
                1e-4
            };
            for (a, b) in data.iter().zip(&back.samples) {
                assert!((a - b).abs() < tol, "{fmt:?} {depth:?}: {a} vs {b}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dither_keeps_silence_exact() {
        let mut q = Quantizer::new(16);
        assert!((0..10_000).all(|_| q.q(0.0) == 0));
        // Dither is zero-mean: a constant small value averages back to itself.
        let x = 100.3 / 32767.0;
        let mean = (0..100_000).map(|_| q.q(x) as f64).sum::<f64>() / 100_000.0;
        assert!((mean - 100.3).abs() < 0.05, "mean {mean}");
    }
}
