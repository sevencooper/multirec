//! Per-session interactive viewer (`viewer.html`) and the recordings library
//! (`index.html`). Both are single self-contained HTML files that work offline from
//! disk: data is embedded as JSON (browsers block `fetch` on `file://`), audio is
//! played straight from the session's files with `<audio>` elements, and waveforms
//! are precomputed here because the page can't decode local files itself.

use super::merge::{Contour, Tag, Utterance};
use super::{audio_io, Track};
use crate::audio::DeviceKind;
use crate::recorder::manifest::{Manifest, MANIFEST_FILE};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const VIEWER_FILE: &str = "viewer.html";
pub const LIBRARY_FILE: &str = "index.html";
/// Waveform resolution embedded in the page.
pub const PEAK_RATE: usize = 20;

const TEMPLATE: &str = include_str!("viewer.html");
const PLACEHOLDER: &str = "__MULTIREC_DATA__";

/// Per-speaker data the viewer needs beyond the manifest.
pub struct SpeakerView {
    pub peaks: Vec<u8>,
    pub baseline_f0: Option<f32>,
    pub baseline_db: Option<f32>,
    pub median_wpm: Option<f32>,
}

/// Peak level per 1/PEAK_RATE s of 16 kHz mono audio, sqrt-scaled to 0..=255
/// (sqrt makes quiet speech visible without clipping loud parts).
pub fn peaks(audio16k: &[f32]) -> Vec<u8> {
    let w = audio_io::SR / PEAK_RATE;
    audio16k
        .chunks(w)
        .map(|c| {
            let m = c.iter().fold(0f32, |m, v| m.max(v.abs())).min(1.0);
            (m.sqrt() * 255.0).round() as u8
        })
        .collect()
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn contour_str(c: Contour) -> &'static str {
    match c {
        Contour::Rising => "rising",
        Contour::Falling => "falling",
        Contour::Level => "level",
        Contour::Unknown => "",
    }
}

fn tag_str(t: Tag) -> &'static str {
    match t {
        Tag::Fast => "fast",
        Tag::Slow => "slow",
        Tag::Loud => "loud",
        Tag::Quiet => "quiet",
    }
}

fn utterance_json(u: &Utterance) -> Value {
    // Words as compact arrays: [text, start, end, confidence%, flags, f0, dB, pause]
    // flags: 1 = emphasised, 2 = non-speech tag.
    let words: Vec<Value> = u
        .words
        .iter()
        .map(|w| {
            let flags = (w.emphasis as u8) | ((w.is_tag as u8) << 1);
            let (f0, db) = w
                .prosody
                .as_ref()
                .map(|p| {
                    (
                        p.f0_hz.map(|f| f.round()),
                        Some((p.loud_db * 10.0).round() / 10.0),
                    )
                })
                .unwrap_or((None, None));
            json!([
                w.text,
                r2(w.start),
                r2(w.end),
                (w.prob * 100.0).round(),
                flags,
                f0,
                db,
                w.pause_after.map(r2)
            ])
        })
        .collect();
    json!({
        "sp": u.speaker,
        "s": r2(u.start),
        "e": r2(u.end),
        "c": contour_str(u.contour),
        "tags": u.tags.iter().map(|t| tag_str(*t)).collect::<Vec<_>>(),
        "gap": u.gap_before.map(r2),
        "w": words,
    })
}

fn kind_str(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Input => "microphone",
        DeviceKind::Loopback => "system audio",
    }
}

/// Write `viewer.html` into `dir`. `conv` is `None` for a recording that hasn't
/// been transcribed yet (waveforms, markers and playback only).
pub fn write_viewer(
    dir: &Path,
    manifest: &Manifest,
    speakers: &[SpeakerView],
    conv: Option<&[Utterance]>,
    model: Option<&str>,
    language: Option<&str>,
) -> Result<PathBuf> {
    let title = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Session".into());
    let exists = |f: &str| dir.join(f).exists();
    let mut files = vec![];
    for f in [
        "transcript.md",
        "transcript.srt",
        "transcript.vtt",
        "transcript.json",
        MANIFEST_FILE,
    ] {
        if exists(f) {
            files.push(f.to_string());
        }
    }
    if let Some(m) = &manifest.mix_file {
        if exists(m) {
            files.push(m.clone());
        }
    }
    let data = json!({
        "v": 1,
        "title": title,
        "started_at": manifest.started_at,
        "duration": r2(manifest.duration_s),
        "model": model,
        "language": language,
        "app": manifest.app,
        "format": manifest.format,
        "quality": manifest.bit_depth,
        "rate": manifest.sample_rate,
        "peak_rate": PEAK_RATE,
        "speakers": manifest.streams.iter().enumerate().map(|(i, s)| {
            let sv = speakers.get(i);
            json!({
                "label": s.label,
                "kind": kind_str(s.kind),
                "device": s.device_name,
                "file": s.file,
                "f0": sv.and_then(|v| v.baseline_f0).map(|f| f.round()),
                "db": sv.and_then(|v| v.baseline_db).map(|d| (d * 10.0).round() / 10.0),
                "wpm": sv.and_then(|v| v.median_wpm).map(|w| w.round()),
                "peaks": sv.map(|v| b64(&v.peaks)).unwrap_or_default(),
            })
        }).collect::<Vec<_>>(),
        "utts": conv.map(|c| c.iter().map(utterance_json).collect::<Vec<_>>()),
        "markers": manifest.markers.iter().map(|m| json!({"t": r2(m.t_s), "l": m.label})).collect::<Vec<_>>(),
        "files": files,
    });
    // Keep "</script>" (and friends) in transcript text from closing the data block.
    let json = serde_json::to_string(&data)?.replace("</", "<\\/");
    let html = TEMPLATE.replacen(PLACEHOLDER, &json, 1);
    let path = dir.join(VIEWER_FILE);
    let tmp = dir.join("viewer.html.tmp");
    std::fs::write(&tmp, html)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Viewer for a session with a transcript (called by the transcription job).
pub fn write_with_transcript(
    dir: &Path,
    manifest: &Manifest,
    tracks: &[Track],
    peaks: Vec<Vec<u8>>,
    conv: &[Utterance],
    model: &str,
    language: &str,
) -> Result<PathBuf> {
    let speakers: Vec<SpeakerView> = tracks
        .iter()
        .zip(peaks)
        .map(|(t, p)| SpeakerView {
            peaks: p,
            baseline_f0: t.baseline_f0,
            baseline_db: t.baseline_db,
            median_wpm: t.median_wpm,
        })
        .collect();
    write_viewer(
        dir,
        manifest,
        &speakers,
        Some(conv),
        Some(model),
        Some(language),
    )
}

/// Viewer for a freshly recorded (not yet transcribed) session.
pub fn write_audio_only(dir: &Path) -> Result<PathBuf> {
    // Never replace a transcribed viewer with the audio-only one.
    if dir.join("transcript.md").exists() && dir.join(VIEWER_FILE).exists() {
        return Ok(dir.join(VIEWER_FILE));
    }
    let manifest = Manifest::load(dir)?;
    let mut speakers = Vec::new();
    for s in &manifest.streams {
        let audio = audio_io::load_mono_16k(&dir.join(&s.file))
            .with_context(|| format!("reading {}", s.file))?;
        speakers.push(SpeakerView {
            peaks: peaks(&audio),
            baseline_f0: None,
            baseline_db: None,
            median_wpm: None,
        });
    }
    write_viewer(dir, &manifest, &speakers, None, None, None)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Percent-encode a path segment for use in a relative href.
fn href(seg: &str) -> String {
    seg.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn fmt_dur(s: f64) -> String {
    let t = s.max(0.0) as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

/// (Re)write `index.html` in the recordings folder, listing every session in it.
pub fn write_library(root: &Path) -> Result<PathBuf> {
    let mut rows: Vec<(String, String)> = Vec::new(); // (sort key, row html)
    let mut total = 0f64;
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Ok(m) = Manifest::load(&p) else { continue };
            let name = e.file_name().to_string_lossy().to_string();
            total += m.duration_s;
            let speakers = m
                .streams
                .iter()
                .map(|s| esc(&s.label))
                .collect::<Vec<_>>()
                .join(", ");
            let transcribed = p.join("transcript.md").exists();
            let viewer = p.join(VIEWER_FILE).exists();
            let open = if viewer {
                format!(
                    "<a class=\"btn\" href=\"{}/{VIEWER_FILE}\">Open</a>",
                    href(&name)
                )
            } else {
                "<span class=\"dim\" style=\"white-space:nowrap\">no viewer yet</span>".into()
            };
            let status = match (transcribed, m.complete) {
                (true, _) => "<span class=\"ok\">transcribed</span>",
                (false, true) => "<span class=\"dim\">audio only</span>",
                (false, false) => "<span class=\"warn\">incomplete</span>",
            };
            let when = m
                .started_at
                .get(..16)
                .unwrap_or(&m.started_at)
                .replace('T', " ");
            let row = format!(
                "<tr data-q=\"{q}\"><td>{open}</td><td><a href=\"{h}/\">{n}</a><div class=\"dim\">{when}</div></td>\
                 <td>{dur}</td><td>{speakers}</td><td>{fmt}</td><td>{status}</td><td>{nm}</td></tr>",
                q = esc(&format!("{name} {when} {}", m.streams.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(" ")).to_lowercase()),
                h = href(&name),
                n = esc(&name),
                dur = fmt_dur(m.duration_s),
                fmt = esc(&m.format.to_uppercase()),
                nm = m.markers.len(),
            );
            rows.push((m.started_at.clone(), row));
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let count = rows.len();
    let body: String = rows.into_iter().map(|r| r.1).collect::<Vec<_>>().join("\n");
    let html = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MultiRec library</title>
<style>
:root{{--bg:#f6f7f9;--fg:#1b1d22;--card:#fff;--dim:#6b7280;--line:#e3e6ea;--acc:#2f6fed}}
@media (prefers-color-scheme:dark){{:root{{--bg:#111318;--fg:#e6e8ec;--card:#1a1d24;--dim:#8b93a1;--line:#2a2f39;--acc:#6b9cff}}}}
body{{margin:0;background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif}}
main{{max-width:1100px;margin:0 auto;padding:24px 16px}}
h1{{font-size:22px;margin:0 0 4px}} .dim{{color:var(--dim);font-size:13px}}
input{{width:100%;max-width:360px;padding:8px 10px;border:1px solid var(--line);border-radius:8px;background:var(--card);color:var(--fg);margin:14px 0}}
.wrap{{overflow-x:auto;background:var(--card);border:1px solid var(--line);border-radius:10px}}
table{{border-collapse:collapse;width:100%}} th,td{{text-align:left;padding:10px 12px;border-bottom:1px solid var(--line);vertical-align:top}}
th{{font-size:12px;text-transform:uppercase;letter-spacing:.04em;color:var(--dim)}} tr:last-child td{{border-bottom:0}}
a{{color:var(--acc);text-decoration:none}} a:hover{{text-decoration:underline}}
.btn{{display:inline-block;padding:4px 12px;border-radius:6px;background:var(--acc);color:#fff}} .btn:hover{{text-decoration:none;opacity:.9}}
.ok{{color:#22a06b}} .warn{{color:#d97706}}
</style></head><body><main>
<h1>Recordings</h1>
<div class="dim">{count} sessions · {total} total · generated by MultiRec</div>
<input id="q" type="search" placeholder="Filter by name, date or speaker…" autofocus>
<div class="wrap"><table><thead><tr><th></th><th>Session</th><th>Length</th><th>Speakers</th><th>Format</th><th>Status</th><th>Markers</th></tr></thead>
<tbody id="rows">
{body}
</tbody></table></div>
</main>
<script>
document.getElementById('q').addEventListener('input',e=>{{const q=e.target.value.toLowerCase().trim();
for(const r of document.querySelectorAll('#rows tr'))r.style.display=!q||r.dataset.q.includes(q)?'':'none';}});
</script></body></html>
"#,
        total = fmt_dur(total),
    );
    let path = root.join(LIBRARY_FILE);
    std::fs::write(&path, html)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::manifest::{ManifestStream, Marker};
    use crate::transcribe::{merge, Word};

    fn manifest() -> Manifest {
        Manifest {
            app: "test".into(),
            version: 1,
            started_at: "2026-01-01T12:00:00Z".into(),
            duration_s: 10.0,
            sample_rate: 48_000,
            format: "flac".into(),
            bit_depth: "16-bit".into(),
            aligned: true,
            streams: vec![ManifestStream {
                label: "Me".into(),
                device_name: "mic".into(),
                kind: DeviceKind::Input,
                file: "Me.flac".into(),
                channels: 1,
                native_rate: 48_000,
                native_channels: 1,
                silence_inserted_s: 0.0,
                dropped_s: 0.0,
                drift_ppm: 0.0,
            }],
            mix_file: None,
            markers: vec![Marker {
                t_s: 2.0,
                label: "Marker 1".into(),
            }],
            complete: true,
        }
    }

    fn embedded(html: &str) -> Value {
        let tag = "id=\"data\">";
        let a = html.find(tag).unwrap() + tag.len();
        let b = a + html[a..].find("</script>").unwrap();
        serde_json::from_str(&html[a..b]).unwrap()
    }

    #[test]
    fn viewer_embeds_valid_data_and_escapes_script() {
        let dir = std::env::temp_dir().join(format!("multirec_viewer_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut tracks = vec![Track {
            label: "Me".into(),
            kind: DeviceKind::Input,
            file: "Me.flac".into(),
            words: vec![
                Word::new("Hello".into(), 1.0, 1.4, 0.9),
                Word::new("</script><b>x".into(), 1.5, 1.9, 0.4),
            ],
            baseline_f0: None,
            baseline_db: None,
            median_wpm: None,
        }];
        let conv = merge::build(&mut tracks, false);
        let p = write_with_transcript(
            &dir,
            &manifest(),
            &tracks,
            vec![vec![10, 200, 30]],
            &conv,
            "base",
            "en",
        )
        .unwrap();
        let html = std::fs::read_to_string(&p).unwrap();
        assert!(!html.contains(PLACEHOLDER));
        let d = embedded(&html);
        assert_eq!(d["utts"][0]["w"][1][0], "</script><b>x");
        assert_eq!(d["speakers"][0]["peaks"], b64(&[10, 200, 30]));
        assert_eq!(d["markers"][0]["l"], "Marker 1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn library_lists_sessions() {
        let root = std::env::temp_dir().join(format!("multirec_lib_{}", std::process::id()));
        let s = root.join("2026-01-01_12-00-00");
        std::fs::create_dir_all(&s).unwrap();
        manifest().save(&s).unwrap();
        let html = std::fs::read_to_string(write_library(&root).unwrap()).unwrap();
        assert!(html.contains("2026-01-01_12-00-00"));
        assert!(html.contains("1 sessions"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn audio_only_viewer_from_recording() {
        let dir =
            std::env::temp_dir().join(format!("multirec_viewer_audio_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = manifest();
        m.streams[0].file = "Me.wav".into();
        m.save(&dir).unwrap();
        let spec = crate::recorder::writer::SinkSpec {
            format: crate::config::AudioFormat::Wav,
            depth: crate::config::BitDepth::S16,
            rate: 48_000,
            opus_kbps: 32,
        };
        let mut sink = crate::recorder::writer::create_sink(&dir.join("Me.wav"), &spec, 1).unwrap();
        // 1 s silence, then 1 s of tone.
        let mut data = vec![0f32; 48_000];
        data.extend((0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.5));
        sink.write(&data).unwrap();
        sink.finalize().unwrap();
        let html = std::fs::read_to_string(write_audio_only(&dir).unwrap()).unwrap();
        let d = embedded(&html);
        assert!(
            d["utts"].is_null(),
            "untranscribed sessions have no utterances"
        );
        let peaks = d["speakers"][0]["peaks"].as_str().unwrap();
        assert!(
            peaks.len() >= 52,
            "~40 peaks expected, got {} b64 chars",
            peaks.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_matches_reference() {
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
    }
}
