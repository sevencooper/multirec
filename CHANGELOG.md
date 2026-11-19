# Changelog

All notable user-visible changes. Format: [Keep a Changelog](https://keepachangelog.com/), versioning: [SemVer](https://semver.org/).

## [Unreleased]

## [0.3.0] - 2026-11-19
### Added
- **GPU transcription** via Vulkan (NVIDIA, AMD, Intel), on by default with automatic CPU fallback. On an RTX-class card a 25-minute call transcribes in under a minute with `large-v3-turbo` (about 100× faster than CPU).
- **Interactive session viewer** (`viewer.html`, one offline file per session):
  - plays and scrubs the real audio, with waveform lanes per speaker, markers and zoom;
  - per-speaker volume, mute and solo, plus playback speed;
  - a word-by-word synced transcript with speaker notes; click to jump;
  - search, a speaker filter, conversation and speaker statistics, shareable time links and keyboard shortcuts.
- A **library page** (`index.html`) in the recordings folder lists all sessions; "Viewer" and "Library" buttons in the app.
- "Use GPU" option in the Transcribe tab; the job log shows the device used and timings.
- whisper.cpp diagnostics are saved to `%LOCALAPPDATA%\MultiRec\whisper.log`.

### Changed
- The Whisper state is reused across chunks (faster on CPU and GPU).
- The exe is larger (~65 MB) because it embeds the GPU shaders. The app still starts and records on machines without a Vulkan driver.

## [0.2.1] - 2026-11-19
### Fixed
- **Transcription crashed the app instantly on CPUs without AVX-512** (most consumer CPUs). whisper.cpp was compiled for the CI build machine's CPU; it now targets a portable AVX2 baseline, and CI refuses to publish builds containing AVX-512 code.

## [0.2.0] - 2026-11-19
### Added
- **Opus** output format (Ogg Opus, speech-tuned VoIP mode with DTX): ~14 MB per hour per track at 32 kbps, with exact length and alignment kept. The transcriber reads `.opus`.
- **Quality presets** above the sources: Lossless (FLAC 16-bit), Compact speech (Opus 32 kbps), Studio (WAV 24-bit).
- TPDF dither for 16-bit output (silence stays exactly zero).
- More sample rates (24 and 32 kHz); size per hour shown for the mix and for each Opus bitrate.

### Changed
- New installs default to **FLAC 16-bit** with the **stereo mix off** (about 85–90% smaller than before). Existing settings are kept.

## [0.1.0] - 2026-11-19
### Added
- Simultaneous recording of any number of microphones and output devices (WASAPI loopback).
- Sample-aligned output: QPC-timestamped capture, gap filling, drift correction via PI-controlled resampling; all files have identical length.
- WAV (16/24-bit, 32-bit float) and FLAC output; crash-safe periodic flushing; optional stereo mix (L = mics, R = system audio); `session.json` manifest.
- Live meters (RMS/peak/hold/clip), waveforms, file sizes, size per hour, free disk space and remaining time, drift/sync/gap statistics.
- Pause/resume, markers, global hotkeys (Ctrl+Alt+R/P/M), automatic reconnect of unplugged devices.
- Local transcription (whisper.cpp) per speaker with DTW word timing, VAD, prosody annotations (intonation, emphasis, pauses, pace, loudness), speaker-bleed removal; Markdown/JSON/SRT/VTT export.
