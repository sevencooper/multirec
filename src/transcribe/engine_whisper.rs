use super::Word;
use anyhow::{anyhow, Result};
use std::path::Path;
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters,
};

pub struct Engine {
    ctx: WhisperContext,
    threads: i32,
    lang: String,
}

/// DTW alignment-head presets give accurate per-token timestamps.
fn dtw_preset(model_id: &str) -> Option<DtwModelPreset> {
    let base = model_id.split("-q").next().unwrap_or(model_id);
    Some(match base {
        "tiny.en" => DtwModelPreset::TinyEn,
        "tiny" => DtwModelPreset::Tiny,
        "base.en" => DtwModelPreset::BaseEn,
        "base" => DtwModelPreset::Base,
        "small.en" => DtwModelPreset::SmallEn,
        "small" => DtwModelPreset::Small,
        "medium.en" => DtwModelPreset::MediumEn,
        "medium" => DtwModelPreset::Medium,
        "large-v3-turbo" => DtwModelPreset::LargeV3Turbo,
        "large-v3" => DtwModelPreset::LargeV3,
        _ => return None,
    })
}

struct RawWord {
    bytes: Vec<u8>,
    start_cs: i64,
    psum: f32,
    n: u32,
}

impl Engine {
    pub fn load(path: &Path, model_id: &str, threads: usize, lang: &str) -> Result<Self> {
        Self::load_with(path, model_id, threads, lang, true, false)
    }

    pub fn load_with(
        path: &Path,
        model_id: &str,
        threads: usize,
        lang: &str,
        dtw: bool,
        flash_attn: bool,
    ) -> Result<Self> {
        let mut cp = WhisperContextParameters::default();
        cp.flash_attn(flash_attn);
        if let Some(preset) = dtw_preset(model_id).filter(|_| dtw) {
            cp.dtw_parameters(DtwParameters {
                mode: DtwMode::ModelPreset {
                    model_preset: preset,
                },
                ..Default::default()
            });
        }
        let ctx = WhisperContext::new_with_params(path, cp)
            .map_err(|e| anyhow!("loading model: {e:?}"))?;
        Ok(Self {
            ctx,
            threads: threads.max(1) as i32,
            lang: lang.to_string(),
        })
    }

    /// Transcribe 16 kHz mono audio (≤ ~30 s works best); word times are offset by `offset_s`.
    pub fn transcribe(&self, audio: &[f32], offset_s: f64) -> Result<Vec<Word>> {
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| anyhow!("whisper state: {e:?}"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(Some(self.lang.as_str()));
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_token_timestamps(true);

        // Whisper needs at least ~1 s of input; pad short slices with silence.
        let min = super::audio_io::SR + super::audio_io::SR / 10;
        let padded;
        let input = if audio.len() < min {
            let mut v = audio.to_vec();
            v.resize(min, 0.0);
            padded = v;
            &padded[..]
        } else {
            audio
        };
        state
            .full(params, input)
            .map_err(|e| anyhow!("whisper: {e:?}"))?;

        let eot = self.ctx.token_eot();
        let mut words: Vec<Word> = Vec::new();
        for seg in state.as_iter() {
            let seg_t1 = seg.end_timestamp();
            let mut raw: Vec<RawWord> = Vec::new();
            for j in 0..seg.n_tokens() {
                let Some(tok) = seg.get_token(j) else {
                    continue;
                };
                let d = tok.token_data();
                if d.id >= eot {
                    continue; // special / timestamp tokens
                }
                let bytes = tok.to_bytes().unwrap_or(&[]);
                if bytes.is_empty() {
                    continue;
                }
                let t = if d.t_dtw >= 0 { d.t_dtw } else { d.t0 };
                let new_word = bytes[0] == b' ' || raw.is_empty();
                if new_word {
                    raw.push(RawWord {
                        bytes: bytes.to_vec(),
                        start_cs: t,
                        psum: d.p,
                        n: 1,
                    });
                } else if let Some(w) = raw.last_mut() {
                    w.bytes.extend_from_slice(bytes);
                    w.psum += d.p;
                    w.n += 1;
                }
            }
            for k in 0..raw.len() {
                let w = &raw[k];
                let text = String::from_utf8_lossy(&w.bytes).trim().to_string();
                if text.is_empty() {
                    continue;
                }
                let start = w.start_cs as f64 / 100.0;
                let next = raw.get(k + 1).map_or(seg_t1, |n| n.start_cs) as f64 / 100.0;
                // A word lasts until the next one starts, unless that is after a pause.
                let est = 0.15 + 0.075 * text.chars().count() as f64;
                let end = next.min(start + est).max(start + 0.05);
                words.push(Word::new(
                    text,
                    start + offset_s,
                    end + offset_s,
                    w.psum / w.n as f32,
                ));
            }
        }
        words.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
        Ok(merge_split_tags(words))
    }
}

/// Re-join bracketed annotations that got split into several "words".
fn merge_split_tags(words: Vec<Word>) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::with_capacity(words.len());
    let mut open: Option<Word> = None;
    for w in words {
        if let Some(mut o) = open.take() {
            o.text.push(' ');
            o.text.push_str(&w.text);
            o.end = w.end;
            if w.text.ends_with(']') || w.text.ends_with(')') {
                out.push(Word::new(o.text, o.start, o.end, o.prob));
            } else {
                open = Some(o);
            }
            continue;
        }
        let t = w.text.as_str();
        if (t.starts_with('[') && !t.contains(']')) || (t.starts_with('(') && !t.contains(')')) {
            open = Some(w);
        } else {
            out.push(w);
        }
    }
    if let Some(o) = open {
        out.push(o);
    }
    out
}

#[cfg(test)]
mod bench {
    use super::*;

    /// MULTIREC_TTS_DIR=<clips> MULTIREC_TEST_MODEL=<id> cargo test --release -- --ignored --nocapture whisper_speed
    #[test]
    #[ignore]
    fn whisper_speed() {
        let dir = std::path::PathBuf::from(std::env::var("MULTIREC_TTS_DIR").unwrap());
        let mut audio = Vec::new();
        for c in ["me1", "other1", "me2", "other2"] {
            audio.extend(
                crate::transcribe::audio_io::load_mono_16k(&dir.join(format!("{c}.wav"))).unwrap(),
            );
        }
        audio.truncate(16_000 * 28);
        let id = std::env::var("MULTIREC_TEST_MODEL").unwrap_or_else(|_| "base.en".into());
        let m = crate::transcribe::models::find(&id).unwrap();
        let path = crate::transcribe::models::path(m);
        println!("system_info: {}", whisper_rs::print_system_info());
        {
            let e = Engine::load_with(&path, &id, 8, "en", false, false).unwrap();
            let mut st = e.ctx.create_state().unwrap();
            let t = std::time::Instant::now();
            st.pcm_to_mel(&audio, 8).unwrap();
            let mel = t.elapsed();
            let t = std::time::Instant::now();
            st.encode(0, 8).unwrap();
            println!(
                "mel={:.2}s encode(1 window)={:.2}s",
                mel.as_secs_f32(),
                t.elapsed().as_secs_f32()
            );
        }
        for (dtw, flash, threads) in [(true, false, 8)] {
            let t = std::time::Instant::now();
            let e = Engine::load_with(&path, &id, threads, "en", dtw, flash).unwrap();
            let load = t.elapsed();
            let t = std::time::Instant::now();
            let w = e.transcribe(&audio, 0.0).unwrap();
            println!(
                "dtw={dtw:<5} flash={flash:<5} threads={threads:<2} load={:>5.2}s transcribe={:>6.2}s words={}",
                load.as_secs_f32(), t.elapsed().as_secs_f32(), w.len()
            );
        }
    }
}
