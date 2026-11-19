use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    Wav,
    Flac,
    Opus,
}

impl AudioFormat {
    pub fn ext(self) -> &'static str {
        match self {
            AudioFormat::Wav => "wav",
            AudioFormat::Flac => "flac",
            AudioFormat::Opus => "opus",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            AudioFormat::Wav => "WAV (uncompressed)",
            AudioFormat::Flac => "FLAC (lossless)",
            AudioFormat::Opus => "Opus (compact, speech-tuned)",
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

/// One-click combinations of format settings. Presets never touch the mix or
/// stereo options, only format, bit depth, sample rate and Opus bitrate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Compact,
    Lossless,
    Studio,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Lossless, Preset::Compact, Preset::Studio];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Compact => "Compact speech",
            Preset::Lossless => "Lossless",
            Preset::Studio => "Studio",
            Preset::Custom => "Custom",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Preset::Compact => "Opus 32 kbps · ~14 MB/h per track · near-transparent for speech",
            Preset::Lossless => "FLAC 16-bit 48 kHz · ~100–200 MB/h per track · bit-perfect",
            Preset::Studio => {
                "WAV 24-bit 48 kHz · 518 MB/h per track · max headroom & compatibility"
            }
            Preset::Custom => "Your own combination",
        }
    }

    pub fn apply(self, c: &mut Config) {
        match self {
            Preset::Compact => {
                c.format = AudioFormat::Opus;
                c.opus_kbps = 32;
                c.sample_rate = 48_000;
            }
            Preset::Lossless => {
                c.format = AudioFormat::Flac;
                c.bit_depth = BitDepth::S16;
                c.sample_rate = 48_000;
            }
            Preset::Studio => {
                c.format = AudioFormat::Wav;
                c.bit_depth = BitDepth::S24;
                c.sample_rate = 48_000;
            }
            Preset::Custom => {}
        }
    }

    pub fn detect(c: &Config) -> Preset {
        match (
            c.format,
            c.effective_depth(),
            c.effective_rate(),
            c.opus_kbps,
        ) {
            (AudioFormat::Opus, _, _, 32) => Preset::Compact,
            (AudioFormat::Flac, BitDepth::S16, 48_000, _) => Preset::Lossless,
            (AudioFormat::Wav, BitDepth::S24, 48_000, _) => Preset::Studio,
            _ => Preset::Custom,
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
    /// Opus bitrate per channel (kbps).
    pub opus_kbps: u32,
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
            format: AudioFormat::Flac,
            bit_depth: BitDepth::S16,
            sample_rate: 48_000,
            opus_kbps: 32,
            write_mix: false,
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

    /// FLAC can't store floats; fall back to 24-bit. (Unused for Opus.)
    pub fn effective_depth(&self) -> BitDepth {
        if self.format == AudioFormat::Flac && self.bit_depth == BitDepth::F32 {
            BitDepth::S24
        } else {
            self.bit_depth
        }
    }

    /// Opus always records at 48 kHz.
    pub fn effective_rate(&self) -> u32 {
        if self.format == AudioFormat::Opus {
            48_000
        } else {
            self.sample_rate
        }
    }

    /// Approximate bytes per second for one channel of audio in the current format.
    pub fn bytes_per_sec_per_channel(&self) -> f64 {
        match self.format {
            AudioFormat::Opus => self.opus_kbps as f64 * 1000.0 / 8.0,
            AudioFormat::Wav => {
                self.effective_rate() as f64 * self.effective_depth().bytes() as f64
            }
            // Typical for speech; silence compresses to almost nothing.
            AudioFormat::Flac => {
                self.effective_rate() as f64 * self.effective_depth().bytes() as f64 * 0.55
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_round_trip() {
        for p in Preset::ALL {
            let mut c = Config::default();
            c.sample_rate = 44_100;
            c.bit_depth = BitDepth::F32;
            p.apply(&mut c);
            assert_eq!(Preset::detect(&c), p);
        }
        assert_eq!(Preset::detect(&Config::default()), Preset::Lossless);
        let mut c = Config::default();
        c.sample_rate = 16_000;
        assert_eq!(Preset::detect(&c), Preset::Custom);
    }
}
