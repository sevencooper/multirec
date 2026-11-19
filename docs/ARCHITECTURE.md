# Architecture

## Goals that drive the design
1. **Tracks must stay sample-aligned for hours.** Each audio device has its own clock, and the clocks disagree by tens of ppm (about 0.1–0.4 s per hour). Loopback devices also stop sending data during silence.
2. **Never lose a recording.** Survive crashes, unplugged devices and full disks as gracefully as possible.
3. **Ship one self-contained `.exe`.**

## Threads and data flow

```
 ┌──────────────┐  Packet{key, rate, ch, f32 data, qpc_hns}
 │ capture thr. │──┐   (one per selected device, WASAPI event-driven,
 │  (mic)       │  │    gain + meter applied here)
 └──────────────┘  │
 ┌──────────────┐  ├──► crossbeam channel ──► ┌──────────────────────────────┐
 │ capture thr. │──┘                          │ session thread               │
 │ (loopback +  │                             │  per stream: Aligner → Sink  │
 │  silent      │                             │  mix queue → mix Sink        │
 │  keep-alive) │                             │  idle padding, flush, manifest│
 └──────────────┘                             └──────────────┬───────────────┘
        │ Arc<StreamShared> (meter, status, format, gain)    │ Arc<SessionShared> (SessionView)
        ▼                                                    ▼
 ┌─────────────────────────────── UI thread (egui) ─────────────────────────────┐
 │ reads meters/status/stats ~30×/s · sends Cmd {Start, Stop, TogglePause, Marker} │
 └──────────────────────────────────────────────────────────────────────────────┘
```

- **Capture threads** (`src/audio/wasapi.rs`) run whenever a device is *selected*, not only while recording, so the meters are live before you press Record. Each initializes COM as MTA, raises its thread priority, opens a shared-mode `IAudioClient` in 32-bit float at the device's mix rate, and waits on the buffer event with a 20 ms timeout, which also covers loopback endpoints that never signal. On error (unplug, format change) the thread reports `Lost` and retries once a second.
- **The session thread** (`src/recorder/session.rs`) owns all file I/O. It discards packets while idle. While recording it routes each packet to that stream's `Aligner`, then to its `Sink`, and feeds the stereo mix.
- **The UI thread** (`src/app/`) never blocks on audio. It only reads shared state and sends commands. Device enumeration runs on a short-lived helper thread so its COM apartment can't clash with the UI's.

## The master clock and the Aligner
`clock::now_hns()` is `QueryPerformanceCounter` converted to 100 ns units, the same conversion WASAPI uses for the `u64QPCPosition` it returns with every packet. Every packet therefore carries *when* its first frame was captured, on a timeline shared by all devices.

When recording starts, `t0 = now_hns()`. For a packet stamped `q`, its first frame belongs at output sample

```
expected = (q − t0) / 10⁷ × out_rate
```

The `Aligner` (`src/recorder/aligner.rs`) knows where that frame *would* land: frames already produced, plus input frames still buffered in the resampler divided by its current step. The difference `err = expected − landing` decides what happens:

| Situation | Action |
|---|---|
| First packet (not yet locked) | Place it exactly: insert silence or drop leading frames to the sample |
| `err > +30 ms` | Gap (loopback silence, glitch, device reconnect) → insert silence |
| `err < −30 ms` | Overlap (data from before `t0`, timestamp jump) → drop input frames |
| otherwise | PI controller nudges the resampling ratio, by at most ±0.2 % |

The PI controller (`KP = 0.3`, `KI = KP²/4`, critically damped, time constant of a few seconds) absorbs clock drift continuously, so there are no audible jumps. The smoothed correction is the drift figure shown in the UI and stored in `session.json`. A positive value means the device clock runs fast.

The **resampler** (`src/dsp/resampler.rs`) is a 32-tap Blackman-Harris windowed sinc with a 256-phase interpolated table. It is zero-phase (output sample *j* is the input interpolated at position *j·step*), so it adds no latency to account for, and at a 1:1 ratio it passes samples through bit-exactly. When downsampling, the cutoff drops to 0.97 × the output Nyquist.

**Idle streams:** a stream that has delivered no packets for 500 ms is padded with silence up to `now − 200 ms`, so files keep growing and the mix keeps flowing. If packets then arrive late, the overlap rule drops them. The silent keep-alive render stream on loopback devices makes this rare.

**Stop:** every stream is padded to the stop instant, its resampler tail is flushed, and then every file is padded with zeros to the longest one. All files end up with identical frame counts.

**Pause:** packets are ignored. On resume, `t0` shifts forward by the paused duration for every aligner, so the timeline stays continuous. Pause and resume are recorded as markers.

## Files
- **Sinks** (`src/recorder/writer.rs`): a `Sink` trait with WAV (`hound`), FLAC and Opus implementations, created from a `SinkSpec` (format, bit depth, rate, Opus bitrate). WAV headers are rewritten on `flush()` every 5 s, so a killed process leaves a valid file.
- **FLAC** (`src/recorder/flac.rs`): our own streaming encoder, about 300 lines:
  - fixed predictors of order 0–4, chosen per subframe;
  - partitioned Rice coding (partition order ≤ 6), with a verbatim fallback;
  - CRC-8/16, 4096-sample blocks.

  Frames are written as soon as they fill. STREAMINFO is patched with the total sample count on finish; 0 ("unknown") stays valid if that never happens. Pure Rust crates offered no streaming encoder that was crash-safe and light enough, so we wrote this one. Tests decode its output with `claxon` and compare bit-exactly.
- **Opus** (`src/recorder/opus.rs`): libopus (bundled, static) in VoIP mode with voice signal hint and DTX, in 20 ms frames, written to an Ogg Opus file with the pure-Rust `ogg` crate. Pages close about once a second for crash safety. The timing contract keeps Opus tracks the same length as every other format:
  - `pre_skip` in OpusHead equals the encoder lookahead, so decoders drop the startup delay;
  - the last packet is held back so the end-of-stream page can carry granule `pre_skip + frames_written`, so decoders trim the final frame's padding;
  - with DTX enabled the encoder still emits one (tiny) packet per frame, so granule positions stay exact.

  Opus only runs at 48 kHz, which `Config::effective_rate()` enforces. The transcriber decodes `.opus` with the same rules (`opus::decode`).
- **Quantization:** 16-bit output (WAV and FLAC) gets TPDF dither of ±1 LSB, except for exact zeros, so digital silence stays silent and FLAC still compresses it to almost nothing.
- **Mix:** optional (off by default); left = sum of input streams, right = sum of loopback streams, clamped. If only one kind exists, both channels carry it.
- **Manifest** (`src/recorder/manifest.rs`, `session.json`): written at start (`complete: false`), refreshed every 30 s, and finalized on stop. Fields:
  ```jsonc
  {
    "app": "MultiRec 0.1.0", "version": 1,
    "started_at": "RFC 3339 local time of sample 0",
    "duration_s": 3.02, "sample_rate": 48000, "format": "flac|opus|wav", "bit_depth": "16-bit | Opus 32 kbps",
    "aligned": true,
    "streams": [{ "label": "Me", "device_name": "...", "kind": "input|loopback", "file": "Me.wav",
                  "channels": 1, "native_rate": 48000, "native_channels": 2,
                  "silence_inserted_s": 0.0, "dropped_s": 0.0, "drift_ppm": 2.7 }],
    "mix_file": "mix.wav",
    "markers": [{ "t_s": 1.5, "label": "Marker 1" }],
    "complete": true
  }
  ```
  Bump `version` on breaking changes. The Transcribe tab reads this file.

## Transcription pipeline (`src/transcribe/`)
Runs on a worker thread. Progress goes through `Arc<Mutex<JobProgress>>`, and cancellation is checked between steps.

1. **Model:** downloaded on demand from Hugging Face into `%LOCALAPPDATA%\MultiRec\models` (`models.rs`) via a `.part` file and rename.
2. **Load** each stream as 16 kHz mono (`audio_io.rs`, streaming through the same resampler).
3. **VAD** (`plan_speech_batches`): energy-based, with a threshold of noise floor + 10 dB (minimum −55 dBFS) on 30 ms frames. Gaps under 0.6 s are merged, and regions get 0.3 s of padding. Regions are then batched into slices of 28 s or less, and longer regions are split at the quietest point. Whisper therefore never sees long silence (where it hallucinates), and every call fits in one 30 s window, where its timestamps are reliable.
4. **Whisper** (`engine_whisper.rs`): greedy decoding with no cross-window context. Word timing comes from **DTW token timestamps** (alignment-head presets per model). Words are assembled from raw token bytes, so multi-byte UTF-8 survives. A word ends at the next word's start, capped by a length-based estimate so pauses stay visible.
5. **Prosody** (`prosody.rs`), per word:
   - RMS loudness;
   - YIN pitch every 10 ms (60–450 Hz);
   - words whose loudest nearby 30 ms frame sits at the noise floor are removed as likely hallucinations.
6. **Bleed suppression** (`bleed.rs`): drops a mic word if a loopback track contains the same normalized word within ±1 s.
7. **Merge** (`merge.rs`):
   - Each speaker's words are grouped into utterances, split at gaps over 1.2 s or at long sentences.
   - Per utterance, with thresholds relative to that speaker's median:
     - contour from the pitch slope over the last 0.6 s (↗ above +2 semitones, ↘ below −3);
     - pace (`fast`/`slow` at ±30 %);
     - loudness (`loud`/`quiet`);
     - word emphasis (+5 dB or +4 semitones).
   - All speakers are interleaved by start time, and long gaps between turns are noted.
8. **Export** (`export.rs`): Markdown, JSON (full word-level data), SRT/VTT (combined and per speaker).

## Code map
| Path | Responsibility |
|---|---|
| `src/main.rs` | Window setup |
| `src/app/mod.rs` | App state, capture lifecycle (`sync_captures`), hotkeys, config autosave, toasts |
| `src/app/record_tab.rs` | Record tab, starting a recording (`start_recording`), settings |
| `src/app/transcribe_tab.rs` | Transcribe tab |
| `src/app/widgets.rs` | Meter, waveform, formatting helpers |
| `src/app/hotkeys.rs` | Global hotkeys (`global-hotkey`) |
| `src/audio/mod.rs` | Device and packet types, shared stream state, capture spawning |
| `src/audio/wasapi.rs` | All WASAPI/COM code |
| `src/audio/meter.rs` | Peak/RMS/hold/clip/waveform state |
| `src/dsp/resampler.rs` | Windowed-sinc resampler with drift correction |
| `src/recorder/aligner.rs` | Timeline alignment (the core of sync) |
| `src/recorder/session.rs` | Session thread, recording lifecycle, hardware test |
| `src/recorder/writer.rs`, `flac.rs`, `opus.rs` | File sinks, FLAC encoder, Ogg Opus writer/reader |
| `src/recorder/manifest.rs` | `session.json` |
| `src/transcribe/*` | Transcription pipeline (above) |
| `src/config.rs` | Persistent settings (`%APPDATA%\MultiRec\config.toml`), quality presets |
| `src/clock.rs`, `src/sysinfo.rs` | QPC clock; free disk space, opening Explorer |

## Known limitations / ideas
- Several remote speakers on one loopback track share one label. Diarization (speaker embeddings + clustering) would fix that.
- Global hotkeys are handled in the UI update loop. Behaviour while the window is *minimized* hasn't been verified.
- WAV files are limited to 4 GB (RF64 or auto-splitting would lift that). FLAC has no limit.
- The UI has no device hot-plug notifications; use **Refresh devices**. `IMMNotificationClient` could automate this.
- Prosody thresholds are tuned on synthetic signals and short tests, and should be validated on real conversations.
