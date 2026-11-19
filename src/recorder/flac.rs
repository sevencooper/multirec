//! Minimal streaming FLAC encoder (fixed predictors + partitioned Rice coding).
//!
//! Frames are self-contained and written as they fill, so a crash loses at most
//! the last ~85 ms; STREAMINFO's total-sample count is patched in on `finish`
//! (0 = "unknown" is valid for decoders if that never happens).

use std::io::{self, Seek, SeekFrom, Write};

pub const BLOCK: usize = 4096;
const MAX_PARTITION_ORDER: u32 = 6;

struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(16 * 1024),
            acc: 0,
            nbits: 0,
        }
    }

    fn put(&mut self, v: u64, n: u32) {
        if n == 0 {
            return;
        }
        if n > 32 {
            self.put(v >> 32, n - 32);
            self.put(v & 0xFFFF_FFFF, 32);
            return;
        }
        let v = v & ((1u64 << n) - 1);
        self.acc = (self.acc << n) | v;
        self.nbits += n;
        while self.nbits >= 8 {
            self.nbits -= 8;
            self.bytes.push((self.acc >> self.nbits) as u8);
        }
        self.acc &= (1u64 << self.nbits) - 1;
    }

    fn put_signed(&mut self, v: i64, n: u32) {
        self.put(v as u64, n);
    }

    fn unary(&mut self, mut q: u64) {
        while q >= 32 {
            self.put(0, 32);
            q -= 32;
        }
        self.put(1, q as u32 + 1);
    }

    fn rice(&mut self, u: u64, k: u32) {
        self.unary(u >> k);
        if k > 0 {
            self.put(u, k);
        }
    }

    fn align(&mut self) {
        if self.nbits > 0 {
            self.put(0, 8 - self.nbits);
        }
    }
}

fn crc8(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
    }
    c
}

fn crc16(data: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
        }
    }
    c
}

/// FLAC's UTF-8-like variable-length integer coding (frame numbers).
fn utf8_coded(v: u64) -> Vec<u8> {
    if v < 0x80 {
        return vec![v as u8];
    }
    let (n, lead): (usize, u8) = if v < 0x800 {
        (2, 0xC0)
    } else if v < 0x1_0000 {
        (3, 0xE0)
    } else if v < 0x20_0000 {
        (4, 0xF0)
    } else if v < 0x400_0000 {
        (5, 0xF8)
    } else if v < 0x8000_0000 {
        (6, 0xFC)
    } else {
        (7, 0xFE)
    };
    let mut out = vec![0u8; n];
    let mut v = v;
    for i in (1..n).rev() {
        out[i] = 0x80 | (v & 0x3F) as u8;
        v >>= 6;
    }
    out[0] = lead | v as u8;
    out
}

#[inline]
fn fold(r: i64) -> u64 {
    if r >= 0 {
        (r as u64) << 1
    } else {
        (((-r) as u64) << 1) - 1
    }
}

fn fixed_residual(x: &[i32], order: usize, out: &mut Vec<i64>) {
    out.clear();
    for i in order..x.len() {
        let s = |k: usize| x[i - k] as i64;
        let r = match order {
            0 => s(0),
            1 => s(0) - s(1),
            2 => s(0) - 2 * s(1) + s(2),
            3 => s(0) - 3 * s(1) + 3 * s(2) - s(3),
            _ => s(0) - 4 * s(1) + 6 * s(2) - 4 * s(3) + s(4),
        };
        out.push(r);
    }
}

fn rice_bits(u: &[u64], k: u32) -> u64 {
    let mut b = u.len() as u64 * (k as u64 + 1);
    for &v in u {
        b += v >> k;
    }
    b
}

fn best_k(u: &[u64]) -> (u32, u64) {
    if u.is_empty() {
        return (0, 0);
    }
    let mean = u.iter().sum::<u64>() / u.len() as u64;
    let guess = if mean == 0 {
        0
    } else {
        (63 - mean.leading_zeros()).min(14)
    };
    let mut best = (0u32, u64::MAX);
    for k in guess.saturating_sub(1)..=(guess + 1).min(14) {
        let b = rice_bits(u, k);
        if b < best.1 {
            best = (k, b);
        }
    }
    best
}

/// Returns (bits, partition order, rice params) for the best partitioning.
fn plan_residual(u: &[u64], n: usize, order: usize) -> (u64, u32, Vec<u32>) {
    let mut best: (u64, u32, Vec<u32>) = (u64::MAX, 0, Vec::new());
    for p in 0..=MAX_PARTITION_ORDER {
        let parts = 1usize << p;
        if !n.is_multiple_of(parts) || n / parts <= order {
            break;
        }
        let plen = n / parts;
        let mut bits = 2 + 4u64;
        let mut ks = Vec::with_capacity(parts);
        let mut start = 0usize;
        for i in 0..parts {
            let len = if i == 0 { plen - order } else { plen };
            let (k, b) = best_k(&u[start..start + len]);
            bits += 4 + b;
            ks.push(k);
            start += len;
        }
        if bits < best.0 {
            best = (bits, p, ks);
        }
    }
    best
}

fn encode_subframe(b: &mut BitWriter, x: &[i32], bps: u32, res: &mut Vec<i64>, u: &mut Vec<u64>) {
    let n = x.len();
    if x.iter().all(|&v| v == x[0]) {
        b.put(0, 1);
        b.put(0, 6);
        b.put(0, 1);
        b.put_signed(x[0] as i64, bps);
        return;
    }
    let mut best: Option<(u64, usize, u32, Vec<u32>)> = None;
    for order in 0..=4usize.min(n.saturating_sub(1)) {
        fixed_residual(x, order, res);
        u.clear();
        u.extend(res.iter().map(|&r| fold(r)));
        let (bits, p, ks) = plan_residual(u, n, order);
        if bits == u64::MAX {
            continue;
        }
        let total = bits + order as u64 * bps as u64;
        if best.as_ref().is_none_or(|bb| total < bb.0) {
            best = Some((total, order, p, ks));
        }
    }
    let verbatim = n as u64 * bps as u64;
    match best {
        Some((total, order, p, ks)) if total < verbatim => {
            b.put(0, 1);
            b.put(0b001000 | order as u64, 6);
            b.put(0, 1);
            for &s in &x[..order] {
                b.put_signed(s as i64, bps);
            }
            fixed_residual(x, order, res);
            b.put(0, 2);
            b.put(p as u64, 4);
            let parts = 1usize << p;
            let plen = n / parts;
            let mut start = 0usize;
            for (i, &k) in ks.iter().enumerate() {
                let len = if i == 0 { plen - order } else { plen };
                b.put(k as u64, 4);
                for &r in &res[start..start + len] {
                    b.rice(fold(r), k);
                }
                start += len;
            }
        }
        _ => {
            b.put(0, 1);
            b.put(1, 6);
            b.put(0, 1);
            for &s in x {
                b.put_signed(s as i64, bps);
            }
        }
    }
}

pub struct FlacEncoder<W: Write + Seek> {
    w: W,
    channels: usize,
    bps: u32,
    rate: u32,
    pending: Vec<Vec<i32>>,
    frame_no: u64,
    total: u64,
    bytes: u64,
    first_block: Option<usize>,
    res: Vec<i64>,
    u: Vec<u64>,
}

impl<W: Write + Seek> FlacEncoder<W> {
    pub fn new(mut w: W, channels: usize, bps: u32, rate: u32) -> io::Result<Self> {
        assert!((1..=8).contains(&channels) && (4..=24).contains(&bps));
        w.write_all(b"fLaC")?;
        let si = streaminfo(BLOCK as u16, rate, channels, bps, 0);
        w.write_all(&si)?;
        Ok(Self {
            w,
            channels,
            bps,
            rate,
            pending: vec![Vec::with_capacity(BLOCK * 2); channels],
            frame_no: 0,
            total: 0,
            bytes: 4 + si.len() as u64,
            first_block: None,
            res: Vec::with_capacity(BLOCK),
            u: Vec::with_capacity(BLOCK),
        })
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Interleaved integer samples, already scaled to `bps`.
    pub fn write(&mut self, interleaved: &[i32]) -> io::Result<()> {
        for f in interleaved.chunks_exact(self.channels) {
            for (c, &s) in f.iter().enumerate() {
                self.pending[c].push(s);
            }
        }
        while self.pending[0].len() >= BLOCK {
            self.encode_frame(BLOCK)?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }

    fn encode_frame(&mut self, n: usize) -> io::Result<()> {
        let mut b = BitWriter::new();
        b.put(0b11_1111_1111_1110, 14);
        b.put(0, 1);
        b.put(0, 1); // fixed block size
        b.put(0b0111, 4); // block size: 16-bit (n-1) follows
        b.put(0b0000, 4); // sample rate: from STREAMINFO
        b.put(self.channels as u64 - 1, 4);
        let ss = match self.bps {
            8 => 0b001,
            12 => 0b010,
            16 => 0b100,
            20 => 0b101,
            24 => 0b110,
            _ => 0b000,
        };
        b.put(ss, 3);
        b.put(0, 1);
        for byte in utf8_coded(self.frame_no) {
            b.put(byte as u64, 8);
        }
        b.put(n as u64 - 1, 16);
        let c8 = crc8(&b.bytes);
        b.put(c8 as u64, 8);
        for c in 0..self.channels {
            encode_subframe(
                &mut b,
                &self.pending[c][..n],
                self.bps,
                &mut self.res,
                &mut self.u,
            );
        }
        b.align();
        let c16 = crc16(&b.bytes);
        b.put(c16 as u64, 16);
        self.w.write_all(&b.bytes)?;
        self.bytes += b.bytes.len() as u64;
        for p in &mut self.pending {
            p.drain(..n);
        }
        self.frame_no += 1;
        self.total += n as u64;
        self.first_block.get_or_insert(n);
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<W> {
        let rest = self.pending[0].len();
        if rest > 0 {
            self.encode_frame(rest)?;
        }
        let block = if self.frame_no == 1 {
            self.first_block.unwrap_or(BLOCK)
        } else {
            BLOCK
        };
        let si = streaminfo(
            block.max(16) as u16,
            self.rate,
            self.channels,
            self.bps,
            self.total,
        );
        self.w.seek(SeekFrom::Start(4))?;
        self.w.write_all(&si)?;
        self.w.seek(SeekFrom::End(0))?;
        self.w.flush()?;
        Ok(self.w)
    }
}

fn streaminfo(block: u16, rate: u32, channels: usize, bps: u32, total: u64) -> Vec<u8> {
    let mut b = BitWriter::new();
    b.put(1, 1); // last metadata block
    b.put(0, 7); // STREAMINFO
    b.put(34, 24);
    b.put(block as u64, 16);
    b.put(block as u64, 16);
    b.put(0, 24); // min frame size unknown
    b.put(0, 24); // max frame size unknown
    b.put(rate as u64, 20);
    b.put(channels as u64 - 1, 3);
    b.put(bps as u64 - 1, 5);
    b.put(total, 36);
    for _ in 0..16 {
        b.put(0, 8); // MD5 unknown
    }
    b.bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn roundtrip(channels: usize, bps: u32, frames: usize) {
        let max = (1i64 << (bps - 1)) - 1;
        let mut seed = 7u64;
        let mut data = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            for c in 0..channels {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let noise = ((seed >> 40) as i64 % 200) - 100;
                let tone = ((i as f64 * 0.01 * (c + 1) as f64).sin() * max as f64 * 0.7) as i64;
                // Include a stretch of digital silence and a full-scale square burst.
                let v = if (5000..7000).contains(&i) {
                    0
                } else if (9000..9100).contains(&i) {
                    if i % 2 == 0 {
                        max
                    } else {
                        -max - 1
                    }
                } else {
                    (tone + noise).clamp(-max - 1, max)
                };
                data.push(v as i32);
            }
        }
        let mut enc = FlacEncoder::new(Cursor::new(Vec::new()), channels, bps, 48_000).unwrap();
        for chunk in data.chunks(1234 * channels) {
            enc.write(chunk).unwrap();
        }
        let bytes = enc.finish().unwrap().into_inner();
        let mut r = claxon::FlacReader::new(Cursor::new(&bytes)).unwrap();
        let info = r.streaminfo();
        assert_eq!(info.samples, Some(frames as u64));
        assert_eq!(info.channels as usize, channels);
        assert_eq!(info.bits_per_sample, bps);
        let decoded: Vec<i32> = r.samples().map(|s| s.unwrap()).collect();
        assert_eq!(decoded.len(), data.len());
        assert!(decoded == data, "sample mismatch");
        assert!(
            bytes.len() < data.len() * (bps as usize / 8),
            "no compression"
        );
    }

    #[test]
    fn mono16() {
        roundtrip(1, 16, 20_000);
    }
    #[test]
    fn stereo24() {
        roundtrip(2, 24, 17_777);
    }
    #[test]
    fn tiny_stream() {
        roundtrip(1, 16, 100);
    }
    #[test]
    fn utf8_lengths() {
        assert_eq!(utf8_coded(0x7F), vec![0x7F]);
        assert_eq!(utf8_coded(0x80), vec![0xC2, 0x80]);
        assert_eq!(utf8_coded(0x10000).len(), 4);
    }
}
