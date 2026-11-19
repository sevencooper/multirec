use crate::audio::DeviceKind;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MANIFEST_FILE: &str = "session.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Marker {
    pub t_s: f64,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestStream {
    pub label: String,
    pub device_name: String,
    pub kind: DeviceKind,
    pub file: String,
    pub channels: u16,
    #[serde(default)]
    pub native_rate: u32,
    #[serde(default)]
    pub native_channels: u16,
    #[serde(default)]
    pub silence_inserted_s: f64,
    #[serde(default)]
    pub dropped_s: f64,
    #[serde(default)]
    pub drift_ppm: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub app: String,
    pub version: u32,
    /// Local wall-clock time of sample 0 (RFC 3339).
    pub started_at: String,
    pub duration_s: f64,
    pub sample_rate: u32,
    pub format: String,
    pub bit_depth: String,
    /// All files start at the same instant and have identical length.
    pub aligned: bool,
    pub streams: Vec<ManifestStream>,
    pub mix_file: Option<String>,
    pub markers: Vec<Marker>,
    /// False while recording (or if the app crashed mid-recording).
    pub complete: bool,
}

impl Manifest {
    pub fn save(&self, dir: &Path) -> Result<()> {
        let tmp = dir.join("session.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, dir.join(MANIFEST_FILE))?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let p = dir.join(MANIFEST_FILE);
        let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        serde_json::from_str(&s).with_context(|| format!("parsing {}", p.display()))
    }
}
