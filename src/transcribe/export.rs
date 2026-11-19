use super::merge::{Contour, Tag, Utterance};
use super::{Track, Word};
use crate::recorder::manifest::{Manifest, Marker};
use anyhow::Result;
use serde::Serialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub fn fmt_ts(t: f64) -> String {
    let s = t.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn fmt_cue(t: f64, sep: char) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}{sep}{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

fn safe_name(s: &str) -> String {
    let n: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if n.is_empty() {
        "speaker".into()
    } else {
        n
    }
}

/// Word with markdown emphasis, keeping punctuation outside the asterisks.
fn md_word(w: &Word) -> String {
    let t = w.text.replace('*', "\\*");
    if w.is_tag {
        return format!("_{}_", t);
    }
    if !w.emphasis {
        return t;
    }
    let core_end = t.trim_end_matches(|c: char| !c.is_alphanumeric()).len();
    let core_start = t.len() - t.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
    if core_start >= core_end {
        return t;
    }
    format!(
        "{}**{}**{}",
        &t[..core_start],
        &t[core_start..core_end],
        &t[core_end..]
    )
}

fn md_utterance(u: &Utterance) -> String {
    let mut s = String::new();
    for (i, w) in u.words.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&md_word(w));
        if let Some(p) = w.pause_after {
            if p >= 1.5 {
                let _ = write!(s, " _(pause {:.1}s)_", p);
            } else {
                s.push_str(" …");
            }
        }
    }
    match u.contour {
        Contour::Rising => s.push_str(" ↗"),
        Contour::Falling => s.push_str(" ↘"),
        _ => {}
    }
    for t in &u.tags {
        s.push_str(match t {
            Tag::Fast => " `fast`",
            Tag::Slow => " `slow`",
            Tag::Loud => " `loud`",
            Tag::Quiet => " `quiet`",
        });
    }
    s
}

fn markdown(
    manifest: &Manifest,
    tracks: &[Track],
    conv: &[Utterance],
    model: &str,
    lang: &str,
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Transcript\n");
    let _ = writeln!(s, "- **Recorded:** {}", manifest.started_at);
    let _ = writeln!(s, "- **Duration:** {}", fmt_ts(manifest.duration_s));
    let speakers: Vec<String> = tracks
        .iter()
        .map(|t| {
            let kind = match t.kind {
                crate::audio::DeviceKind::Input => "microphone",
                crate::audio::DeviceKind::Loopback => "system audio",
            };
            format!("{} ({kind})", t.label)
        })
        .collect();
    let _ = writeln!(s, "- **Speakers:** {}", speakers.join(", "));
    let _ = writeln!(s, "- **Model:** {model} · language: {lang}\n");
    let _ = writeln!(
        s,
        "> **Legend** — ↗ rising intonation (question-like) · ↘ strongly falling · **word** emphasised \
         (louder/higher than the speaker's norm) · … short pause · _(pause 2.3s)_ longer silence · \
         `fast`/`slow` pace · `loud`/`quiet` relative to the speaker · _[tag]_ non-speech sound\n"
    );
    let mut markers: Vec<&Marker> = manifest.markers.iter().collect();
    markers.sort_by(|a, b| a.t_s.partial_cmp(&b.t_s).unwrap());
    let mut mi = 0;
    for u in conv {
        while mi < markers.len() && markers[mi].t_s <= u.start {
            let _ = writeln!(
                s,
                "\n---\n📍 **{}** — {}\n",
                markers[mi].label,
                fmt_ts(markers[mi].t_s)
            );
            mi += 1;
        }
        if let Some(g) = u.gap_before {
            let _ = writeln!(s, "_(silence {:.1}s)_\n", g);
        }
        let _ = writeln!(
            s,
            "**[{}] {}:** {}\n",
            fmt_ts(u.start),
            u.label,
            md_utterance(u)
        );
    }
    for m in &markers[mi..] {
        let _ = writeln!(s, "\n---\n📍 **{}** — {}\n", m.label, fmt_ts(m.t_s));
    }
    s
}

/// Split an utterance into subtitle-sized cues (≤ 14 words, ≤ 7 s).
fn cues(u: &Utterance) -> Vec<(f64, f64, String)> {
    let mut out = Vec::new();
    let mut cur: Vec<&Word> = Vec::new();
    for w in &u.words {
        if let Some(f) = cur.first() {
            if cur.len() >= 14 || w.end - f.start > 7.0 {
                out.push((f.start, cur.last().unwrap().end, join(&cur)));
                cur.clear();
            }
        }
        cur.push(w);
    }
    if let Some(f) = cur.first() {
        out.push((f.start, cur.last().unwrap().end, join(&cur)));
    }
    out
}

fn join(ws: &[&Word]) -> String {
    ws.iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn subtitles(conv: &[Utterance], speaker: Option<usize>, vtt: bool) -> String {
    let mut s = String::new();
    if vtt {
        s.push_str("WEBVTT\n\n");
    }
    let sep = if vtt { '.' } else { ',' };
    let mut n = 1;
    for u in conv
        .iter()
        .filter(|u| speaker.is_none_or(|sp| u.speaker == sp))
    {
        for (a, b, text) in cues(u) {
            if !vtt {
                let _ = writeln!(s, "{n}");
            }
            let _ = writeln!(
                s,
                "{} --> {}",
                fmt_cue(a, sep),
                fmt_cue(b.max(a + 0.2), sep)
            );
            if vtt {
                let _ = writeln!(s, "<v {}>{}\n", u.label, text);
            } else if speaker.is_none() {
                let _ = writeln!(s, "{}: {}\n", u.label, text);
            } else {
                let _ = writeln!(s, "{}\n", text);
            }
            n += 1;
        }
    }
    s
}

#[derive(Serialize)]
struct JsonSpeaker<'a> {
    label: &'a str,
    kind: crate::audio::DeviceKind,
    file: &'a str,
    baseline_f0_hz: Option<f32>,
    baseline_loudness_db: Option<f32>,
    median_wpm: Option<f32>,
}

#[derive(Serialize)]
struct JsonOut<'a> {
    recorded_at: &'a str,
    duration_s: f64,
    model: &'a str,
    language: &'a str,
    speakers: Vec<JsonSpeaker<'a>>,
    markers: &'a [Marker],
    utterances: &'a [Utterance],
}

pub fn write_all(
    dir: &Path,
    manifest: &Manifest,
    tracks: &[Track],
    conv: &[Utterance],
    model: &str,
    lang: &str,
) -> Result<Vec<PathBuf>> {
    let mut outs = Vec::new();
    let mut put = |name: String, content: String| -> Result<()> {
        let p = dir.join(name);
        std::fs::write(&p, content)?;
        outs.push(p);
        Ok(())
    };
    put(
        "transcript.md".into(),
        markdown(manifest, tracks, conv, model, lang),
    )?;
    let json = JsonOut {
        recorded_at: &manifest.started_at,
        duration_s: manifest.duration_s,
        model,
        language: lang,
        speakers: tracks
            .iter()
            .map(|t| JsonSpeaker {
                label: &t.label,
                kind: t.kind,
                file: &t.file,
                baseline_f0_hz: t.baseline_f0,
                baseline_loudness_db: t.baseline_db,
                median_wpm: t.median_wpm,
            })
            .collect(),
        markers: &manifest.markers,
        utterances: conv,
    };
    put(
        "transcript.json".into(),
        serde_json::to_string_pretty(&json)?,
    )?;
    put("transcript.srt".into(), subtitles(conv, None, false))?;
    put("transcript.vtt".into(), subtitles(conv, None, true))?;
    for (i, t) in tracks.iter().enumerate() {
        put(
            format!("transcript_{}.srt", safe_name(&t.label)),
            subtitles(conv, Some(i), false),
        )?;
    }
    Ok(outs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_keeps_punctuation_outside() {
        let mut w = Word::new("really,".into(), 0.0, 1.0, 1.0);
        w.emphasis = true;
        assert_eq!(md_word(&w), "**really**,");
    }

    #[test]
    fn cue_format() {
        assert_eq!(fmt_cue(3725.5, ','), "01:02:05,500");
    }
}
