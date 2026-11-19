mod hotkeys;
mod record_tab;
mod transcribe_tab;
mod widgets;

use crate::audio::{self, CaptureHandle, DeviceInfo, DeviceKind, Packet, StreamShared};
use crate::config::Config;
use crate::recorder::session::{self, Cmd, Phase, SessionShared, SessionView};
use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;
use hotkeys::HotkeyAction;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use widgets::Toast;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Record,
    Transcribe,
}

struct Capture {
    _handle: CaptureHandle,
    shared: Arc<StreamShared>,
    keep_alive: bool,
}

pub struct App {
    cfg: Config,
    saved_cfg: String,
    last_cfg_check: Instant,
    devices: Vec<DeviceInfo>,
    captures: HashMap<String, Capture>,
    pkt_tx: Sender<Packet>,
    cmd_tx: Sender<Cmd>,
    session: Arc<SessionShared>,
    session_thread: Option<JoinHandle<()>>,
    view: SessionView,
    tab: Tab,
    _hotkeys: Option<hotkeys::Hotkeys>,
    hk_rx: Receiver<HotkeyAction>,
    tr: transcribe_tab::TranscribeUi,
    disk_free: Option<u64>,
    disk_checked: Option<Instant>,
    toast: Option<Toast>,
    seen_result: bool,
}

/// Window icon: a red record dot.
pub fn make_icon() -> egui::IconData {
    let n = 64u32;
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let i = ((y * n + x) * 4) as usize;
            if d < 30.0 {
                let (r, g, b) = if d < 22.0 {
                    (228, 60, 52)
                } else {
                    (50, 50, 58)
                };
                let a = ((30.0 - d).clamp(0.0, 1.0) * 255.0) as u8;
                rgba[i..i + 4].copy_from_slice(&[r, g, b, a]);
            }
        }
    }
    egui::IconData {
        rgba,
        width: n,
        height: n,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = Config::load();
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.button_padding = egui::vec2(10.0, 5.0);
        });
        let (pkt_tx, pkt_rx) = unbounded();
        let (cmd_tx, session, th) = session::spawn(pkt_rx);
        let (hk_tx, hk_rx) = unbounded();
        let mut toast = None;
        let hk = if cfg.hotkeys_enabled {
            match hotkeys::setup(cc.egui_ctx.clone(), hk_tx) {
                Ok(h) => Some(h),
                Err(e) => {
                    toast = Some(Toast {
                        text: format!("Global hotkeys unavailable: {e}"),
                        error: true,
                        at: Instant::now(),
                    });
                    None
                }
            }
        } else {
            None
        };
        let mut app = Self {
            saved_cfg: cfg.to_toml(),
            cfg,
            last_cfg_check: Instant::now(),
            devices: Vec::new(),
            captures: HashMap::new(),
            pkt_tx,
            cmd_tx,
            session,
            session_thread: Some(th),
            view: SessionView::default(),
            tab: Tab::Record,
            _hotkeys: hk,
            hk_rx,
            tr: transcribe_tab::TranscribeUi::default(),
            disk_free: None,
            disk_checked: None,
            toast,
            seen_result: true,
        };
        app.refresh_devices();
        if !app.cfg.initialized {
            // First run: default mic + default speakers.
            for kind in [DeviceKind::Input, DeviceKind::Loopback] {
                if let Some(d) = app.devices.iter().find(|d| d.kind == kind && d.is_default) {
                    app.cfg.selected.push(d.key.clone());
                    app.cfg.labels.insert(
                        d.key.clone(),
                        if kind == DeviceKind::Input {
                            "Me".into()
                        } else {
                            "Others".into()
                        },
                    );
                }
            }
            app.cfg.initialized = true;
        }
        app.sync_captures();
        app
    }

    fn idle(&self) -> bool {
        self.view.phase == Phase::Idle
    }

    fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            error,
            at: Instant::now(),
        });
    }

    fn refresh_devices(&mut self) {
        match audio::list_devices() {
            Ok(d) => self.devices = d,
            Err(e) => self.notify(format!("Could not list audio devices: {e}"), true),
        }
    }

    /// Start/stop capture threads to match the current selection (monitoring runs
    /// even when not recording so meters are live).
    fn sync_captures(&mut self) {
        if !self.idle() {
            return;
        }
        let want: HashSet<String> = self
            .cfg
            .selected
            .iter()
            .filter(|k| self.devices.iter().any(|d| &d.key == *k))
            .cloned()
            .collect();
        let ka = self.cfg.keep_alive;
        self.captures
            .retain(|k, c| want.contains(k) && (!k.starts_with("lb:") || c.keep_alive == ka));
        for k in want {
            if self.captures.contains_key(&k) {
                continue;
            }
            let Some(dev) = self.devices.iter().find(|d| d.key == k) else {
                continue;
            };
            let shared = Arc::new(StreamShared::new());
            shared.set_gain_db(*self.cfg.gains_db.get(&k).unwrap_or(&0.0));
            let handle = audio::spawn_capture(dev, shared.clone(), self.pkt_tx.clone(), ka);
            self.captures.insert(
                k,
                Capture {
                    _handle: handle,
                    shared,
                    keep_alive: ka,
                },
            );
        }
    }

    fn cmd(&self, c: Cmd) {
        let _ = self.cmd_tx.send(c);
    }

    fn handle_hotkeys(&mut self) {
        while let Ok(a) = self.hk_rx.try_recv() {
            match a {
                HotkeyAction::StartStop => {
                    if self.idle() {
                        self.start_recording();
                    } else {
                        self.cmd(Cmd::Stop);
                    }
                }
                HotkeyAction::Pause if !self.idle() => self.cmd(Cmd::TogglePause),
                HotkeyAction::Marker if !self.idle() => self.cmd(Cmd::Marker),
                _ => {}
            }
        }
    }

    fn maybe_save_config(&mut self) {
        if self.last_cfg_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_cfg_check = Instant::now();
        let s = self.cfg.to_toml();
        if s != self.saved_cfg {
            if let Err(e) = self.cfg.save() {
                self.notify(format!("Could not save settings: {e}"), true);
            }
            self.saved_cfg = s;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let prev_phase = self.view.phase;
        self.view = self.session.view.lock().clone();
        if prev_phase != Phase::Idle && self.view.phase == Phase::Idle {
            self.seen_result = false;
            // Selection may have changed while recording was locked.
            self.sync_captures();
        }
        if !self.seen_result {
            if let Some(r) = self.view.last_result.clone() {
                self.seen_result = true;
                match r {
                    Ok(dir) => {
                        self.tr.session_dir = dir.display().to_string();
                        self.notify(format!("Saved to {}", dir.display()), false);
                    }
                    Err(e) => self.notify(e, true),
                }
            }
        }
        self.handle_hotkeys();

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("⏺ MultiRec")
                        .strong()
                        .size(18.0)
                        .color(widgets::RED),
                );
                ui.separator();
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Record,
                    egui::RichText::new("Record").size(15.0),
                );
                ui.selectable_value(
                    &mut self.tab,
                    Tab::Transcribe,
                    egui::RichText::new("Transcribe").size(15.0),
                );
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| match self.view.phase {
                        Phase::Recording => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "● REC {}",
                                    widgets::fmt_dur(self.view.elapsed_s)
                                ))
                                .color(widgets::RED)
                                .strong(),
                            );
                        }
                        Phase::Paused => {
                            ui.label(
                                egui::RichText::new("⏸ PAUSED")
                                    .color(widgets::AMBER)
                                    .strong(),
                            );
                        }
                        Phase::Idle => {}
                    },
                );
            });
            ui.add_space(2.0);
        });

        let toast_visible = self
            .toast
            .as_ref()
            .is_some_and(|t| t.at.elapsed() < Duration::from_secs(12));
        if toast_visible {
            egui::TopBottomPanel::bottom("toast").show(ctx, |ui| {
                let (text, error) = self
                    .toast
                    .as_ref()
                    .map(|t| (t.text.clone(), t.error))
                    .unwrap();
                ui.horizontal(|ui| {
                    let c = if error { widgets::RED } else { widgets::GREEN };
                    ui.colored_label(c, text);
                    if ui.small_button("✕").clicked() {
                        self.toast = None;
                    }
                });
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Record => self.record_ui(ui),
            Tab::Transcribe => self.transcribe_ui(ui),
        });

        self.maybe_save_config();
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Finalise any running recording before the process exits.
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Some(t) = self.session_thread.take() {
            let _ = t.join();
        }
        self.captures.clear();
        let _ = self.cfg.save();
    }
}
