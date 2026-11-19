//! Groups each speaker's words into utterances, derives utterance-level prosody
//! (intonation contour, pace, loudness) and interleaves all speakers by time.

use super::prosody::{baselines, contour};
use super::{Track, Word};
use serde::Serialize;

/// Silence that ends an utterance.
const UTT_GAP_S: f64 = 1.2;
/// Silence inside an utterance worth marking.
const PAUSE_S: f64 = 0.5;
/// Silence between turns worth marking.
const TURN_PAUSE_S: f64 = 2.0;
const MAX_WORDS: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Contour {
    Rising,
    Falling,
    Level,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tag {
    Fast,
    Slow,
    Loud,
    Quiet,
}

#[derive(Clone, Debug, Serialize)]
pub struct Utterance {
    pub speaker: usize,
    pub label: String,
    pub start: f64,
    pub end: f64,
    pub words: Vec<Word>,
    pub contour: Contour,
    /// Pitch change over the final ~0.6 s (semitones).
    pub final_pitch_change_st: Option<f32>,
    pub pace_wpm: Option<f32>,
    pub loudness_rel_db: Option<f32>,
    pub tags: Vec<Tag>,
    /// Silence before this utterance (any speaker), if notable.
    pub gap_before: Option<f64>,
}

impl Utterance {
    #[allow(dead_code)]
    pub fn text_plain(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn ends_sentence(w: &Word) -> bool {
    w.text.ends_with(['.', '?', '!'])
}

fn group(si: usize, track: &Track) -> Vec<Utterance> {
    let mut out = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    for w in &track.words {
        if let Some(last) = cur.last() {
            let gap = w.start - last.end;
            let split = gap > UTT_GAP_S || (cur.len() >= MAX_WORDS && ends_sentence(last));
            if split {
                out.push(make(si, &track.label, std::mem::take(&mut cur)));
            }
        }
        cur.push(w.clone());
    }
    if !cur.is_empty() {
        out.push(make(si, &track.label, cur));
    }
    out
}

fn make(si: usize, label: &str, mut words: Vec<Word>) -> Utterance {
    for i in 0..words.len().saturating_sub(1) {
        let gap = words[i + 1].start - words[i].end;
        if gap >= PAUSE_S {
            words[i].pause_after = Some(gap);
        }
    }
    Utterance {
        speaker: si,
        label: label.to_string(),
        start: words.first().map_or(0.0, |w| w.start),
        end: words.last().map_or(0.0, |w| w.end),
        words,
        contour: Contour::Unknown,
        final_pitch_change_st: None,
        pace_wpm: None,
        loudness_rel_db: None,
        tags: Vec::new(),
        gap_before: None,
    }
}

fn analyse(u: &mut Utterance, base_f0: Option<f32>, base_db: Option<f32>) {
    let speech: Vec<&Word> = u.words.iter().filter(|w| !w.is_tag).collect();
    let dur = (u.end - u.start).max(0.1);
    if speech.len() >= 4 {
        u.pace_wpm = Some((speech.len() as f64 / dur * 60.0) as f32);
    }
    // Intonation at the end of the utterance.
    let tail_from = (u.end - 0.6) as f32;
    let pts: Vec<(f32, f32)> = speech
        .iter()
        .flat_map(|w| w.pitch.iter().copied())
        .filter(|p| p.0 >= tail_from)
        .collect();
    if let Some((_, change)) = contour(&pts) {
        u.final_pitch_change_st = Some(change);
        u.contour = if change > 2.0 {
            Contour::Rising
        } else if change < -3.0 {
            Contour::Falling
        } else {
            Contour::Level
        };
    }
    if let Some(bd) = base_db {
        let dbs: Vec<f32> = speech
            .iter()
            .filter_map(|w| w.prosody.as_ref().map(|p| p.loud_db))
            .collect();
        if !dbs.is_empty() {
            let rel = dbs.iter().sum::<f32>() / dbs.len() as f32 - bd;
            u.loudness_rel_db = Some(rel);
            if speech.len() >= 3 {
                if rel > 6.0 {
                    u.tags.push(Tag::Loud);
                } else if rel < -8.0 {
                    u.tags.push(Tag::Quiet);
                }
            }
        }
    }
    // Word emphasis: clearly louder or higher than the speaker's norm.
    for w in u.words.iter_mut().filter(|w| !w.is_tag) {
        let Some(p) = &w.prosody else { continue };
        let alnum = w.text.chars().filter(|c| c.is_alphanumeric()).count();
        if alnum < 3 {
            continue;
        }
        let loud = base_db.is_some_and(|b| p.loud_db > b + 5.0);
        let high = match (base_f0, p.f0_hz) {
            (Some(b), Some(f)) => f > b * 2f32.powf(4.0 / 12.0),
            _ => false,
        };
        w.emphasis = loud || high;
    }
}

pub fn build(tracks: &mut [Track], prosody: bool) -> Vec<Utterance> {
    let mut all = Vec::new();
    for (si, t) in tracks.iter_mut().enumerate() {
        let (f0, db) = baselines(&t.words);
        t.baseline_f0 = f0;
        t.baseline_db = db;
        let mut utts = group(si, t);
        if prosody {
            for u in &mut utts {
                analyse(u, f0, db);
            }
            let mut wpm: Vec<f32> = utts.iter().filter_map(|u| u.pace_wpm).collect();
            wpm.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let med = if wpm.len() >= 3 {
                Some(wpm[wpm.len() / 2])
            } else {
                None
            };
            t.median_wpm = med;
            if let Some(m) = med {
                for u in &mut utts {
                    if let Some(p) = u.pace_wpm {
                        if u.words.len() >= 6 {
                            if p > m * 1.3 {
                                u.tags.push(Tag::Fast);
                            } else if p < m * 0.7 {
                                u.tags.push(Tag::Slow);
                            }
                        }
                    }
                }
            }
        }
        all.extend(utts);
    }
    all.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    let mut last_end = f64::NEG_INFINITY;
    for u in &mut all {
        if last_end.is_finite() && u.start - last_end >= TURN_PAUSE_S {
            u.gap_before = Some(u.start - last_end);
        }
        last_end = last_end.max(u.end);
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::DeviceKind;

    fn tr(label: &str, words: &[(&str, f64)]) -> Track {
        Track {
            label: label.into(),
            kind: DeviceKind::Input,
            file: String::new(),
            words: words
                .iter()
                .map(|(t, s)| Word::new(t.to_string(), *s, s + 0.3, 1.0))
                .collect(),
            baseline_f0: None,
            baseline_db: None,
            median_wpm: None,
        }
    }

    #[test]
    fn interleaves_and_marks_pauses() {
        let mut t = vec![
            tr("Me", &[("Hello", 0.0), ("there.", 0.4), ("So", 10.0)]),
            tr("Others", &[("Hi!", 2.0), ("Yes", 2.4), ("indeed", 3.5)]),
        ];
        let c = build(&mut t, false);
        let order: Vec<_> = c
            .iter()
            .map(|u| (u.label.as_str(), u.text_plain()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("Me", "Hello there.".to_string()),
                ("Others", "Hi! Yes indeed".to_string()),
                ("Me", "So".to_string()),
            ]
        );
        assert!(c[2].gap_before.unwrap() > 6.0);
        assert!(c[1].words[1].pause_after.is_some());
    }
}
