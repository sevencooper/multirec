//! Band-limited (windowed-sinc) resampler with an adjustable ratio, used both
//! for sample-rate conversion and for fine clock-drift correction.
//!
//! Output sample `j` is the band-limited interpolation of the input at fractional
//! position `pos_j`; `pos` advances by `step = in_rate / out_rate * (1 - correction)`.
//! The filter is zero-phase, so there is no added latency to account for.

const HALF: usize = 16;
const TAPS: usize = 2 * HALF;
const PHASES: usize = 256;

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Blackman-Harris window over t ∈ [-1, 1].
fn window(t: f64) -> f64 {
    if t.abs() >= 1.0 {
        return 0.0;
    }
    let n = (t + 1.0) * 0.5;
    let tau = 2.0 * std::f64::consts::PI;
    0.35875 - 0.48829 * (tau * n).cos() + 0.14128 * (2.0 * tau * n).cos()
        - 0.01168 * (3.0 * tau * n).cos()
}

fn build_table(cutoff: f64) -> Vec<f32> {
    let mut t = vec![0f32; (PHASES + 1) * TAPS];
    for p in 0..=PHASES {
        let frac = p as f64 / PHASES as f64;
        for kk in 0..TAPS {
            let k = kk as f64 - (HALF as f64 - 1.0);
            let x = k - frac;
            t[p * TAPS + kk] = (cutoff * sinc(cutoff * x) * window(x / HALF as f64)) as f32;
        }
    }
    t
}

pub struct Resampler {
    table: Vec<f32>,
    buf: Vec<f32>,
    pos: f64,
    base_step: f64,
    step: f64,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let base_step = in_rate as f64 / out_rate as f64;
        let cutoff = if in_rate > out_rate {
            out_rate as f64 / in_rate as f64 * 0.97
        } else {
            1.0
        };
        Self {
            table: build_table(cutoff),
            buf: vec![0.0; HALF - 1],
            pos: (HALF - 1) as f64,
            base_step,
            step: base_step,
        }
    }

    /// Relative speed correction: positive values produce *more* output per input.
    pub fn set_correction(&mut self, c: f64) {
        self.step = self.base_step * (1.0 - c);
    }

    pub fn step(&self) -> f64 {
        self.step
    }

    pub fn push(&mut self, x: &[f32]) {
        self.buf.extend_from_slice(x);
    }

    pub fn push_sample(&mut self, x: f32) {
        self.buf.push(x);
    }

    pub fn push_zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0.0);
    }

    /// Input frames buffered beyond the next output position.
    pub fn ahead(&self) -> f64 {
        self.buf.len() as f64 - self.pos
    }

    /// Produce as many output samples as the buffered input allows.
    pub fn process(&mut self, out: &mut Vec<f32>) {
        loop {
            let i = self.pos as usize;
            if i + HALF >= self.buf.len() {
                break;
            }
            let frac = self.pos - i as f64;
            let ph = frac * PHASES as f64;
            let p0 = (ph as usize).min(PHASES);
            let t = (ph - p0 as f64) as f32;
            let p1 = (p0 + 1).min(PHASES);
            let r0 = &self.table[p0 * TAPS..p0 * TAPS + TAPS];
            let r1 = &self.table[p1 * TAPS..p1 * TAPS + TAPS];
            let base = i + 1 - HALF;
            let s = &self.buf[base..base + TAPS];
            let mut acc = 0f32;
            for j in 0..TAPS {
                acc += s[j] * (r0[j] + (r1[j] - r0[j]) * t);
            }
            out.push(acc);
            self.pos += self.step;
        }
        let keep_from = (self.pos as usize).saturating_sub(HALF - 1);
        if keep_from > 16_384 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }

    /// Push enough trailing zeros to emit the remaining tail.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        self.push_zeros(HALF + 1);
        self.process(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_is_exact() {
        let mut r = Resampler::new(48_000, 48_000);
        let x: Vec<f32> = (0..1000)
            .map(|i| ((i * 7919) % 200) as f32 / 100.0 - 1.0)
            .collect();
        r.push(&x);
        let mut out = Vec::new();
        r.flush(&mut out);
        assert!(out.len() >= x.len());
        for (a, b) in x.iter().zip(&out) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn sine_survives_conversion() {
        // 1 kHz at 44.1 kHz → 48 kHz: correct length, amplitude and frequency.
        let n_in = 44_100;
        let x: Vec<f32> = (0..n_in)
            .map(|i| (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin() as f32 * 0.5)
            .collect();
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        for c in x.chunks(441) {
            r.push(c);
            r.process(&mut out);
        }
        r.flush(&mut out);
        assert!((out.len() as i64 - 48_000).abs() < 40, "len {}", out.len());
        for (j, &y) in out.iter().enumerate().skip(100).take(47_000) {
            let expect =
                (2.0 * std::f64::consts::PI * 1000.0 * j as f64 / 48_000.0).sin() as f32 * 0.5;
            assert!((y - expect).abs() < 2e-3, "at {j}: {y} vs {expect}");
        }
    }

    #[test]
    fn downsampling_rejects_alias() {
        // 12 kHz tone must vanish when going 48 kHz → 16 kHz (Nyquist 8 kHz).
        let x: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f64::consts::PI * 12_000.0 * i as f64 / 48_000.0).sin() as f32)
            .collect();
        let mut r = Resampler::new(48_000, 16_000);
        r.push(&x);
        let mut out = Vec::new();
        r.flush(&mut out);
        let rms = (out[200..15_000].iter().map(|v| v * v).sum::<f32>() / 14_800.0).sqrt();
        assert!(rms < 0.01, "alias rms {rms}");
    }
}
