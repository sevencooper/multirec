use super::widgets::{fmt_dur, section_title, AMBER, DIM, GREEN, RED};
use super::App;
use crate::recorder::manifest::Manifest;
use crate::transcribe::{self, models, JobOptions, JobProgress, Progress};
use eframe::egui::{self, RichText, Ui};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

const LANGS: &[(&str, &str)] = &[
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("tr", "Turkish"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
];

#[derive(Default)]
pub struct TranscribeUi {
    pub session_dir: String,
    job: Option<Progress>,
    preview: Option<(String, Result<Manifest, String>)>,
}

impl App {
    pub(super) fn transcribe_ui(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        section_title(ui, "📝 Speaker transcripts");
        ui.label(
            RichText::new(
                "Each recorded stream is transcribed separately, so every line is attributed to the right side \
                 of the conversation. Word timing, pauses, emphasis, intonation and pace are preserved.",
            )
            .color(DIM),
        );
        if !transcribe::engine_available() {
            ui.colored_label(
                RED,
                "This build was compiled without the speech-to-text engine (feature `whisper`).",
            );
        }
        ui.add_space(6.0);
        let running = self.tr.job.as_ref().is_some_and(|j| !j.lock().done);

        egui::Grid::new("tr")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Session folder");
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        !running,
                        egui::TextEdit::singleline(&mut self.tr.session_dir).desired_width(420.0),
                    );
                    if ui
                        .add_enabled(!running, egui::Button::new("Browse…"))
                        .clicked()
                    {
                        let start = if self.tr.session_dir.is_empty() {
                            self.cfg.output_dir.clone()
                        } else {
                            PathBuf::from(&self.tr.session_dir)
                        };
                        if let Some(p) = rfd::FileDialog::new().set_directory(start).pick_folder() {
                            self.tr.session_dir = p.display().to_string();
                        }
                    }
                });
                ui.end_row();

                ui.label("Model");
                let tc = &mut self.cfg.transcribe;
                let cur = models::find(&tc.model).ok();
                let text = cur.map_or(tc.model.clone(), |m| {
                    format!("{}  ({} MB)", m.id, m.size_mb)
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("model")
                        .width(260.0)
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            for m in models::MODELS {
                                let mark = if models::is_downloaded(m) {
                                    "✔ "
                                } else {
                                    "   "
                                };
                                ui.selectable_value(
                                    &mut tc.model,
                                    m.id.to_string(),
                                    format!("{mark}{} — {} ({} MB)", m.id, m.desc, m.size_mb),
                                );
                            }
                        });
                    if let Some(m) = cur {
                        if !models::is_downloaded(m) {
                            ui.label(RichText::new("downloads on first use").small().color(DIM));
                        }
                    }
                });
                ui.end_row();

                ui.label("Language");
                ui.horizontal(|ui| {
                    let lname = LANGS
                        .iter()
                        .find(|l| l.0 == tc.language)
                        .map_or(tc.language.clone(), |l| l.1.to_string());
                    egui::ComboBox::from_id_salt("lang")
                        .selected_text(lname)
                        .show_ui(ui, |ui| {
                            for (code, name) in LANGS {
                                ui.selectable_value(&mut tc.language, code.to_string(), *name);
                            }
                        });
                    if tc.model.ends_with(".en") && tc.language != "en" && tc.language != "auto" {
                        ui.colored_label(AMBER, "English-only model selected");
                    }
                });
                ui.end_row();

                ui.label("CPU threads");
                let maxt = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(8);
                ui.add(egui::Slider::new(&mut tc.threads, 1..=maxt));
                ui.end_row();

                ui.label("Options");
                ui.vertical(|ui| {
                    ui.checkbox(
                        &mut tc.prosody,
                        "Prosody annotations (intonation, emphasis, pace, loudness)",
                    );
                    ui.checkbox(
                        &mut tc.bleed_suppression,
                        "Remove speaker bleed (system audio picked up by the mic)",
                    );
                });
                ui.end_row();
            });

        // Session preview.
        if self
            .tr
            .preview
            .as_ref()
            .is_none_or(|p| p.0 != self.tr.session_dir)
        {
            let r = if self.tr.session_dir.trim().is_empty() {
                Err("Choose a session folder (one containing session.json).".to_string())
            } else {
                Manifest::load(std::path::Path::new(self.tr.session_dir.trim()))
                    .map_err(|e| format!("{e:#}"))
            };
            self.tr.preview = Some((self.tr.session_dir.clone(), r));
        }
        ui.add_space(6.0);
        let ok = match &self.tr.preview {
            Some((_, Ok(m))) => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(format!("Recorded {} · {}", m.started_at, fmt_dur(m.duration_s)));
                    for s in &m.streams {
                        ui.label(RichText::new(format!("• {} — {} ({})", s.label, s.device_name, s.file)).small());
                    }
                    if !m.complete {
                        ui.colored_label(AMBER, "This session was not closed cleanly (crash?). Files should still be readable.");
                    }
                });
                true
            }
            Some((_, Err(e))) => {
                ui.colored_label(DIM, e);
                false
            }
            None => false,
        };

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can = ok && !running && transcribe::engine_available();
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(RichText::new("▶  Transcribe").size(16.0))
                        .min_size(egui::vec2(140.0, 32.0)),
                )
                .clicked()
            {
                let tc = &self.cfg.transcribe;
                let opts = JobOptions {
                    session_dir: PathBuf::from(self.tr.session_dir.trim()),
                    model_id: tc.model.clone(),
                    language: tc.language.clone(),
                    threads: tc.threads,
                    bleed: tc.bleed_suppression,
                    prosody: tc.prosody,
                };
                let p: Progress = Arc::new(Mutex::new(JobProgress::default()));
                let p2 = p.clone();
                std::thread::spawn(move || transcribe::run_job(opts, p2));
                self.tr.job = Some(p);
            }
            if running && ui.button("Cancel").clicked() {
                if let Some(j) = &self.tr.job {
                    j.lock().cancel = true;
                }
            }
        });

        if let Some(job) = self.tr.job.clone() {
            let g = job.lock();
            ui.add_space(6.0);
            ui.label(&g.stage);
            ui.add(
                egui::ProgressBar::new(g.fraction)
                    .show_percentage()
                    .animate(!g.done),
            );
            if let Some(e) = &g.error {
                ui.colored_label(RED, e);
            }
            if g.done && g.error.is_none() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(GREEN, "✔ Transcripts written:");
                    for o in &g.outputs {
                        let name = o
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        if ui.link(name).clicked() {
                            crate::sysinfo::open_in_explorer(o);
                        }
                    }
                    if let Some(dir) = g.outputs.first().and_then(|o| o.parent()) {
                        if ui.button("📂 Open folder").clicked() {
                            crate::sysinfo::open_in_explorer(dir);
                        }
                    }
                });
            }
            if !g.log.is_empty() {
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &g.log {
                            ui.label(RichText::new(l).small().monospace().color(DIM));
                        }
                    });
            }
        }
    }
}
