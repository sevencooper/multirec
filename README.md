# MultiRec

**Record your microphone and your computer's sound at the same time, as separate, perfectly synchronized tracks, then turn them into a speaker-labelled transcript.**

MultiRec is a small Windows app. A typical use is a call: your voice goes to one file, everyone else (whatever plays through your speakers or headset) goes to another. The files start at the same instant and stay in sync to within a fraction of a millisecond, so you can drop them into any audio editor and they line up.

- One portable `.exe`. No installer, no runtime dependencies.
- Lossless FLAC by default, or compact Opus at ~14 MB per hour per track.
- Records any number of microphones and output devices at once.
- Live meters, waveforms, file sizes, disk-space estimate and sync statistics.
- Optional transcription that runs entirely on your PC, labels each speaker, and keeps the *how* as well as the *what*: pauses, emphasis, rising intonation, pace.

> **Platform:** Windows 10 (1703+) or Windows 11, 64-bit, on a CPU with AVX2 (any Intel or AMD desktop/laptop CPU from roughly 2015 on).

---

## Download

Get `multirec.exe` from the [**latest release**](../../releases/latest) and run it. Windows SmartScreen may warn about an unsigned app: click **More info → Run anyway**.

Prefer to build it yourself? See [CONTRIBUTING.md](CONTRIBUTING.md).

## Quick start

1. Start `multirec.exe`. On first run it pre-selects your default microphone (labelled **Me**) and your default speakers (labelled **Others**).
2. Check that the meters move when you talk and when audio plays.
3. Click **⏺ Record**, then **⏹ Stop** when you're done.
4. Click **📂 Open folder** to see your files, or **📝 Transcribe…** to get a transcript.

> **Headsets with several outputs** (for example "Game" and "Chat" channels): calls usually play on the *Chat* device. Tick the output that your call app actually uses, or tick both; each becomes its own track.

## Recording

### Sources
- **🎤 Microphones / inputs:** anything Windows lists as a recording device.
- **🔊 Speakers / outputs:** anything Windows lists as a playback device. MultiRec records *what that device plays* (WASAPI loopback). Nothing is muted or rerouted.

Each selected source has:
- **Label:** the speaker name used for the file name and in transcripts.
- **Gain:** ±24 dB, applied before recording.
- **Live meter:** RMS (solid), peak (light), 1.5 s peak hold (white tick) and a clip light.
- **5-second waveform.**
- **Status line:**
  - device format → recording format
  - file size
  - **drift:** how fast that device's clock runs, in parts per million
  - **sync err:** current alignment error in ms
  - **gap-filled / dropped:** silence inserted or data discarded to keep sync

### Controls
| Action | Button | Global hotkey (works while another app has focus) |
|---|---|---|
| Record / Stop | ⏺ / ⏹ | **Ctrl+Alt+R** |
| Pause / Resume | ⏸ / ▶ | **Ctrl+Alt+P** |
| Drop a marker | 🚩 | **Ctrl+Alt+M** |

Markers and pauses are saved with the session and appear in transcripts.

### Choosing a quality
The **Quality** row above the sources switches between three presets:

| Preset | Format | Per mono track per hour | Best for |
|---|---|---|---|
| **Lossless** (default) | FLAC, 16-bit, 48 kHz | ~100–200 MB (silence costs almost nothing) | Archiving, editing, anything you might process later |
| **Compact speech** | Opus 32 kbps | ~14 MB | Long calls, meetings, keeping years of recordings |
| **Studio** | WAV, 24-bit, 48 kHz | 518 MB | Maximum headroom and compatibility with every tool |

Why these are safe for speech:
- **16-bit** gives 96 dB of dynamic range, far more than a voice needs. MultiRec adds proper dither, so quiet passages stay clean.
- **FLAC** is lossless: it decodes to exactly the same samples, just smaller.
- **Opus** is the codec built for voice calls. At 32 kbps speech is near-transparent, and in our tests Whisper produced word-for-word identical transcripts from Opus and WAV.

"Custom" appears when you change individual settings.

### What you get
Each recording creates a folder such as `Music\MultiRec\2026-01-31_14-05-09\`:

| File | Contents |
|---|---|
| `Me.flac`, `Others.flac`, … | One file per source (extension follows the format), mono by default |
| `mix.flac` | *Optional* stereo quick-listen file: **left = microphones, right = system audio** |
| `session.json` | Start time, devices, sample rate, measured drift, gap statistics, markers |
| `transcript.*` | Added by the Transcribe tab (see below) |

**All files have exactly the same length and start at the same instant**, in every format. To edit, import them together at position 0.

### Settings (Record tab → *Output settings*)
| Setting | Default | Notes |
|---|---|---|
| Output folder | `Music\MultiRec` | |
| Session folder name | `{date}_{time}` | |
| Format | FLAC | FLAC (lossless), Opus (compact) or WAV (uncompressed) |
| Opus bitrate | 32 kbps | 24 / 32 / 48 / 64 kbps per channel; Opus only |
| Bit depth | 16-bit | 16-bit, 24-bit or 32-bit float (WAV only); not used for Opus |
| Sample rate | 48 kHz | Devices at other rates are converted. Opus always uses 48 kHz. |
| Stereo mix | off | Writes `mix.*`. It's redundant (any editor can rebuild it) and doubles the size of a two-track session. |
| System audio in stereo | off | Mono halves the file size; voices don't need stereo |
| Keep output devices awake | on | Plays inaudible silence so Windows keeps delivering audio during quiet moments. Sync is maintained either way. |
| Global hotkeys | on | Takes effect after restart |

Settings are saved automatically to `%APPDATA%\MultiRec\config.toml`. Existing installs keep their previous settings; pick a preset to switch.

### Reliability
- **Crash-safe:** files are made readable every 5 seconds. If the app or PC dies, everything up to that point plays fine.
- **Unplugged device:** that track continues with silence, a warning appears, and MultiRec reconnects automatically when the device returns. The other tracks are unaffected.
- **File size limit:** a WAV file can be at most 4 GB, which is about 15 hours of 24-bit 48 kHz mono. FLAC and Opus have no such limit.

## Transcription

The **Transcribe** tab turns a recording session into text using [Whisper](https://github.com/openai/whisper) (via whisper.cpp), running locally on your CPU.

1. Choose the session folder. It's filled in automatically after a recording.
2. Choose a **model**. It downloads once to `%LOCALAPPDATA%\MultiRec\models`.

   | Model | Size | Use when |
   |---|---|---|
   | `small` (default) | 466 MB | good balance |
   | `large-v3-turbo-q5_0` | 547 MB | best accuracy for the size |
   | `base` / `tiny` | 142 / 75 MB | quick drafts |
   | `*.en` | — | English-only variants; slightly better for English |

3. Choose the **language** (or auto-detect) and click **▶ Transcribe**.

Because every speaker is on their own track, attribution is exact. No guessing who said what.

### Output files
| File | Contents |
|---|---|
| `transcript.md` | Readable, timestamped conversation with annotations |
| `transcript.json` | Everything, per word: timing, confidence, pitch, loudness. For further processing. |
| `transcript.srt`, `transcript.vtt` | Subtitles for the whole conversation |
| `transcript_<Label>.srt` | Subtitles per speaker |

### Annotations in `transcript.md`
```
**[00:00:05] Others:** Yes, I can hear you **perfectly**. What did you want to talk about … today? ↗
```
| Mark | Meaning |
|---|---|
| ↗ / ↘ | Rising (question-like) / strongly falling intonation at the end of an utterance |
| **word** | Emphasised: clearly louder or higher than that speaker's normal voice |
| … | Short pause; longer ones are written as _(pause 2.3s)_ |
| _(silence 4.1s)_ | Silence between turns |
| `fast` `slow` `loud` `quiet` | Utterance compared with that speaker's own average |
| _[laughs]_ | Non-speech sound reported by the model |

**Speaker bleed:** if you use speakers instead of headphones, your mic also picks up the other side. MultiRec removes those duplicated words from your track. Turn this off in the Transcribe tab if needed.

**Limitation:** if several people talk through the same output device (for example a group call), they share one label such as "Others".

## Privacy
Everything stays on your PC. The only network access is the one-time download of the transcription model you pick, from Hugging Face (`huggingface.co/ggerganov/whisper.cpp`). MultiRec has no telemetry or accounts.

Recording other people may require their consent where you live.

## Troubleshooting
| Problem | Fix |
|---|---|
| A device is missing | Plug it in, then click **⟳ Refresh devices**. Disabled devices don't appear. |
| Output meter doesn't move | You selected a different output than the one playing audio. Check *Settings → System → Sound → Output*, or your call app's output setting. |
| "device lost — retrying" | The device was unplugged or its format changed. It reconnects automatically. |
| Hotkeys don't work | Another app uses the same shortcut; a message appears at startup. Use the buttons instead. |
| Transcription is slow | Use a smaller model, or raise *CPU threads*. An hour of audio with `small` takes roughly 10–30 minutes on a modern CPU. |
| Words are wrong or missing | Use a larger model and set the language explicitly instead of auto. |
| SmartScreen warning | The exe isn't code-signed. Choose *More info → Run anyway*, or build from source. |

## For developers and AI agents
- [CONTRIBUTING.md](CONTRIBUTING.md): building, testing, releasing.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how capture, sync, files and transcription work.
- [AGENTS.md](AGENTS.md): working rules for AI coding assistants.
- [CHANGELOG.md](CHANGELOG.md)

## License
[MIT](LICENSE). Uses [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT) and Whisper models (MIT).
