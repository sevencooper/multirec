//! Streaming Ogg Opus writer and reader.
//!
//! Timing contract (keeps tracks sample-aligned across formats):
//! - The encoder's algorithmic delay is stored as `pre_skip` in the OpusHead header;
//!   decoders drop that many samples from the start.
//! - The final page's granule position is `pre_skip + frames_written`, so decoders trim
//!   the padding of the last frame. Decoded length == frames passed to `write`, exactly.
//! - Pages are closed about once a second, so a crash leaves a playable file.

use anyhow::{anyhow, bail, Context, Result};
use ogg::writing::PacketWriteEndInfo;
use ogg::{PacketReader, PacketWriter};
use std::io::{self, Read, Seek, Write};

/// Opus always runs at 48 kHz here.
pub const RATE: u32 = 48_000;
/// 20 ms frames: the sweet spot for quality vs overhead.
const FRAME: usize = 960;
/// Close a page every 50 packets (≈ 1 s).
const PACKETS_PER_PAGE: u32 = 50;
const MAX_PACKET: usize = 4000;

/// Counts bytes passing through to the inner writer.
pub struct Counting<W: Write> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub struct OpusWriter<W: Write> {
    pw: PacketWriter<'static, Counting<W>>,
    enc: opus::Encoder,
    channels: usize,
    serial: u32,
    pre_skip: u64,
    pending: Vec<f32>,
    /// Real frames (per channel) passed to `write`.
    frames_in: u64,
    /// Frames encoded so far, including padding.
    frames_encoded: u64,
    /// Last encoded packet, held back so the final one can carry the exact end granule.
    held: Option<(Vec<u8>, u64)>,
    packets_in_page: u32,
    out: Vec<u8>,
}

fn opus_err(e: opus::Error) -> anyhow::Error {
    anyhow!("opus: {e}")
}

impl<W: Write> OpusWriter<W> {
    /// `kbps` is the bitrate per channel.
    pub fn new(w: W, channels: usize, kbps: u32) -> Result<Self> {
        if !(1..=2).contains(&channels) {
            bail!("Opus writer supports 1 or 2 channels");
        }
        let ch = if channels == 1 {
            opus::Channels::Mono
        } else {
            opus::Channels::Stereo
        };
        let mut enc = opus::Encoder::new(RATE, ch, opus::Application::Voip).map_err(opus_err)?;
        enc.set_bitrate(opus::Bitrate::Bits((kbps * 1000 * channels as u32) as i32))
            .map_err(opus_err)?;
        enc.set_complexity(10).map_err(opus_err)?;
        enc.set_signal(opus::Signal::Voice).map_err(opus_err)?;
        // DTX: during silence the encoder emits tiny packets (still one per frame,
        // so timing is unaffected) instead of spending ~15 kbps on nothing.
        enc.set_dtx(true).map_err(opus_err)?;
        let pre_skip = enc.get_lookahead().map_err(opus_err)?.max(0) as u64;
        let serial = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(1))
            ^ 0x4d52_6563; // "MRec"
        let mut pw = PacketWriter::new(Counting { inner: w, bytes: 0 });

        let mut head = Vec::with_capacity(19);
        head.extend_from_slice(b"OpusHead");
        head.push(1); // version
        head.push(channels as u8);
        head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
        head.extend_from_slice(&RATE.to_le_bytes()); // original input rate
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // mapping family 0 (mono/stereo)
        pw.write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)?;

        let vendor = format!("MultiRec {}", env!("CARGO_PKG_VERSION"));
        let mut tags = Vec::new();
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor.as_bytes());
        tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments
        pw.write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)?;

        Ok(Self {
            pw,
            enc,
            channels,
            serial,
            pre_skip,
            pending: Vec::with_capacity(FRAME * channels * 2),
            frames_in: 0,
            frames_encoded: 0,
            held: None,
            packets_in_page: 0,
            out: vec![0u8; MAX_PACKET],
        })
    }

    pub fn bytes_written(&self) -> u64 {
        self.pw.inner().bytes
    }

    /// Interleaved samples at 48 kHz.
    pub fn write(&mut self, interleaved: &[f32]) -> Result<()> {
        self.frames_in += (interleaved.len() / self.channels) as u64;
        self.pending.extend_from_slice(interleaved);
        let n = FRAME * self.channels;
        while self.pending.len() >= n {
            let frame: Vec<f32> = self.pending.drain(..n).collect();
            self.encode(&frame)?;
        }
        Ok(())
    }

    fn encode(&mut self, frame: &[f32]) -> Result<()> {
        let len = self
            .enc
            .encode_float(frame, &mut self.out)
            .map_err(opus_err)?;
        self.frames_encoded += FRAME as u64;
        let packet = self.out[..len].to_vec();
        if let Some((p, g)) = self.held.take() {
            self.packets_in_page += 1;
            let info = if self.packets_in_page >= PACKETS_PER_PAGE {
                self.packets_in_page = 0;
                PacketWriteEndInfo::EndPage
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            self.pw.write_packet(p, self.serial, info, g)?;
        }
        self.held = Some((packet, self.frames_encoded));
        Ok(())
    }

    /// Close the current page and flush to disk (crash safety).
    pub fn flush(&mut self) -> Result<()> {
        if let Some((p, g)) = self.held.take() {
            self.pw
                .write_packet(p, self.serial, PacketWriteEndInfo::EndPage, g)?;
            self.packets_in_page = 0;
        }
        self.pw.inner_mut().flush()?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        // Feed zeros until the encoder has emitted every real sample (its lookahead),
        // and make sure there is a final packet to carry the end-of-stream flag.
        let target = self.frames_in + self.pre_skip;
        let zeros = vec![0f32; FRAME * self.channels];
        let partial = self.pending.len() / self.channels;
        if partial > 0 {
            let mut f = std::mem::take(&mut self.pending);
            f.resize(FRAME * self.channels, 0.0);
            self.encode(&f)?;
        }
        while self.frames_encoded < target || self.held.is_none() {
            self.encode(&zeros)?;
        }
        let (p, _) = self.held.take().expect("final packet");
        self.pw
            .write_packet(p, self.serial, PacketWriteEndInfo::EndStream, target)?;
        let mut c = self.pw.into_inner();
        c.flush()?;
        Ok(c.inner)
    }
}

/// Decoded Opus file: interleaved f32 at 48 kHz.
#[cfg(test)]
pub struct Decoded {
    pub channels: usize,
    pub samples: Vec<f32>,
}

/// Reads channel count without decoding.
pub fn probe<R: Read + Seek>(r: R) -> Result<usize> {
    let mut pr = PacketReader::new(r);
    let head = pr.read_packet()?.context("empty Ogg file")?;
    parse_head(&head.data).map(|(c, _)| c)
}

fn parse_head(d: &[u8]) -> Result<(usize, u64)> {
    if d.len() < 19 || &d[..8] != b"OpusHead" {
        bail!("not an Opus file");
    }
    let channels = d[9] as usize;
    if !(1..=2).contains(&channels) || d[18] != 0 {
        bail!("unsupported Opus channel layout");
    }
    Ok((channels, u16::from_le_bytes([d[10], d[11]]) as u64))
}

/// Decode a whole file, honouring pre-skip and end trimming.
pub fn decode<R: Read + Seek>(r: R, mut f: impl FnMut(f32)) -> Result<usize> {
    let mut pr = PacketReader::new(r);
    let head = pr.read_packet()?.context("empty Ogg file")?;
    let (channels, pre_skip) = parse_head(&head.data)?;
    let _tags = pr.read_packet()?.context("missing OpusTags")?;
    let ch = if channels == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    };
    let mut dec = opus::Decoder::new(RATE, ch).map_err(opus_err)?;
    let mut buf = vec![0f32; 5760 * channels];
    let mut decoded: u64 = 0; // frames out of the decoder, incl. pre-skip
                              // A truncated (crashed) file may end mid-page; read errors there just end the stream.
    while let Ok(Some(pkt)) = pr.read_packet() {
        let n = match dec.decode_float(&pkt.data, &mut buf, false) {
            Ok(n) => n,
            Err(_) => continue,
        };
        // Frames allowed up to the end of this packet's page (end trimming).
        let limit = pkt.absgp_page();
        for i in 0..n {
            let pos = decoded + i as u64;
            if pos < pre_skip || pos >= limit.max(pre_skip) {
                continue;
            }
            for c in 0..channels {
                f(buf[i * channels + c]);
            }
        }
        decoded += n as u64;
    }
    Ok(channels)
}

#[cfg(test)]
pub fn decode_all<R: Read + Seek>(r: R) -> Result<Decoded> {
    let mut samples = Vec::new();
    let channels = decode(r, |s| samples.push(s))?;
    Ok(Decoded { channels, samples })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn voice(n: usize, ch: usize) -> Vec<f32> {
        // Harmonic-rich 150 Hz "voice" with slow amplitude modulation.
        (0..n)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                let env = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
                let ph = 2.0 * std::f32::consts::PI * 150.0 * t;
                let v = env * (0.3 * ph.sin() + 0.15 * (2.0 * ph).sin() + 0.08 * (3.0 * ph).sin());
                std::iter::repeat(v).take(ch)
            })
            .collect()
    }

    fn encode(data: &[f32], ch: usize, kbps: u32, chunk: usize) -> Vec<u8> {
        let mut w = OpusWriter::new(Cursor::new(Vec::new()), ch, kbps).unwrap();
        for c in data.chunks(chunk * ch) {
            w.write(c).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn exact_length_mono_and_stereo() {
        for (ch, frames) in [(1usize, 48_000 * 3 + 123), (2, 47_999)] {
            let data = voice(frames, ch);
            let bytes = encode(&data, ch, 32, 777);
            let d = decode_all(Cursor::new(&bytes)).unwrap();
            assert_eq!(d.channels, ch);
            assert_eq!(d.samples.len(), data.len(), "ch={ch}");
        }
    }

    #[test]
    fn timing_and_quality_preserved() {
        let n = 48_000 * 2;
        let mut data = voice(n, 1);
        // A click at exactly 1.5 s.
        let click = 72_000;
        for k in 0..48 {
            data[click + k] += if k % 2 == 0 { 0.9 } else { -0.9 };
        }
        let bytes = encode(&data, 1, 32, 480);
        let d = decode_all(Cursor::new(&bytes)).unwrap().samples;
        // Cross-correlate around the click to find the delay.
        let mut best = (0i64, f32::MIN);
        for lag in -96i64..=96 {
            let mut s = 0f32;
            for k in 0..200usize {
                let i = click + k;
                let j = (i as i64 + lag) as usize;
                s += data[i] * d[j];
            }
            if s > best.1 {
                best = (lag, s);
            }
        }
        assert!(best.0.abs() <= 48, "delay {} samples", best.0);
        // Opus (SILK) is perceptual, so waveform SNR is meaningless. Check that the
        // loudness envelope (10 ms RMS frames) and the overall level survive.
        let lag = best.0;
        let env = |x: &[f32], off: i64| -> Vec<f32> {
            (10..190)
                .map(|f| {
                    let s = (f * 480) as i64 + off;
                    let fr = &x[s as usize..s as usize + 480];
                    (fr.iter().map(|v| v * v).sum::<f32>() / 480.0).sqrt()
                })
                .collect()
        };
        let (a, b) = (env(&data, 0), env(&d, lag));
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let (ma, mb) = (mean(&a), mean(&b));
        let cov: f32 = a.iter().zip(&b).map(|(x, y)| (x - ma) * (y - mb)).sum();
        let va: f32 = a.iter().map(|x| (x - ma).powi(2)).sum();
        let vb: f32 = b.iter().map(|y| (y - mb).powi(2)).sum();
        let corr = cov / (va * vb).sqrt();
        assert!(corr > 0.95, "envelope correlation {corr:.3}");
        let level_db = 20.0 * (mb / ma).log10();
        assert!(level_db.abs() < 1.5, "level change {level_db:.2} dB");
        // ~32 kbps → about 4 KB/s.
        assert!(bytes.len() < 2 * 4_000 + 2_000, "size {}", bytes.len());
    }

    #[test]
    fn unfinished_file_is_readable() {
        let data = voice(48_000 * 2, 1);
        let mut w = OpusWriter::new(Cursor::new(Vec::new()), 1, 32).unwrap();
        w.write(&data).unwrap();
        w.flush().unwrap();
        // Simulate a crash: take the bytes without finish().
        let bytes = w.pw.into_inner().inner.into_inner();
        let d = decode_all(Cursor::new(&bytes)).unwrap();
        assert!(d.samples.len() > 48_000, "only {} samples", d.samples.len());
    }

    #[test]
    fn digital_silence_is_tiny() {
        let bytes = encode(&vec![0f32; 48_000 * 10], 1, 32, 960);
        // With DTX: ~10 s of silence in a few KB (vs ~18 KB without).
        assert!(bytes.len() < 5_000, "silence took {} bytes", bytes.len());
    }
}
