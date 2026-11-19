use super::widgets::{self, fmt_bytes, fmt_dur, fmt_hm, section_title, AMBER, DIM, GREEN, RED};
use super::{App, Tab};
use crate::audio::{CaptureStatus, DeviceKind};
use crate::config::{AudioFormat, BitDepth, Preset};
use crate::recorder::session::{Cmd, Phase, StartSpec, StreamSpec};
use eframe::egui::{self, Button, Color32, RichText, Ui};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn sanitize(s: &str) -> String {
    let t: String = s
        .trim()
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let t = t.trim_matches(|c| c == '.' || c == ' ').to_string();
    if t.is_empty() {
        "stream".into()
    } else {
        t
    }
}

impl App {
    fn stream_channels(&self, kind: DeviceKind) -> u16 {
        if kind == DeviceKind::Loopback && self.cfg.keep_loopback_stereo {
            2
        } else {
            1
        }
    }

    fn label_for(&self, key: &str, kind: DeviceKind, name: &str) -> String {
        self.cfg
            .labels
            .get(key)
            .cloned()
            .unwrap_or_else(|| match kind {
                DeviceKind::Input => name.split(" (").next().unwrap_or(name).to_string(),
                DeviceKind::Loopback => "System".into(),
            })
    }

    pub(super) fn start_recording(&mut self) {
        let now = chrono::Local::now();
        let folder = sanitize(
            &self
                .cfg
                .folder_template
                .replace("{date}", &now.format("%Y-%m-%d").to_string())
                .replace("{time}", &now.format("%H-%M-%S").to_string()),
        );
        let mut dir: PathBuf = self.cfg.output_dir.join(&folder);
        let mut n = 2;
        while dir.exists() {
            dir = self.cfg.output_dir.join(format!("{folder}_{n}"));
            n += 1;
        }
        let ext = self.cfg.format.ext();
        let mut used = vec![format!("mix.{ext}")];
        let mut streams = Vec::new();
        // Inputs first, then loopbacks, in device order.
        for kind in [DeviceKind::Input, DeviceKind::Loopback] {
            for d in self.devices.iter().filter(|d| d.kind == kind) {
                if !self.cfg.selected.contains(&d.key) {
                    continue;
                }
                let Some(cap) = self.captures.get(&d.key) else {
                    continue;
                };
                let label = self.label_for(&d.key, d.kind, &d.name);
                let base = sanitize(&label);
                let mut file = format!("{base}.{ext}");
                let mut i = 2;
                while used.iter().any(|u| u.eq_ignore_ascii_case(&file)) {
                    file = format!("{base}_{i}.{ext}");
                    i += 1;
                }
                used.push(file.clone());
                streams.push(StreamSpec {
                    key: d.key.clone(),
                    label,
                    device_name: d.name.clone(),
                    kind: d.kind,
                    channels: self.stream_channels(d.kind),
                    file_name: file,
                    shared: cap.shared.clone(),
                });
            }
        }
        if streams.is_empty() {
            self.notify("Select at least one source to record.", true);
            return;
        }
        self.cmd(Cmd::Start(StartSpec {
            dir,
            streams,
            format: self.cfg.format,
            depth: self.cfg.effective_depth(),
            rate: self.cfg.effective_rate(),
            opus_kbps: self.cfg.opus_kbps,
            write_mix: self.cfg.write_mix,
            started_at: now.to_rfc3339(),
        }));
    }

    pub(super) fn record_ui(&mut self, ui: &mut Ui) {
        let phase = self.view.phase;
        let idle = phase == Phase::Idle;

        // ── Transport ─────────────────────────────────────────────
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (txt, col) = match phase {
                Phase::Recording => ("● REC", RED),
                Phase::Paused => ("⏸ PAUSED", AMBER),
                Phase::Idle => ("■ READY", DIM),
            };
            ui.label(RichText::new(txt).size(20.0).strong().color(col));
            ui.add_space(6.0);
            ui.label(
                RichText::new(fmt_dur(self.view.elapsed_s))
                    .size(32.0)
                    .monospace(),
            );
            ui.add_space(16.0);
            let big = |t: &str| {
                Button::new(RichText::new(t).size(16.0)).min_size(egui::vec2(120.0, 34.0))
            };
            if idle {
                if ui
                    .add(big("⏺  Record").fill(Color32::from_rgb(150, 40, 35)))
                    .clicked()
                {
                    self.start_recording();
                }
            } else {
                if ui.add(big("⏹  Stop")).clicked() {
                    self.cmd(Cmd::Stop);
                }
                let pl = if phase == Phase::Paused {
                    "▶  Resume"
                } else {
                    "⏸  Pause"
                };
                if ui.add(big(pl)).clicked() {
                    self.cmd(Cmd::TogglePause);
                }
                if ui.add(big("🚩  Marker")).clicked() {
                    self.cmd(Cmd::Marker);
                }
            }
        });
        if self._hotkeys.is_some() {
            ui.label(RichText::new(super::hotkeys::HINT).small().color(DIM));
        }
        if let Some(dir) = &self.view.dir {
            if !idle {
                ui.label(
                    RichText::new(format!("Recording to {}", dir.display()))
                        .small()
                        .color(DIM),
                );
            }
        }
        ui.separator();

        let mut selection_changed = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.totals_ui(ui);
                self.preset_ui(ui, idle);
                ui.add_space(6.0);
                selection_changed |= self.sources_ui(ui, idle, DeviceKind::Input);
                ui.add_space(6.0);
                selection_changed |= self.sources_ui(ui, idle, DeviceKind::Loopback);
                ui.add_space(6.0);
                self.markers_ui(ui);
                self.last_result_ui(ui);
                ui.add_space(6.0);
                egui::CollapsingHeader::new(RichText::new("Output settings").strong().size(15.0))
                    .default_open(false)
                    .show(ui, |ui| self.settings_ui(ui, idle));
            });
        if selection_changed {
            self.sync_captures();
        }
    }

    fn totals_ui(&mut self, ui: &mut Ui) {
        let mut chans = 0u32;
        let mut n_streams = 0;
        for d in &self.devices {
            if self.cfg.selected.contains(&d.key) && self.captures.contains_key(&d.key) {
                chans += self.stream_channels(d.kind) as u32;
                n_streams += 1;
            }
        }
        if self.cfg.write_mix && n_streams > 0 {
            chans += 2;
        }
        let estimate = self.cfg.bytes_per_sec_per_channel() * chans as f64;
        // FLAC size depends on content: switch to the measured rate once recording.
        let bps =
            if self.cfg.format == AudioFormat::Flac && !self.idle() && self.view.elapsed_s > 5.0 {
                self.view.total_bytes as f64 / self.view.elapsed_s
            } else {
                estimate
            };
        if self
            .disk_checked
            .is_none_or(|t| t.elapsed() > Duration::from_secs(3))
        {
            self.disk_checked = Some(Instant::now());
            self.disk_free = crate::sysinfo::disk_free(&self.cfg.output_dir);
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("totals")
                .num_columns(4)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    let stat = |ui: &mut Ui, k: &str, v: String| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(k).small().color(DIM));
                            ui.label(RichText::new(v).size(16.0).strong());
                        });
                    };
                    stat(ui, "Total size", fmt_bytes(self.view.total_bytes));
                    let est = if self.cfg.format == AudioFormat::Flac {
                        "≈ per hour (FLAC est.)"
                    } else {
                        "Per hour"
                    };
                    stat(ui, est, fmt_bytes((bps * 3600.0) as u64));
                    stat(
                        ui,
                        "Free disk space",
                        self.disk_free.map_or("—".into(), fmt_bytes),
                    );
                    let left = match (self.disk_free, bps > 0.0) {
                        (Some(f), true) => fmt_hm(f as f64 / bps),
                        _ => "—".into(),
                    };
                    stat(ui, "Recording time left", left);
                    ui.end_row();
                });
            let quality = if self.cfg.format == AudioFormat::Opus {
                format!("{} kbps", self.cfg.opus_kbps)
            } else {
                self.cfg.effective_depth().label().to_string()
            };
            let mut info = format!(
                "{} streams · {} Hz · {} {}{}",
                n_streams,
                self.cfg.effective_rate(),
                quality,
                self.cfg.format.ext().to_uppercase(),
                if self.cfg.write_mix {
                    " · + stereo mix (L = mics, R = system)"
                } else {
                    ""
                }
            );
            if self.cfg.format == AudioFormat::Wav {
                let per_file_hours = 4.0e9 / (self.cfg.bytes_per_sec_per_channel() * 2.0) / 3600.0;
                info.push_str(&format!(" · WAV limit ≈ {:.0} h per file", per_file_hours));
            }
            ui.label(RichText::new(info).small().color(DIM));
        });
    }

    /// One-click quality presets, always visible above the sources.
    fn preset_ui(&mut self, ui: &mut Ui, idle: bool) {
        let current = Preset::detect(&self.cfg);
        ui.add_enabled_ui(idle, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Quality").strong());
                for p in Preset::ALL {
                    let resp = ui
                        .selectable_label(current == p, p.label())
                        .on_hover_text(p.description());
                    if resp.clicked() {
                        p.apply(&mut self.cfg);
                    }
                }
                if current == Preset::Custom {
                    ui.selectable_label(true, "Custom")
                        .on_hover_text("Adjusted in Output settings");
                }
                ui.label(RichText::new(current.description()).small().color(DIM));
            });
        });
    }

    /// Returns true if the selection changed.
    fn sources_ui(&mut self, ui: &mut Ui, idle: bool, kind: DeviceKind) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            section_title(
                ui,
                match kind {
                    DeviceKind::Input => "🎤 Microphones / inputs",
                    DeviceKind::Loopback => "🔊 Speakers / outputs (records what you hear)",
                },
            );
            if kind == DeviceKind::Input {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(idle, Button::new("⟳ Refresh devices"))
                        .clicked()
                    {
                        self.refresh_devices();
                        changed = true;
                    }
                });
            }
        });
        let devices: Vec<_> = self
            .devices
            .iter()
            .filter(|d| d.kind == kind)
            .cloned()
            .collect();
        if devices.is_empty() {
            ui.label(RichText::new("No devices found.").color(DIM));
        }
        for d in devices {
            let mut sel = self.cfg.selected.contains(&d.key);
            let frame = if sel {
                egui::Frame::group(ui.style()).fill(Color32::from_rgb(32, 34, 40))
            } else {
                egui::Frame::none().inner_margin(egui::Margin::symmetric(6.0, 2.0))
            };
            frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let name = if d.is_default {
                        format!("{}  (default)", d.name)
                    } else {
                        d.name.clone()
                    };
                    if ui
                        .add_enabled(
                            idle,
                            egui::Checkbox::new(&mut sel, RichText::new(name).strong()),
                        )
                        .changed()
                    {
                        if sel {
                            self.cfg.selected.push(d.key.clone());
                            if !self.cfg.labels.contains_key(&d.key) {
                                let l = self.label_for(&d.key, d.kind, &d.name);
                                self.cfg.labels.insert(d.key.clone(), l);
                            }
                        } else {
                            self.cfg.selected.retain(|k| k != &d.key);
                        }
                        changed = true;
                    }
                });
                if !sel {
                    return;
                }
                let Some(cap) = self.captures.get(&d.key) else {
                    ui.label(RichText::new("starting…").color(DIM));
                    return;
                };
                let shared = cap.shared.clone();
                ui.horizontal(|ui| {
                    ui.label("Label");
                    let label = self.cfg.labels.entry(d.key.clone()).or_insert_with(|| {
                        if kind == DeviceKind::Input {
                            "Me".into()
                        } else {
                            "Others".into()
                        }
                    });
                    ui.add_enabled(idle, egui::TextEdit::singleline(label).desired_width(110.0));
                    ui.add_space(8.0);
                    ui.label("Gain");
                    let g = self.cfg.gains_db.entry(d.key.clone()).or_insert(0.0);
                    if ui
                        .add(
                            egui::DragValue::new(g)
                                .range(-24.0..=24.0)
                                .speed(0.1)
                                .suffix(" dB")
                                .fixed_decimals(1),
                        )
                        .changed()
                    {
                        shared.set_gain_db(*g);
                    }
                    if *g != 0.0 && ui.small_button("0").on_hover_text("Reset gain").clicked() {
                        *g = 0.0;
                        shared.set_gain_db(0.0);
                    }
                });
                let meter = shared.meter.lock().clone();
                let w = ui.available_width();
                ui.horizontal(|ui| {
                    widgets::level_meter(ui, &meter, (w - 120.0).max(100.0));
                    let pk = widgets::db(meter.peak);
                    ui.label(
                        RichText::new(if pk <= -59.0 {
                            "  -∞ dB".to_string()
                        } else {
                            format!("{pk:>5.1} dB")
                        })
                        .monospace(),
                    );
                    widgets::clip_led(ui, &meter);
                });
                let col = if kind == DeviceKind::Input {
                    Color32::from_rgb(110, 170, 250)
                } else {
                    Color32::from_rgb(200, 140, 250)
                };
                widgets::waveform(ui, &meter, w, 38.0, col);

                // Status / stats line.
                let status = shared.status.lock().clone();
                let fmt = *shared.format.lock();
                let mut parts: Vec<(String, Color32)> = Vec::new();
                match &status {
                    CaptureStatus::Running => parts.push(("● live".into(), GREEN)),
                    CaptureStatus::Starting => parts.push(("○ starting".into(), DIM)),
                    CaptureStatus::Lost(e) => {
                        parts.push((format!("⚠ device lost — retrying ({e})"), RED))
                    }
                    CaptureStatus::Stopped => parts.push(("■ stopped".into(), DIM)),
                }
                if let Some((r, c)) = fmt {
                    parts.push((
                        format!(
                            "{} Hz {}ch → {} Hz {}ch",
                            r,
                            c,
                            self.cfg.effective_rate(),
                            self.stream_channels(kind)
                        ),
                        DIM,
                    ));
                }
                if let Some(st) = self.view.streams.iter().find(|s| s.key == d.key) {
                    parts.push((fmt_bytes(st.bytes), Color32::LIGHT_GRAY));
                    parts.push((format!("drift {:+.0} ppm", st.drift_ppm), DIM));
                    parts.push((
                        format!("sync err {:+.1} ms", st.err_ms),
                        if st.err_ms.abs() > 5.0 { AMBER } else { DIM },
                    ));
                    if st.silence_s > 0.05 {
                        parts.push((format!("gap-filled {:.1} s", st.silence_s), DIM));
                    }
                    if st.dropped_s > 0.05 {
                        parts.push((format!("dropped {:.2} s", st.dropped_s), AMBER));
                    }
                }
                if meter.clip_count > 0 {
                    parts.push((format!("clipped ×{}", meter.clip_count), AMBER));
                }
                ui.horizontal_wrapped(|ui| {
                    for (i, (t, c)) in parts.into_iter().enumerate() {
                        if i > 0 {
                            ui.label(RichText::new("·").small().color(DIM));
                        }
                        ui.label(RichText::new(t).small().color(c));
                    }
                });
            });
        }
        changed
    }

    fn markers_ui(&mut self, ui: &mut Ui) {
        if self.view.markers.is_empty() {
            return;
        }
        section_title(ui, "🚩 Markers");
        for m in &self.view.markers {
            ui.label(RichText::new(format!("{}  {}", fmt_dur(m.t_s), m.label)).monospace());
        }
        ui.add_space(4.0);
    }

    fn last_result_ui(&mut self, ui: &mut Ui) {
        if !self.idle() {
            return;
        }
        if let Some(Ok(dir)) = self.view.last_result.clone() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(GREEN, "✔ Last recording:");
                    ui.label(dir.display().to_string());
                    if ui.button("📂 Open folder").clicked() {
                        crate::sysinfo::open_in_explorer(&dir);
                    }
                    let viewer = dir.join(crate::transcribe::viewer::VIEWER_FILE);
                    if viewer.exists()
                        && ui
                            .button("🌐 Viewer")
                            .on_hover_text("Play, scrub and browse this session in your browser")
                            .clicked()
                    {
                        crate::sysinfo::open_in_explorer(&viewer);
                    }
                    if ui.button("📝 Transcribe…").clicked() {
                        self.tr.session_dir = dir.display().to_string();
                        self.tab = Tab::Transcribe;
                    }
                });
            });
        }
    }

    fn settings_ui(&mut self, ui: &mut Ui, idle: bool) {
        ui.add_enabled_ui(idle, |ui| {
            egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Output folder");
                ui.horizontal(|ui| {
                    let mut s = self.cfg.output_dir.display().to_string();
                    if ui.add(egui::TextEdit::singleline(&mut s).desired_width(360.0)).changed() {
                        self.cfg.output_dir = PathBuf::from(s);
                    }
                    if ui.button("Browse…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().set_directory(&self.cfg.output_dir).pick_folder() {
                            self.cfg.output_dir = p;
                        }
                    }
                    if ui.button("📂").on_hover_text("Open").clicked() {
                        let _ = std::fs::create_dir_all(&self.cfg.output_dir);
                        crate::sysinfo::open_in_explorer(&self.cfg.output_dir);
                    }
                    let lib = self.cfg.output_dir.join(crate::transcribe::viewer::LIBRARY_FILE);
                    if ui
                        .button("📚 Library")
                        .on_hover_text("Browse all recordings in your browser")
                        .clicked()
                    {
                        let _ = crate::transcribe::viewer::write_library(&self.cfg.output_dir);
                        crate::sysinfo::open_in_explorer(&lib);
                    }
                });
                ui.end_row();

                ui.label("Session folder name");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.folder_template).desired_width(200.0));
                    ui.label(RichText::new("{date} {time} are replaced").small().color(DIM));
                });
                ui.end_row();

                ui.label("Format");
                egui::ComboBox::from_id_salt("fmt")
                    .selected_text(self.cfg.format.label())
                    .show_ui(ui, |ui| {
                        for f in [AudioFormat::Flac, AudioFormat::Opus, AudioFormat::Wav] {
                            ui.selectable_value(&mut self.cfg.format, f, f.label());
                        }
                    });
                ui.end_row();

                if self.cfg.format == AudioFormat::Opus {
                    ui.label("Opus bitrate");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("kbps")
                            .selected_text(format!("{} kbps", self.cfg.opus_kbps))
                            .show_ui(ui, |ui| {
                                for (k, note) in [
                                    (24, "very good speech"),
                                    (32, "near-transparent speech (recommended)"),
                                    (48, "transparent speech, good music"),
                                    (64, "transparent music"),
                                ] {
                                    ui.selectable_value(&mut self.cfg.opus_kbps, k, format!("{k} kbps — {note}"));
                                }
                            });
                        ui.label(
                            RichText::new(format!(
                                "≈ {} per hour per mono track",
                                fmt_bytes((self.cfg.opus_kbps as f64 * 1000.0 / 8.0 * 3600.0) as u64)
                            ))
                            .small()
                            .color(DIM),
                        );
                    });
                    ui.end_row();
                }

                let pcm = self.cfg.format != AudioFormat::Opus;
                ui.label("Bit depth");
                ui.add_enabled_ui(pcm, |ui| egui::ComboBox::from_id_salt("depth")
                    .selected_text(self.cfg.effective_depth().label())
                    .show_ui(ui, |ui| {
                        for d in [BitDepth::S16, BitDepth::S24, BitDepth::F32] {
                            let ok = !(d == BitDepth::F32 && self.cfg.format == AudioFormat::Flac);
                            ui.add_enabled_ui(ok, |ui| {
                                ui.selectable_value(&mut self.cfg.bit_depth, d, d.label());
                            });
                        }
                    }));
                ui.end_row();

                ui.label("Sample rate");
                ui.add_enabled_ui(pcm, |ui| egui::ComboBox::from_id_salt("rate")
                    .selected_text(format!("{} Hz", self.cfg.effective_rate()))
                    .show_ui(ui, |ui| {
                        for r in [16_000u32, 24_000, 32_000, 44_100, 48_000, 96_000] {
                            ui.selectable_value(&mut self.cfg.sample_rate, r, format!("{r} Hz"));
                        }
                    }));
                ui.end_row();

                ui.label("Files");
                ui.vertical(|ui| {
                    let mix_cost = fmt_bytes((self.cfg.bytes_per_sec_per_channel() * 2.0 * 3600.0) as u64);
                    ui.checkbox(
                        &mut self.cfg.write_mix,
                        format!("Also write a stereo mix (left = microphones, right = system audio) — +{mix_cost}/h"),
                    )
                    .on_hover_text("Redundant: any editor can rebuild it from the separate tracks. Handy for quick listening.");
                    ui.checkbox(&mut self.cfg.keep_loopback_stereo, "Keep system audio in stereo (otherwise mono)");
                });
                ui.end_row();

                ui.label("Reliability");
                let ka = self.cfg.keep_alive;
                ui.checkbox(&mut self.cfg.keep_alive, "Keep output devices awake during silence (recommended)")
                    .on_hover_text("Plays inaudible silence on recorded outputs so Windows keeps delivering timestamps. Gaps are filled automatically either way.");
                if ka != self.cfg.keep_alive {
                    self.sync_captures();
                }
                ui.end_row();

                ui.label("Hotkeys");
                ui.checkbox(&mut self.cfg.hotkeys_enabled, format!("Global hotkeys ({}) — applies after restart", super::hotkeys::HINT));
                ui.end_row();
            });
        });
        if !idle {
            ui.label(
                RichText::new("Settings are locked while recording.")
                    .small()
                    .color(DIM),
            );
        }
    }
}
