use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    Wav,
    Flac,
}

impl AudioFormat {
    pub fn ext(self) -> &'static str {
        match self {
            AudioFormat::Wav => "wav",
            AudioFormat::Flac => "flac",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            AudioFormat::Wav => "WAV",
            AudioFormat::Flac => "FLAC (lossless, ~50% size)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitDepth {
    S16,
    S24,
    F32,
}

impl BitDepth {
    pub fn bytes(self) -> u32 {
        match self {
            BitDepth::S16 => 2,
            BitDepth::S24 => 3,
            BitDepth::F32 => 4,
        }
    }
    pub fn bits(self) -> u32 {
        self.bytes() * 8
    }
    pub fn label(self) -> &'static str {
        match self {
            BitDepth::S16 => "16-bit",
            BitDepth::S24 => "24-bit",
            BitDepth::F32 => "32-bit float",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscribeConfig {
    pub model: String,
    pub language: String,
    pub threads: usize,
    pub bleed_suppression: bool,
    pub prosody: bool,
}

impl Default for TranscribeConfig {
    fn default() -> Self {
        Self {
            model: "small".into(),
            language: "auto".into(),
            threads: std::thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(4),
            bleed_suppression: true,
            prosody: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub initialized: bool,
    pub output_dir: PathBuf,
    /// Session folder name; `{date}` and `{time}` are substituted.
    pub folder_template: String,
    pub format: AudioFormat,
    pub bit_depth: BitDepth,
    pub sample_rate: u32,
    pub write_mix: bool,
    pub keep_loopback_stereo: bool,
    /// Play an inaudible stream on loopback devices so they deliver data during silence.
    pub keep_alive: bool,
    pub hotkeys_enabled: bool,
    /// Device keys ("in:<id>" / "lb:<id>") selected for recording.
    pub selected: Vec<String>,
    pub labels: HashMap<String, String>,
    pub gains_db: HashMap<String, f32>,
    pub transcribe: TranscribeConfig,
}

impl Default for Config {
    fn default() -> Self {
        let base = dirs::audio_dir()
            .or_else(dirs::document_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            initialized: false,
            output_dir: base.join("MultiRec"),
            folder_template: "{date}_{time}".into(),
            format: AudioFormat::Wav,
            bit_depth: BitDepth::S24,
            sample_rate: 48_000,
            write_mix: true,
            keep_loopback_stereo: false,
            keep_alive: true,
            hotkeys_enabled: true,
            selected: Vec::new(),
            labels: HashMap::new(),
            gains_db: HashMap::new(),
            transcribe: TranscribeConfig::default(),
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("MultiRec").join("config.toml"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path().ok_or_else(|| anyhow::anyhow!("no config directory"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// FLAC can't store floats; fall back to 24-bit.
    pub fn effective_depth(&self) -> BitDepth {
        if self.format == AudioFormat::Flac && self.bit_depth == BitDepth::F32 {
            BitDepth::S24
        } else {
            self.bit_depth
        }
    }
}
