# AGENTS.md: instructions for AI coding assistants

MultiRec is a Windows GUI app in Rust (egui) that records several audio devices (microphones plus WASAPI loopback of outputs) into **sample-aligned** files, and transcribes them per speaker with local Whisper.

Read these as needed:
- `README.md`: user-facing behaviour. Keep it accurate when you change behaviour.
- `docs/ARCHITECTURE.md`: threads, the sync algorithm, file formats, the transcription pipeline. **Read it before touching `src/audio/` or `src/recorder/`.**
- `CONTRIBUTING.md`: toolchain, commands, testing levels, release steps.

## Commands
| Purpose | Command |
|---|---|
| Fast unit tests (Linux/WSL, no whisper.cpp) | `cargo test --no-default-features` |
| Type-check Linux-side | `cargo check --no-default-features` |
| Windows release build from WSL | `./build-windows.sh` |
| Windows unit tests from WSL | `./build-windows.sh test` |
| Real-device recording test (needs a mic + speakers) | `./build-windows.sh hwtest` |
| Transcription end-to-end | `./build-windows.sh e2e <win-dir>` (clips from `scripts/gen-test-speech.ps1`) |

`src/audio/wasapi.rs` and Windows-only `cfg` code compile **only** on Windows: a Linux `cargo check` does not cover them. After editing Windows code, run `./build-windows.sh` (or `cargo build` on Windows). If no Windows toolchain is available, say clearly that the change is unverified.

You can't see the GUI. For UI changes, make sure it compiles, then ask the human to check it visually. Don't claim UI behaviour you haven't observed.

## Invariants (don't break these)
1. **All tracks of a session start at the same instant and end with identical frame counts.** Sync logic lives in `recorder/aligner.rs` and `recorder/session.rs::stop`. Any change there must keep the `aligner::tests` passing, and should add a simulated test for the new case.
2. **Capture threads must not block**: no file I/O, no long-held locks, no heavy work. Do processing in the session thread.
3. **The UI thread never waits on audio or disk** (except the short join on exit, which finalizes files).
4. **Files stay readable after a crash**: periodic `Sink::flush()` (WAV header rewrite) and a frame-by-frame FLAC encoder. Don't replace these with whole-file-at-end writers.
5. **One portable exe**: static CRT (`.cargo/config.toml`), and no runtime DLLs beyond Windows system DLLs. Check with `objdump -p multirec.exe | grep "DLL Name"`. The one exception is `vulkan-1.dll` (GPU transcription), which must stay **delay-loaded** (`build.rs`) and is only touched after `engine_whisper::vulkan_available()` returns true.
6. **Everything local**: the only network call is the model download in `transcribe/models.rs`. No telemetry.
7. **Opus files keep exact length**: `pre_skip` = encoder lookahead, and the final granule = `pre_skip + frames_written` (see `recorder/opus.rs`). Never drop packets during silence (DTX packets still get written), or timing breaks.
8. **`session.json` is a contract** between recorder and transcriber (and users' scripts). Add fields with `#[serde(default)]`; bump `version` on breaking changes.

## Gotchas already learned
- GPU builds (default `gpu` feature) need the Vulkan SDK on the build machine. Without it, use `--no-default-features --features whisper`. Benchmark: `whisper_speed` (GPU about 0.4 s against CPU about 80 s for 28 s of audio with `large-v3-turbo` on a high-end desktop GPU and 8-core CPU).
- Reuse one `WhisperState` per job (`Engine::state`). Creating one per chunk costs a GPU warm-up each time.
- **Never build whisper.cpp "native".** `.cargo/config.toml` `[env]` pins ggml to an AVX2 baseline (`GGML_NATIVE=OFF`). Native builds on CI produced AVX-512 code that crashed users' machines instantly, with no error. whisper-rs-sys does **not** rebuild when these variables change: run `cargo clean -p whisper-rs-sys` locally, and bump `prefix-key` in `ci.yml`. CI checks the exe for AVX-512 (`zmm`) instructions.
- `whisper-rs` **0.16** API: `state.as_iter()`, `seg.start_timestamp()` (10 ms units), `seg.get_token(j)`, `tok.token_data().t_dtw`. Older versions (0.13) failed bindgen with current LLVM. Ignore examples written for older APIs.
- Use **DTW timestamps** (`engine_whisper.rs::dtw_preset`). The default timestamps were off by seconds, and caused real words to be filtered out as "hallucinations". Adding a model means adding its preset.
- Whisper hallucinates on silence. Keep the VAD batching (`plan_speech_batches`) and the energy gate in `prosody::annotate_words`.
- The `windows` crate is pinned to 0.58. WASAPI structs (`WAVEFORMATEX`, `WAVEFORMATEXTENSIBLE`) are `packed`, so use `ptr::read_unaligned`. Packet timestamps (`u64QPCPosition`) are in 100 ns units, matching `clock::now_hns()`.
- `eframe` is pinned (`=0.29.1`). Its API changes between versions (for example `ComboBox::from_id_salt`, `DragValue::range`).
- Loopback capture delivers **no packets during silence** unless something plays. The keep-alive silent render stream and the session's idle padding both exist for this reason.
- Don't judge Opus quality by waveform SNR: VoIP mode is perceptual, so measure loudness envelope or transcription accuracy instead (see `opus::tests`).
- Dither never touches exact zeros, so FLAC still compresses digital silence. Keep it that way.
- Devices are keyed `in:<endpoint-id>` / `lb:<endpoint-id>` in config and packets. Device names aren't unique.

## Conventions
- Rust 2021. Keep `cargo build` warning-free, and run `cargo fmt`.
- Errors: `anyhow` with context. Messages are shown to users, so make them actionable.
- Tests: deterministic, no network or devices in default tests. Real hardware or network tests are `#[ignore]` and documented in CONTRIBUTING.md.
- Update `CHANGELOG.md` (Unreleased section) for user-visible changes.

## Public repository: privacy
This repository is public. Never add personal data: no user names, home paths, machine-specific device names or IDs, email addresses, recordings, transcripts or tokens. Use temp dirs and environment variables in tests. Before committing, grep the diff for paths like `C:\Users\` or `/home/`.
