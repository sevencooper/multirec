# Changelog

All notable user-visible changes. Format: [Keep a Changelog](https://keepachangelog.com/), versioning: [SemVer](https://semver.org/).

## [Unreleased]
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
