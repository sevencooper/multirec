#!/usr/bin/env bash
# Build/test the Windows .exe from WSL using the *Windows* Rust toolchain (cargo.exe).
#
# The source is mirrored to a folder on the Windows drive first, because building
# on a \\wsl$ path is slow and breaks some build scripts.
#
#   ./build-windows.sh           release build
#   ./build-windows.sh test      unit tests on Windows
#   ./build-windows.sh hwtest    records 3 s from the default mic + speakers and checks alignment
#   ./build-windows.sh e2e DIR   transcription end-to-end test (DIR = Windows path with TTS clips,
#                                see scripts/gen-test-speech.ps1); downloads a Whisper model once
#
# Override the mirror location with MULTIREC_WIN_DIR (a WSL path), e.g.
#   MULTIREC_WIN_DIR=/mnt/d/build/multirec ./build-windows.sh
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
DST="${MULTIREC_WIN_DIR:-/mnt/c/dev/multirec}"
command -v cargo.exe >/dev/null || { echo "cargo.exe not found: install Rust on Windows (see CONTRIBUTING.md)"; exit 1; }
mkdir -p "$DST"
rsync -a --delete --exclude target --exclude .git "$SRC/" "$DST/"
cd "$DST"
case "${1:-build}" in
  build)  cmd.exe /c "cargo build --release"
          echo "Built: $(wslpath -w "$DST")\\target\\release\\multirec.exe" ;;
  test)   cmd.exe /c "cargo test --release" ;;
  hwtest) cmd.exe /c "cargo test --release -- --ignored --nocapture records_default_devices" ;;
  e2e)    [ -n "${2:-}" ] || { echo "usage: $0 e2e <windows-dir-with-tts-clips>"; exit 2; }
          cmd.exe /c "set MULTIREC_TTS_DIR=$2&& cargo test --release -- --ignored --nocapture transcribes_synthetic_session" ;;
  *) echo "usage: $0 [build|test|hwtest|e2e DIR]"; exit 2 ;;
esac
