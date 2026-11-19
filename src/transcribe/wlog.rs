//! Captures whisper.cpp / ggml log output: everything goes to
//! `%LOCALAPPDATA%\MultiRec\whisper.log` (useful after a crash), and lines about
//! the compute device are kept for the job log in the UI.

use parking_lot::Mutex;
use std::io::Write;
use std::sync::Once;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
static NOTABLE: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Logger;

fn notable(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    (m.contains("using") && m.contains("backend"))
        || m.contains("no gpu found")
        || m.contains("error")
        || m.contains("failed")
}

/// Turn whisper.cpp's device lines into one plain sentence for the job log.
pub fn describe(line: &str) -> String {
    let l = line.to_ascii_lowercase();
    if l.contains("no gpu found") {
        "No usable GPU found: running on the CPU".into()
    } else if let Some(i) = l.find("using ") {
        let dev = line[i + 6..].trim().trim_end_matches(" backend");
        format!("Running on the GPU ({dev})")
    } else {
        format!("whisper: {line}")
    }
}

impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        let msg = record.args().to_string();
        let msg = msg.trim_end();
        if msg.is_empty() {
            return;
        }
        if let Some(f) = FILE.lock().as_mut() {
            let _ = writeln!(f, "[{}] {}", record.level(), msg);
        }
        if notable(msg) {
            NOTABLE.lock().push(msg.to_string());
        }
    }
    fn flush(&self) {
        if let Some(f) = FILE.lock().as_mut() {
            let _ = f.flush();
        }
    }
}

static LOGGER: Logger = Logger;

pub fn log_path() -> std::path::PathBuf {
    super::models::dir()
        .parent()
        .map(|p| p.join("whisper.log"))
        .unwrap_or_else(|| "whisper.log".into())
}

/// Install the hooks once and start a fresh log file for this job.
pub fn start_job() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
        whisper_rs::install_logging_hooks();
    });
    let p = log_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    *FILE.lock() = std::fs::File::create(p).ok();
    NOTABLE.lock().clear();
}

pub fn take_notable() -> Vec<String> {
    std::mem::take(&mut *NOTABLE.lock())
}
