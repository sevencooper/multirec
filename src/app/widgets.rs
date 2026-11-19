use crate::audio::meter::MeterState;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use std::time::{Duration, Instant};

pub const RED: Color32 = Color32::from_rgb(230, 70, 60);
pub const AMBER: Color32 = Color32::from_rgb(235, 175, 50);
pub const GREEN: Color32 = Color32::from_rgb(80, 200, 120);
pub const DIM: Color32 = Color32::from_rgb(130, 130, 140);

const DB_MIN: f32 = -60.0;

pub fn db(x: f32) -> f32 {
    20.0 * x.max(1e-6).log10()
}

fn norm(x: f32) -> f32 {
    ((db(x) - DB_MIN) / -DB_MIN).clamp(0.0, 1.0)
}

fn level_color(dbv: f32) -> Color32 {
    if dbv > -3.0 {
        RED
    } else if dbv > -12.0 {
        AMBER
    } else {
        GREEN
    }
}

/// Horizontal level meter: RMS fill, peak bar, hold tick, dB scale.
pub fn level_meter(ui: &mut Ui, m: &MeterState, width: f32) {
    let h = 16.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_rgb(28, 28, 32));
    let stale = m
        .last_update
        .is_none_or(|t| t.elapsed() > Duration::from_millis(500));
    let (peak, rms, hold) = if stale {
        (0.0, 0.0, 0.0)
    } else {
        (m.peak, m.rms, m.hold)
    };
    let x = |v: f32| rect.left() + rect.width() * norm(v);
    // Peak (lighter) under RMS (solid).
    let peak_rect = Rect::from_min_max(rect.min, Pos2::new(x(peak), rect.max.y));
    p.rect_filled(peak_rect, 3.0, level_color(db(peak)).gamma_multiply(0.35));
    let rms_rect = Rect::from_min_max(
        Pos2::new(rect.left(), rect.top() + 3.0),
        Pos2::new(x(rms), rect.bottom() - 3.0),
    );
    p.rect_filled(rms_rect, 2.0, level_color(db(rms)));
    if hold > 0.0 {
        let hx = x(hold);
        p.line_segment(
            [Pos2::new(hx, rect.top()), Pos2::new(hx, rect.bottom())],
            Stroke::new(2.0_f32, Color32::WHITE),
        );
    }
    for mark in [-48.0f32, -36.0, -24.0, -12.0, -6.0, -3.0] {
        let mx = rect.left() + rect.width() * ((mark - DB_MIN) / -DB_MIN);
        p.line_segment(
            [
                Pos2::new(mx, rect.bottom() - 4.0),
                Pos2::new(mx, rect.bottom()),
            ],
            Stroke::new(1.0_f32, Color32::from_gray(90)),
        );
    }
}

pub fn clip_led(ui: &mut Ui, m: &MeterState) {
    let recent = m
        .last_clip
        .is_some_and(|t| t.elapsed() < Duration::from_secs(2));
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
    let c = if recent {
        RED
    } else {
        Color32::from_rgb(60, 30, 30)
    };
    ui.painter().circle_filled(rect.center(), 6.0, c);
    resp.on_hover_text(format!("Clipping events: {}", m.clip_count));
}

/// Scrolling min/max waveform of the last 5 seconds.
pub fn waveform(ui: &mut Ui, m: &MeterState, width: f32, height: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_rgb(22, 22, 26));
    let mid = rect.center().y;
    p.line_segment(
        [Pos2::new(rect.left(), mid), Pos2::new(rect.right(), mid)],
        Stroke::new(1.0_f32, Color32::from_gray(45)),
    );
    let n = crate::audio::meter::WAVE_BUCKETS;
    let dx = rect.width() / n as f32;
    let off = n.saturating_sub(m.wave.len());
    let half = rect.height() * 0.5 - 1.0;
    for (i, &(lo, hi)) in m.wave.iter().enumerate() {
        let xx = rect.left() + (off + i) as f32 * dx;
        // Mild compression so speech is visible without clipping the display.
        let f = |v: f32| v.signum() * v.abs().sqrt();
        let y0 = mid - f(hi).clamp(-1.0, 1.0) * half;
        let y1 = mid - f(lo).clamp(-1.0, 1.0) * half;
        p.line_segment(
            [Pos2::new(xx, y0), Pos2::new(xx, y1.max(y0 + 1.0))],
            Stroke::new(dx.max(1.0), color),
        );
    }
}

pub fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

pub fn fmt_dur(s: f64) -> String {
    let t = s.max(0.0);
    let whole = t as u64;
    format!(
        "{:02}:{:02}:{:02}.{}",
        whole / 3600,
        (whole / 60) % 60,
        whole % 60,
        ((t.fract()) * 10.0) as u32
    )
}

pub fn fmt_hm(s: f64) -> String {
    let m = (s.max(0.0) / 60.0) as u64;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub fn section_title(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).strong().size(15.0));
}
