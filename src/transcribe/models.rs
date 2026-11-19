use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;

pub struct ModelInfo {
    pub id: &'static str,
    pub file: &'static str,
    pub size_mb: u32,
    pub desc: &'static str,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "tiny",
        file: "ggml-tiny.bin",
        size_mb: 75,
        desc: "fastest, rough",
    },
    ModelInfo {
        id: "base",
        file: "ggml-base.bin",
        size_mb: 142,
        desc: "fast",
    },
    ModelInfo {
        id: "small",
        file: "ggml-small.bin",
        size_mb: 466,
        desc: "balanced",
    },
    ModelInfo {
        id: "medium",
        file: "ggml-medium.bin",
        size_mb: 1500,
        desc: "accurate, slow on CPU",
    },
    ModelInfo {
        id: "large-v3-turbo-q5_0",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size_mb: 547,
        desc: "best accuracy per MB",
    },
    ModelInfo {
        id: "large-v3-turbo",
        file: "ggml-large-v3-turbo.bin",
        size_mb: 1620,
        desc: "best accuracy",
    },
    ModelInfo {
        id: "tiny.en",
        file: "ggml-tiny.en.bin",
        size_mb: 75,
        desc: "English only",
    },
    ModelInfo {
        id: "base.en",
        file: "ggml-base.en.bin",
        size_mb: 142,
        desc: "English only",
    },
    ModelInfo {
        id: "small.en",
        file: "ggml-small.en.bin",
        size_mb: 466,
        desc: "English only",
    },
    ModelInfo {
        id: "medium.en",
        file: "ggml-medium.en.bin",
        size_mb: 1500,
        desc: "English only",
    },
];

const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

pub fn find(id: &str) -> Result<&'static ModelInfo> {
    MODELS
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| anyhow!("unknown model {id}"))
}

pub fn dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MultiRec")
        .join("models")
}

pub fn path(m: &ModelInfo) -> PathBuf {
    dir().join(m.file)
}

pub fn is_downloaded(m: &ModelInfo) -> bool {
    std::fs::metadata(path(m))
        .map(|md| md.len() > 1_000_000)
        .unwrap_or(false)
}

/// Download the model if needed. `progress(done, total)` returns false to cancel.
pub fn ensure(
    m: &ModelInfo,
    mut progress: impl FnMut(u64, Option<u64>) -> bool,
) -> Result<PathBuf> {
    let dest = path(m);
    if is_downloaded(m) {
        return Ok(dest);
    }
    std::fs::create_dir_all(dir())?;
    let part = dest.with_extension("part");
    let resp = ureq::get(&format!("{BASE_URL}{}", m.file))
        .call()
        .with_context(|| format!("downloading {}", m.file))?;
    let total = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok());
    let mut reader = resp.into_reader();
    let mut f = std::io::BufWriter::new(std::fs::File::create(&part)?);
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])?;
        done += n as u64;
        if !progress(done, total) {
            drop(f);
            let _ = std::fs::remove_file(&part);
            bail!("download cancelled");
        }
    }
    f.flush()?;
    drop(f);
    if let Some(t) = total {
        if done != t {
            let _ = std::fs::remove_file(&part);
            bail!("download incomplete ({done} of {t} bytes)");
        }
    }
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}
