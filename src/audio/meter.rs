use std::collections::VecDeque;
use std::time::Instant;

/// Number of waveform buckets kept (20 ms each → 5 s of history).
pub const WAVE_BUCKETS: usize = 250;
const PEAK_FALL_DB_PER_S: f32 = 24.0;
const HOLD_S: f32 = 1.5;

#[derive(Clone, Debug)]
pub struct MeterState {
    /// Decaying peak (linear, 0..1+).
    pub peak: f32,
    /// Smoothed RMS of the mono downmix (linear).
    pub rms: f32,
    /// Peak-hold value (linear).
    pub hold: f32,
    hold_left_s: f32,
    pub clip_count: u64,
    pub last_clip: Option<Instant>,
    /// (min, max) of the mono signal per 20 ms bucket.
    pub wave: VecDeque<(f32, f32)>,
    b_min: f32,
    b_max: f32,
    b_n: u32,
    pub last_update: Option<Instant>,
}

impl Default for MeterState {
    fn default() -> Self {
        Self {
            peak: 0.0,
            rms: 0.0,
            hold: 0.0,
            hold_left_s: 0.0,
            clip_count: 0,
            last_clip: None,
            wave: VecDeque::with_capacity(WAVE_BUCKETS + 1),
            b_min: 0.0,
            b_max: 0.0,
            b_n: 0,
            last_update: None,
        }
    }
}

impl MeterState {
    pub fn update(&mut self, data: &[f32], channels: usize, rate: u32) {
        let ch = channels.max(1);
        let frames = data.len() / ch;
        if frames == 0 || rate == 0 {
            return;
        }
        let dt = frames as f32 / rate as f32;
        let bucket_len = (rate / 50).max(1);
        let mut pk = 0f32;
        let mut sumsq = 0f64;
        let mut clipped = false;
        for f in data.chunks_exact(ch) {
            let mut sum = 0f32;
            for &s in f {
                let a = s.abs();
                if a > pk {
                    pk = a;
                }
                if a >= 0.999 {
                    clipped = true;
                }
                sum += s;
            }
            let m = sum / ch as f32;
            sumsq += (m * m) as f64;
            if self.b_n == 0 {
                self.b_min = m;
                self.b_max = m;
            } else {
                self.b_min = self.b_min.min(m);
                self.b_max = self.b_max.max(m);
            }
            self.b_n += 1;
            if self.b_n >= bucket_len {
                self.wave.push_back((self.b_min, self.b_max));
                while self.wave.len() > WAVE_BUCKETS {
                    self.wave.pop_front();
                }
                self.b_n = 0;
            }
        }
        let rms_pkt = (sumsq / frames as f64).sqrt() as f32;
        let fall = 10f32.powf(-PEAK_FALL_DB_PER_S * dt / 20.0);
        self.peak = pk.max(self.peak * fall);
        let a = 1.0 - (-dt / 0.3).exp();
        self.rms += a * (rms_pkt - self.rms);
        if pk >= self.hold {
            self.hold = pk;
            self.hold_left_s = HOLD_S;
        } else {
            self.hold_left_s -= dt;
            if self.hold_left_s <= 0.0 {
                self.hold = self.peak;
            }
        }
        if clipped {
            self.clip_count += 1;
            self.last_clip = Some(Instant::now());
        }
        self.last_update = Some(Instant::now());
    }
}
