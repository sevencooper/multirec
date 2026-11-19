# Changelog

All notable user-visible changes. Format: [Keep a Changelog](https://keepachangelog.com/), versioning: [SemVer](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-11-19
### Added
- Simultaneous recording of any number of microphones and output devices (WASAPI loopback).
- Sample-aligned output: QPC-timestamped capture, gap filling, drift correction via PI-controlled resampling; all files have identical length.
- WAV (16/24-bit, 32-bit float) and FLAC output; crash-safe periodic flushing; optional stereo mix (L = mics, R = system audio); `session.json` manifest.
- Live meters (RMS/peak/hold/clip), waveforms, file sizes, size per hour, free disk space and remaining time, drift/sync/gap statistics.
- Pause/resume, markers, global hotkeys (Ctrl+Alt+R/P/M), automatic reconnect of unplugged devices.
- Local transcription (whisper.cpp) per speaker with DTW word timing, VAD, prosody annotations (intonation, emphasis, pauses, pace, loudness), speaker-bleed removal; Markdown/JSON/SRT/VTT export.
