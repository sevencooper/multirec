//! Maps a device's packet stream onto the shared recording timeline.
//!
//! Every packet carries the master-clock (QPC) time of its first frame. From the
//! recording start `t0` we know which *output* sample that frame belongs at. The
//! aligner compares that with where the frame would actually land and
//! - inserts silence for large positive errors (loopback silence, glitches),
//! - drops input for large negative errors (data from before `t0`, overlaps),
//! - otherwise nudges the resampler ratio with a PI controller, so device clock
//!   drift is absorbed smoothly and every file stays locked to the master clock.

use crate::clock::HNS_PER_SEC;
use crate::dsp::Resampler;

const GAP_THRESH_S: f64 = 0.030;
const KP: f64 = 0.3;
const KI: f64 = KP * KP / 4.0;
const MAX_CORR: f64 = 2e-3;
const SILENCE_CHUNK: usize = 1 << 16;

#[derive(Clone, Debug, Default)]
pub struct AlignStats {
    pub silence_s: f64,
    pub dropped_s: f64,
    /// Measured clock deviation of the device vs. the master clock.
    pub drift_ppm: f64,
    /// Smoothed alignment error in milliseconds (positive = device behind).
    pub err_ms: f64,
}

pub struct Aligner {
    out_rate: u32,
    out_ch: usize,
    in_rate: u32,
    rs: Vec<Resampler>,
    tmp: Vec<Vec<f32>>,
    t0: i64,
    produced: u64,
    /// Set once the first packet has been placed exactly.
    locked: bool,
    corr: f64,
    integ: f64,
    pub stats: AlignStats,
}

impl Aligner {
    pub fn new(out_rate: u32, out_channels: usize, t0_hns: i64) -> Self {
        let out_ch = out_channels.clamp(1, 2);
        Self {
            out_rate,
            out_ch,
            in_rate: 0,
            rs: Vec::new(),
            tmp: vec![Vec::new(); out_ch],
            t0: t0_hns,
            produced: 0,
            locked: false,
            corr: 0.0,
            integ: 0.0,
            stats: AlignStats::default(),
        }
    }

    pub fn channels(&self) -> usize {
        self.out_ch
    }

    #[allow(dead_code)]
    pub fn produced(&self) -> u64 {
        self.produced
    }

    pub fn shift_t0(&mut self, delta_hns: i64) {
        self.t0 += delta_hns;
    }

    fn ensure_rate(&mut self, rate: u32, out: &mut Vec<f32>) {
        if rate == self.in_rate && !self.rs.is_empty() {
            return;
        }
        if !self.rs.is_empty() {
            self.flush(out);
        }
        self.in_rate = rate;
        self.rs = (0..self.out_ch)
            .map(|_| Resampler::new(rate, self.out_rate))
            .collect();
        for r in &mut self.rs {
            r.set_correction(self.corr);
        }
    }

    /// Output-sample index at which the next pushed input frame will land.
    fn landing(&self) -> f64 {
        let r = &self.rs[0];
        self.produced as f64 + r.ahead() / r.step()
    }

    fn run(&mut self, out: &mut Vec<f32>) {
        for (r, t) in self.rs.iter_mut().zip(self.tmp.iter_mut()) {
            t.clear();
            r.process(t);
        }
        let n = self.tmp.iter().map(|t| t.len()).min().unwrap_or(0);
        out.reserve(n * self.out_ch);
        for i in 0..n {
            for t in &self.tmp {
                out.push(t[i]);
            }
        }
        self.produced += n as u64;
    }

    fn insert_silence(&mut self, n_out: f64, out: &mut Vec<f32>) {
        let mut n_in = (n_out * self.rs[0].step()).round().max(0.0) as usize;
        self.stats.silence_s += n_in as f64 / self.in_rate as f64;
        while n_in > 0 {
            let c = n_in.min(SILENCE_CHUNK);
            for r in &mut self.rs {
                r.push_zeros(c);
            }
            self.run(out);
            n_in -= c;
        }
    }

    fn control(&mut self, err_s: f64, dt: f64) {
        let lim = MAX_CORR / KI;
        self.integ = (self.integ + err_s * dt).clamp(-lim, lim);
        self.corr = (KP * err_s + KI * self.integ).clamp(-MAX_CORR, MAX_CORR);
        for r in &mut self.rs {
            r.set_correction(self.corr);
        }
        // A fast device clock needs negative correction; report the device's deviation.
        self.stats.drift_ppm += 0.02 * (-self.corr * 1e6 - self.stats.drift_ppm);
        self.stats.err_ms += 0.1 * (err_s * 1000.0 - self.stats.err_ms);
    }

    /// Feed one packet; aligned output frames (interleaved, `channels()` wide) are appended to `out`.
    pub fn push(
        &mut self,
        rate: u32,
        channels: u16,
        data: &[f32],
        qpc_hns: Option<i64>,
        out: &mut Vec<f32>,
    ) {
        let ch = channels.max(1) as usize;
        let frames = data.len() / ch;
        if frames == 0 || rate == 0 {
            return;
        }
        self.ensure_rate(rate, out);
        let mut skip = 0usize;
        if let Some(q) = qpc_hns {
            let expected = (q - self.t0) as f64 / HNS_PER_SEC as f64 * self.out_rate as f64;
            let err = expected - self.landing();
            // Before lock, place the very first frames exactly; afterwards only
            // treat large errors as gaps/overlaps and let the PI loop handle the rest.
            let thr = if self.locked {
                GAP_THRESH_S * self.out_rate as f64
            } else {
                0.5
            };
            if err > thr {
                self.insert_silence(err, out);
            } else if err < -thr {
                let d = ((-err) * self.rs[0].step()).round() as usize;
                skip = d.min(frames);
                self.stats.dropped_s += skip as f64 / rate as f64;
                if skip == frames {
                    return;
                }
                self.locked = true;
            } else {
                self.control(err / self.out_rate as f64, frames as f64 / rate as f64);
            }
        }
        if qpc_hns.is_some() {
            self.locked = true;
        }
        let frames_data = &data[skip * ch..frames * ch];
        match self.out_ch {
            1 => {
                let inv = 1.0 / ch as f32;
                let r = &mut self.rs[0];
                for f in frames_data.chunks_exact(ch) {
                    r.push_sample(f.iter().sum::<f32>() * inv);
                }
            }
            _ => {
                let (a, b) = self.rs.split_at_mut(1);
                for f in frames_data.chunks_exact(ch) {
                    a[0].push_sample(f[0]);
                    b[0].push_sample(if ch > 1 { f[1] } else { f[0] });
                }
            }
        }
        self.run(out);
    }

    /// Fill with silence until (approximately) `target` output frames exist.
    /// Used for streams that have gone quiet (no packets) and at stop.
    pub fn pad_to(&mut self, target: u64, out: &mut Vec<f32>) {
        if self.rs.is_empty() {
            self.ensure_rate(self.out_rate, out);
        }
        let l = self.landing();
        if target as f64 > l + 1.0 {
            self.insert_silence(target as f64 - l, out);
        }
    }

    /// Drain the resampler tail.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if self.rs.is_empty() {
            return;
        }
        for (r, t) in self.rs.iter_mut().zip(self.tmp.iter_mut()) {
            t.clear();
            r.flush(t);
        }
        let n = self.tmp.iter().map(|t| t.len()).min().unwrap_or(0);
        for i in 0..n {
            for t in &self.tmp {
                out.push(t[i]);
            }
        }
        self.produced += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: u32 = 48_000;

    /// Simulates a device whose real clock runs at `nominal * (1 + ppm)`, sends
    /// `pkt`-frame packets with jittered timestamps, contains one impulse at
    /// `impulse_t` seconds of true time, and optionally goes silent (no packets)
    /// during `gap`. Returns (output, aligner).
    fn simulate(
        nominal: u32,
        ppm: f64,
        total_s: f64,
        impulse_t: f64,
        gap: Option<(f64, f64)>,
        start_offset_s: f64,
    ) -> (Vec<f32>, Aligner) {
        let t0 = 1_000 * HNS_PER_SEC;
        let mut al = Aligner::new(OUT, 1, t0);
        let true_rate = nominal as f64 * (1.0 + ppm * 1e-6);
        let pkt = (nominal / 100) as usize;
        let impulse_idx = (impulse_t * true_rate).round() as u64;
        let mut out = Vec::new();
        let mut n: u64 = 0;
        let mut seed: u64 = 12345;
        loop {
            let t = n as f64 / true_rate + start_offset_s;
            if t > total_s {
                break;
            }
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let jitter = ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.001; // ±0.5 ms
            let mut data = vec![0f32; pkt];
            if impulse_idx >= n && impulse_idx < n + pkt as u64 {
                data[(impulse_idx - n) as usize] = 1.0;
            }
            let in_gap = gap.map_or(false, |(a, b)| t >= a && t < b);
            if !in_gap {
                let q = t0 + ((t + jitter) * HNS_PER_SEC as f64) as i64;
                al.push(nominal, 1, &data, Some(q), &mut out);
            }
            n += pkt as u64;
        }
        al.flush(&mut out);
        (out, al)
    }

    fn argmax(x: &[f32]) -> usize {
        x.iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0
    }

    #[test]
    fn locks_drifting_clock() {
        // +500 ppm is far worse than real hardware (typically < 100 ppm).
        let (out, al) = simulate(48_000, 500.0, 130.0, 120.0, None, 0.0);
        let pos = argmax(&out) as i64;
        let expect = 120 * OUT as i64;
        assert!(
            (pos - expect).abs() <= 48,
            "impulse at {pos}, expected {expect}"
        );
        assert!(
            (al.stats.drift_ppm - 500.0).abs() < 60.0,
            "drift {}",
            al.stats.drift_ppm
        );
        assert!((out.len() as i64 - 130 * OUT as i64).abs() < 480);
    }

    #[test]
    fn negative_drift() {
        let (out, _) = simulate(48_000, -300.0, 100.0, 90.0, None, 0.0);
        let pos = argmax(&out) as i64;
        assert!((pos - 90 * OUT as i64).abs() <= 48, "impulse at {pos}");
    }

    #[test]
    fn fills_gaps_with_silence() {
        let (out, al) = simulate(48_000, 0.0, 60.0, 50.0, Some((20.0, 30.0)), 0.0);
        let pos = argmax(&out) as i64;
        assert!((pos - 50 * OUT as i64).abs() <= 48, "impulse at {pos}");
        assert!(
            (al.stats.silence_s - 10.0).abs() < 0.05,
            "silence {}",
            al.stats.silence_s
        );
    }

    #[test]
    fn converts_rate() {
        let (out, _) = simulate(44_100, 50.0, 40.0, 30.0, None, 0.0);
        let pos = argmax(&out) as i64;
        assert!((pos - 30 * OUT as i64).abs() <= 48, "impulse at {pos}");
    }

    #[test]
    fn late_start_is_padded_and_early_data_dropped() {
        // Device starts delivering 0.5 s after t0 → leading silence.
        let (out, al) = simulate(48_000, 0.0, 10.0, 5.0, None, 0.5);
        assert!((argmax(&out) as i64 - 5 * OUT as i64 - OUT as i64 / 2).abs() <= 48);
        assert!((al.stats.silence_s - 0.5).abs() < 0.02);
        // Device buffer contains data from before t0 → dropped.
        let (out, al) = simulate(48_000, 0.0, 10.0, 5.0, None, -0.3);
        assert!((argmax(&out) as i64 - (4.7 * OUT as f64) as i64).abs() <= 48);
        assert!((al.stats.dropped_s - 0.3).abs() < 0.02);
    }

    #[test]
    fn pad_to_reaches_target() {
        let mut al = Aligner::new(OUT, 2, 0);
        let mut out = Vec::new();
        al.pad_to(96_000, &mut out);
        al.flush(&mut out);
        assert!((al.produced() as i64 - 96_000).abs() < 40);
        assert_eq!(out.len() as u64, al.produced() * 2);
    }
}
