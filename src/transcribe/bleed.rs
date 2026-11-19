//! When someone uses speakers instead of headphones, the microphone also picks
//! up the remote side. Those words then appear twice: once in the system-audio
//! track (clean) and once in the mic track (bled). Remove the mic copies.

use super::Track;
use crate::audio::DeviceKind;

pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

const WINDOW_S: f64 = 1.0;

pub fn suppress(tracks: &mut [Track]) -> usize {
    let mut remote: Vec<(f64, String)> = tracks
        .iter()
        .filter(|t| t.kind == DeviceKind::Loopback)
        .flat_map(|t| t.words.iter())
        .filter(|w| !w.is_tag)
        .map(|w| ((w.start + w.end) * 0.5, normalize(&w.text)))
        .filter(|(_, n)| !n.is_empty())
        .collect();
    if remote.is_empty() {
        return 0;
    }
    remote.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let mut removed = 0;
    for t in tracks.iter_mut().filter(|t| t.kind == DeviceKind::Input) {
        let before = t.words.len();
        t.words.retain(|w| {
            if w.is_tag {
                return true;
            }
            let n = normalize(&w.text);
            if n.is_empty() {
                return true;
            }
            let mid = (w.start + w.end) * 0.5;
            let lo = remote.partition_point(|r| r.0 < mid - WINDOW_S);
            !remote[lo..]
                .iter()
                .take_while(|r| r.0 <= mid + WINDOW_S)
                .any(|r| r.1 == n)
        });
        removed += before - t.words.len();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::Word;

    fn track(kind: DeviceKind, words: &[(&str, f64)]) -> Track {
        Track {
            label: "x".into(),
            kind,
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
    fn removes_only_echoed_words() {
        let mut t = vec![
            track(
                DeviceKind::Input,
                &[("Hi", 0.0), ("how", 5.05), ("are", 5.4), ("great!", 9.0)],
            ),
            track(
                DeviceKind::Loopback,
                &[("How", 5.0), ("are", 5.35), ("you?", 5.7)],
            ),
        ];
        assert_eq!(suppress(&mut t), 2);
        let left: Vec<_> = t[0].words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(left, vec!["Hi", "great!"]);
    }
}
