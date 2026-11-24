#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

cargo build --release --target aarch64-apple-darwin -p whisper-bridge

TARGET_DIR="$(python3 -c 'import json, subprocess; print(json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"]))["target_directory"])')"
SRC_DYLIB="${TARGET_DIR}/aarch64-apple-darwin/release/libwhisper_bridge.dylib"
DEST_DIR="${REPO_ROOT}/src-tauri/resources/runtime"
mkdir -p "${DEST_DIR}"
cp "${SRC_DYLIB}" "${DEST_DIR}/libwhisper_bridge.dylib"
