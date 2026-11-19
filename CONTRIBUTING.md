# Contributing / Developer guide

This guide covers setting up a build environment, the day-to-day workflow, testing and releasing. Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before changing anything in `src/audio` or `src/recorder`.

## Toolchain

MultiRec is Rust, targeting `x86_64-pc-windows-msvc`. The audio code uses WASAPI, so **the app only runs on Windows**. Most logic (DSP, sync, FLAC, transcript processing) is platform-independent and unit-tested on Linux as well.

### Windows (required to build the .exe)
In PowerShell:
```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup
winget install --id Kitware.CMake
winget install --id LLVM.LLVM
winget install --id KhronosGroup.VulkanSDK
setx LIBCLANG_PATH "C:\Program Files\LLVM\bin"
```
Open a new terminal (from WSL: run `wsl --shutdown` first, so the new variables are visible), then run `rustup default stable-msvc`.
- CMake builds the bundled libopus (always) and whisper.cpp (default `whisper` feature).
- LLVM provides libclang for whisper-rs bindings.
- The Vulkan SDK (`VULKAN_SDK`, `glslc`) compiles whisper.cpp's GPU shaders (default `gpu` feature). It's needed at build time only.

No GPU SDK? Build CPU-only with `cargo build --release --no-default-features --features whisper`.

### WSL / Linux (optional, for fast unit tests)
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
sudo apt install -y build-essential cmake pkg-config libgtk-3-dev libxdo-dev
```

## Workflows

### A. Native Windows
```powershell
cargo build --release            # target\release\multirec.exe
cargo test --release             # unit tests
cargo run --release              # launch the app
```

### B. Edit in WSL, build with the Windows toolchain
`build-windows.sh` mirrors the source to a Windows-drive folder (default `C:\dev\multirec`; override with `MULTIREC_WIN_DIR`) and runs `cargo.exe` there. Building directly on a `\\wsl$` path is slow and breaks some build scripts.
```bash
./build-windows.sh               # release exe; prints its Windows path
./build-windows.sh test          # unit tests on Windows
./build-windows.sh hwtest        # real-device test (see below)
cargo test --no-default-features # unit tests on Linux, without whisper.cpp (~1 min cold)
```

### Features
| Feature | Default | Effect |
|---|---|---|
| `whisper` | on | Local transcription via whisper.cpp. `--no-default-features` builds without it; the Transcribe tab then shows a notice. |
| `gpu` | on | Vulkan GPU transcription. Needs the Vulkan SDK at build time. `vulkan-1.dll` is **delay-loaded** (see `build.rs`), so the exe starts without it, and `engine_whisper::vulkan_available()` checks for it before touching whisper.cpp. Adds about 55 MB of embedded shaders to the exe. |

`.cargo/config.toml` links the MSVC C runtime statically, so the exe has no VC++ redistributable dependency. Keep that.

## Testing

| Level | Command | What it proves |
|---|---|---|
| Unit | `cargo test` | Resampler accuracy and anti-aliasing; aligner sync under ±500 ppm drift, gaps, late or early start, 44.1→48 kHz; FLAC bit-exact round trips (decoded with `claxon`); WAV/FLAC sinks; YIN pitch; VAD; bleed removal; merge order; export formatting |
| Hardware | `./build-windows.sh hwtest` | Records 3 s in **each format (WAV, FLAC, Opus)** from the **default mic + default speakers** through the real capture → session → file pipeline. Asserts that all files have equal length, the duration is right, the manifest is complete, and markers are saved. |
| Transcription E2E | see below | Full pipeline on a synthetic two-speaker conversation, including speaker bleed: correct attribution, bleed removed, markers and pauses rendered |
| Manual | run the app | Anything UI-related. Use the checklist below. |

**Transcription E2E** (downloads the `base.en` model once):
```powershell
powershell -ExecutionPolicy Bypass -File scripts\gen-test-speech.ps1 -OutDir C:\temp\multirec-tts
```
```bash
./build-windows.sh e2e 'C:\temp\multirec-tts'
```
Set `MULTIREC_TEST_MODEL` to try another model, `MULTIREC_TEST_FORMAT=opus|flac|wav` to store the test tracks in that format, and `MULTIREC_TEST_CPU=1` to force the CPU.

**Diagnostics** (both `#[ignore]`):
- `whisper_speed` benchmarks GPU against CPU on the TTS clips (`MULTIREC_TTS_DIR`).
- `transcribes_real_session` runs the app's exact job on a *copy* of a real session (`MULTIREC_SESSION_DIR`). Never commit its output.

**Manual checklist for UI or capture changes:**
- [ ] Meters react on every selected source; clip light fires above about −0.1 dBFS.
- [ ] Record → Pause → Resume → Marker → Stop. Files open and are of equal length; markers appear in `session.json`.
- [ ] Unplug a USB device mid-recording: a warning appears, the other tracks continue, it reconnects on replug.
- [ ] Long silence on the output device (nothing playing): tracks stay aligned (play a clap at the end and compare).
- [ ] Close the window while recording: files are finalized (`"complete": true`).
- [ ] FLAC output opens in Audacity or a similar editor.

## Code conventions
- `cargo fmt` before committing; run `cargo clippy` and fix lints in code you touch.
- Keep the build warning-free. Mark intentionally unused items with a targeted `#[allow(dead_code)]`, not a crate-wide one.
- Use `anyhow::Result` with context (`.with_context(|| …)`) for anything user-facing; error strings end up in the UI.
- Capture threads must never block: no file I/O, no locks held across WASAPI calls, and no allocations beyond the packet buffer.
- New DSP or sync logic needs a unit test that simulates the failure mode it handles.
- Write docs and comments in plain English. Explain *why*, not *what*.

## Dependencies and known pitfalls
- **`eframe` is pinned** (`=0.29.1`), because egui's API changes every release. Upgrade deliberately and fix the UI code in one go.
- **`whisper-rs` must be 0.16 or newer.** 0.13's bindgen produced opaque structs with recent LLVM. 0.16 uses a segment-object API (`state.as_iter()`, `seg.get_token(j)`); don't copy older examples.
- **Word timestamps:** use DTW (`DtwParameters` with a model preset, mapped in `engine_whisper.rs::dtw_preset`). whisper.cpp's default heuristic timestamps were seconds off in testing. A new model needs an entry in that mapping.
- **CMake 4.x** builds whisper.cpp fine with whisper-rs 0.16. If an older dependency fails with a `cmake_minimum_required` error, set `CMAKE_POLICY_VERSION_MINIMUM=3.5`.
- **The `windows` crate** is pinned to 0.58. WASAPI struct layouts are `packed`: read them with `ptr::read_unaligned`, never by reference.

## Releasing
1. Update `version` in `Cargo.toml` and add a section to `CHANGELOG.md`.
2. Commit: `git commit -am "Release v0.2.0"`.
3. Tag and push: `git tag v0.2.0 && git push && git push --tags`.
4. GitHub Actions (`.github/workflows/ci.yml`) tests, builds and publishes a Release with `multirec.exe` attached.

## Privacy rule for commits
The repository is public. Never commit:
- recordings, transcripts or models (`.gitignore` covers `*.wav`, `*.flac`, `*.bin`);
- personal paths, user names, device IDs or names from your machine;
- secrets or tokens.

Tests must use generic or temp paths, and environment variables for anything machine-specific.
