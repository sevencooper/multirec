#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod clock;
mod config;
mod dsp;
mod recorder;
mod sysinfo;
mod transcribe;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("MultiRec")
            .with_inner_size([1000.0, 760.0])
            .with_min_inner_size([760.0, 540.0])
            .with_icon(app::make_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "MultiRec",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
