//! Lightweight prosody: loudness per word, pitch (YIN) per word, used to mark
//! emphasis, intonation contours and to reject words hallucinated over silence.

use super::Word;

pub fn rms_db(x: &[f32]) -> f32 {
    if x.is_empty() {
        return -120.0;
    }
    let ms = x.iter().map(|v| (v * v) as f64).sum::<f64>() / x.len() as f64;
    (10.0 * ms.max(1e-12).log10()) as f32
}

/// 10th percentile of 50 ms frame loudness ≈ background noise level.
pub fn noise_floor_db(audio: &[f32], sr: usize) -> f32 {
    let w = sr / 20;
    let mut v: Vec<f32> = audio
        .chunks(w)
        .filter(|c| c.len() == w)
        .map(rms_db)
        .collect();
    if v.is_empty() {
        return -120.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 10]
}

/// YIN fundamental-frequency estimate for one frame (needs `w + max_tau` samples).
pub fn yin(x: &[f32], sr: f32, fmin: f32, fmax: f32, w: usize) -> Option<f32> {
    let min_tau = (sr / fmax).floor().max(2.0) as usize;
    let max_tau = (sr / fmin).ceil() as usize;
    if x.len() < w + max_tau + 1 {
        return None;
    }
    let mut d = vec![0f32; max_tau + 2];
    for tau in 1..=max_tau + 1 {
        let mut s = 0f32;
        for j in 0..w {
            let e = x[j] - x[j + tau];
            s += e * e;
        }
        d[tau] = s;
    }
    // Cumulative mean normalised difference.
    let mut cm = vec![1f32; max_tau + 2];
    let mut run = 0f32;
    for tau in 1..=max_tau + 1 {
        run += d[tau];
        cm[tau] = if run > 0.0 {
            d[tau] * tau as f32 / run
        } else {
            1.0
        };
    }
    let thresh = 0.15;
    let mut tau = min_tau;
    while tau <= max_tau {
        if cm[tau] < thresh {
            while tau < max_tau && cm[tau + 1] < cm[tau] {
                tau += 1;
            }
            let (a, b, c) = (cm[tau - 1], cm[tau], cm[tau + 1]);
            let denom = a - 2.0 * b + c;
            let shift = if denom.abs() > 1e-9 {
                0.5 * (a - c) / denom
            } else {
                0.0
            };
            return Some(sr / (tau as f32 + shift.clamp(-1.0, 1.0)));
        }
        tau += 1;
    }
    None
}

/// Pitch samples (absolute s, Hz) every 10 ms between `start` and `end`.
pub fn pitch_track(
    audio: &[f32],
    sr: usize,
    start: f64,
    end: f64,
    floor_db: f32,
) -> Vec<(f32, f32)> {
    const FMIN: f32 = 60.0;
    const FMAX: f32 = 450.0;
    let w = sr * 25 / 1000;
    let max_tau = (sr as f32 / FMIN).ceil() as usize;
    let frame = w + max_tau + 1;
    let hop = sr / 100;
    let mut out = Vec::new();
    let mut i = (start * sr as f64) as usize;
    let stop = ((end * sr as f64) as usize).min(audio.len());
    while i < stop && i + frame <= audio.len() {
        let x = &audio[i..i + frame];
        if rms_db(&x[..w]) > floor_db + 10.0 {
            if let Some(f) = yin(x, sr as f32, FMIN, FMAX, w) {
                out.push(((i + w / 2) as f32 / sr as f32, f));
            }
        }
        i += hop;
    }
    out
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

pub fn semitones(f: f32) -> f32 {
    12.0 * (f / 100.0).log2()
}

/// Least-squares slope of semitone pitch over time (st/s) and total change across the span.
pub fn contour(points: &[(f32, f32)]) -> Option<(f32, f32)> {
    if points.len() < 4 {
        return None;
    }
    let n = points.len() as f32;
    let mt = points.iter().map(|p| p.0).sum::<f32>() / n;
    let ms = points.iter().map(|p| semitones(p.1)).sum::<f32>() / n;
    let mut num = 0f32;
    let mut den = 0f32;
    for &(t, f) in points {
        num += (t - mt) * (semitones(f) - ms);
        den += (t - mt) * (t - mt);
    }
    if den <= 1e-9 {
        return None;
    }
    let slope = num / den;
    let span = points.last().unwrap().0 - points[0].0;
    Some((slope, slope * span))
}

/// Measures loudness/pitch for each word and removes non-tag words whose audio
/// is at the noise floor (Whisper hallucinates on silence). Returns removed count.
pub fn annotate_words(words: &mut Vec<Word>, audio: &[f32], sr: usize, with_pitch: bool) -> usize {
    let floor = noise_floor_db(audio, sr);
    let gate = (floor + 4.0).max(-62.0);
    let before = words.len();
    words.retain_mut(|w| {
        let a = (((w.start - 0.03).max(0.0)) * sr as f64) as usize;
        let b = (((w.end + 0.03) * sr as f64) as usize).min(audio.len());
        if a >= b {
            return w.is_tag;
        }
        let db = rms_db(&audio[a..b]);
        // Gate on the loudest 30 ms frame near the word, tolerant of timing error.
        let ga = (((w.start - 0.15).max(0.0)) * sr as f64) as usize;
        let gb = (((w.end + 0.15) * sr as f64) as usize).min(audio.len());
        let peak_db = audio[ga..gb]
            .chunks(sr * 30 / 1000)
            .map(rms_db)
            .fold(-120.0f32, f32::max);
        if peak_db < gate && !w.is_tag {
            return false;
        }
        let mut pr = super::WordProsody {
            f0_hz: None,
            loud_db: db,
        };
        if with_pitch && !w.is_tag {
            w.pitch = pitch_track(audio, sr, w.start, w.end, floor);
            let mut f: Vec<f32> = w.pitch.iter().map(|p| p.1).collect();
            pr.f0_hz = median(&mut f);
        }
        w.prosody = Some(pr);
        true
    });
    before - words.len()
}

pub fn baselines(words: &[Word]) -> (Option<f32>, Option<f32>) {
    let mut f0: Vec<f32> = words
        .iter()
        .filter_map(|w| w.prosody.as_ref().and_then(|p| p.f0_hz))
        .collect();
    let mut db: Vec<f32> = words
        .iter()
        .filter(|w| !w.is_tag)
        .filter_map(|w| w.prosody.as_ref().map(|p| p.loud_db))
        .collect();
    (median(&mut f0), median(&mut db))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f0: impl Fn(f32) -> f32, secs: f32, sr: usize) -> Vec<f32> {
        let mut ph = 0f32;
        (0..(secs * sr as f32) as usize)
            .map(|i| {
                let t = i as f32 / sr as f32;
                ph += 2.0 * std::f32::consts::PI * f0(t) / sr as f32;
                // Harmonic-rich, voice-like signal.
                0.3 * ph.sin() + 0.15 * (2.0 * ph).sin() + 0.08 * (3.0 * ph).sin()
            })
            .collect()
    }

    #[test]
    fn yin_finds_pitch() {
        let sr = 16_000;
        for f in [90.0, 150.0, 220.0, 330.0] {
            let x = tone(|_| f, 0.2, sr);
            let est = yin(&x, sr as f32, 60.0, 450.0, 400).unwrap();
            assert!((est - f).abs() / f < 0.02, "{f}: {est}");
        }
    }

    #[test]
    fn rising_and_falling_contours() {
        let sr = 16_000;
        let up = tone(|t| 150.0 * 2f32.powf(t * 6.0 / 12.0), 0.8, sr); // +6 st/s
        let pts = pitch_track(&up, sr, 0.0, 0.8, -90.0);
        let (slope, _) = contour(&pts).unwrap();
        assert!((slope - 6.0).abs() < 1.0, "slope {slope}");
        let down = tone(|t| 200.0 * 2f32.powf(-t * 5.0 / 12.0), 0.8, sr);
        let (slope, _) = contour(&pitch_track(&down, sr, 0.0, 0.8, -90.0)).unwrap();
        assert!((slope + 5.0).abs() < 1.0, "slope {slope}");
    }

    #[test]
    fn hallucinations_over_silence_removed() {
        let sr = 16_000;
        let mut audio = vec![0f32; sr * 4];
        let speech = tone(|_| 150.0, 1.0, sr);
        audio[sr..2 * sr].copy_from_slice(&speech);
        let mut words = vec![
            Word::new("hello".into(), 1.1, 1.6, 0.9),
            Word::new("Thanks".into(), 3.0, 3.5, 0.8),
        ];
        let dropped = annotate_words(&mut words, &audio, sr, true);
        assert_eq!(dropped, 1);
        assert_eq!(words[0].text, "hello");
        let f0 = words[0].prosody.as_ref().unwrap().f0_hz.unwrap();
        assert!((f0 - 150.0).abs() < 4.0);
    }
}
