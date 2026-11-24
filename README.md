# Coral&Kai Companion

**Coral&Kai Companion** is a 100% Pure Rust + Tauri 2.0 + Embedded SQLite (`rusqlite` + `sqlite-vec` + `FTS5`) offline-ready 2D desktop AI companion engineered for Apple Silicon (8 GB M1 memory budget). It features two distinct companions — **Coral · คอรัล** and **Kai · ไค** — and uses a **BYOM (Bring Your Own Model)** architecture with zero bundled weights and zero Python, Docker, or PostgreSQL runtime dependencies.

## ภาพรวมภาษาไทย (Thai Overview)

**Coral&Kai Companion** คือเพื่อนคู่ใจ AI 2D บนเดสก์ท็อปที่พัฒนาด้วย Pure Rust + Tauri 2.0 และ SQLite แบบฝังในเครื่อง (`rusqlite` + `sqlite-vec` + `FTS5`) ออกแบบมาเพื่อทำงานแบบออฟไลน์เต็มรูปแบบบน Apple Silicon (รองรับงบหน่วยความจำ 8 GB M1) มาพร้อมสองตัวละคร (**Coral · คอรัล** และ **Kai · ไค**) ที่แยกฐานความจำและบุคลิกภาพออกจากกันอย่างสมบูรณ์ ใช้แนวทาง **BYOM (Bring Your Own Model)** โดยไม่ฝังไฟล์น้ำหนักโมเดลขนาดใหญ่ในซอร์สโค้ด และไม่ต้องพึ่งพา Python, Docker หรือ PostgreSQL

---

## Key Features

- **Dual 2D Vector & Live2D Companions (`Coral · คอรัล` & `Kai · ไค`)**
  - Switch seamlessly between Coral and Kai with character-matched Thai polite particles (`ค่ะ/คะ` for Coral, `ครับ` for Kai) and bilingual Thai/English voice & prompt routing.
  - Strictly isolated per-character SQLite memory databases stored under `profiles/coral/soul_memory.sqlite` and `profiles/kai/soul_memory.sqlite` in the app data directory (`~/Library/Application Support/com.coralkai.companion/`).
  - Supports both the built-in vector/Live2D companion rigs (`src-tauri/resources/live2d/coral_companion` and `kai_companion`) and custom imported `.model3.json` folders.

- **Persistent Inner Life (`crates/soul-memory`)**
  - **7-Emotion Decay Engine**: tracks `joy`, `sadness`, `shy`, `surprised`, `thinking`, `empathetic`, and `neutral` in `emotion_states` using exponential half-life decay so emotional tone evolves naturally over time.
  - **Personal Diary (`diary_entries` + `diary_entries_fts`)**: FTS5-indexed companion journal entries with same-day merge support.
  - **Self-Bio & User Profile (`persona_bio`, `user_profile`)**: persistent companion identity and learned user context.
  - **Memory Compaction (`consolidate_now()`)**: deduplicates redundant `facts` and merges same-day diary entries while preserving hybrid vector (`sqlite-vec`) + BM25 (`FTS5`) recall.

- **G.R.I.L.L.O. Reflective Inner Loop (`src-tauri/src/inner_loop.rs`)**
  - Gentle Reflective Inner Loop Lifecycle Orchestrator running on an opt-in background Tokio task (**OFF by default**).
  - Protected by 4-layer safety guards (`SkippedDisabled`, `SkippedNoModel`, `SkippedBusy`, `SkippedLowRam`) that prevent background ticks when disabled, when no GGUF LLM is loaded, during active chat/voice/recent user activity or quiet hours, or when an 8 GB-class Mac approaches memory limits.
  - Performs memory reflection/compaction, emotion decay refreshes, and optional non-intrusive proactive outreach (`companion://proactive`).

- **Centralized "Karada" Avatar State Server (`src-tauri/src/avatar_state.rs`)**
  - Single Source of Truth (`AvatarStateServer`) managing `AvatarLogicalState` (`idle | think | write | speak | sleep`), instantaneous emotion + decayed emotion profile, and 60 FPS lip-sync visemes broadcast over `avatar://state`.
  - Enforces a hard **Eyes-Closed Guard** invariant: whenever `logical_state == sleep` or `emotion == joy` with intensity `>= 0.65`, `eyes_closed` is forced `true` and both `auto_blink_enabled` and `gaze_tracking_enabled` are paused immediately.

- **Modular `CompanionPlugin` Trait & Optional Pure-Rust MCP Client (`crates/brain-llm`)**
  - Single message chain powered by a fault-isolated `PluginRegistry`.
  - Built-in offline plugins: `time_awareness`, `diary_writer`, and `bio_updater`.
  - Optional Pure-Rust JSON-RPC MCP client (`McpClientManager` / `McpPluginBridge`) supporting env-sandboxed `stdio` and raw-TCP `sse` transports (**OFF by default**; connection errors emit non-blocking warnings so offline chat never stalls).

---

## Project Architecture

| Crate / Directory | Role |
| --- | --- |
| [`crates/shared-types`](crates/shared-types) | Shared domain types, traits (`MemoryEngine`, `BrainEngine`, `VoiceEngine`), IPC event constants, and `SoulError` |
| [`crates/soul-memory`](crates/soul-memory) | Embedded SQLite (`rusqlite` + `sqlite-vec` + `FTS5`), 7-emotion decay, diary/bio/profile storage, compaction, and optional ONNX embeddings |
| [`crates/brain-llm`](crates/brain-llm) | Local GGUF inference (`llama-cpp-2` Metal), 8 GB RAM guard, prompt builder, `CompanionPlugin` registry, and Pure-Rust MCP client |
| [`crates/voice-io`](crates/voice-io) | `cpal` mic capture, `rubato` 16 kHz resampling, adaptive noise-floor VAD, Metal Whisper ASR orchestration, macOS `AVFoundation` TTS, and 60 FPS visemes |
| [`crates/whisper-bridge`](crates/whisper-bridge) | Isolated `cdylib` (`whisper-rs` with `metal`) compiled to `src-tauri/resources/runtime/libwhisper_bridge.dylib` to avoid GGML symbol collisions with `llama-cpp-2` |
| [`src-tauri`](src-tauri) | Tauri 2.0 application host, isolated character profiles, Karada `AvatarStateServer`, G.R.I.L.L.O. `InnerLoopManager`, and IPC commands |
| [`ui`](ui) | Vite + TypeScript frontend (`App.ts`, `AvatarStage.ts`, `companionRenderer.ts`, `live2dRuntime.ts`, `MemoryDrawer.ts`, `ModelDrawer.ts`) |

---

## Getting Started

### Prerequisites

- **macOS on Apple Silicon** (macOS 13.0+ target) with **Xcode Command Line Tools** (Metal & `AVFoundation`)
- **Rust toolchain** supporting **Edition 2024** (`rustc`, `cargo`)
- **CMake** (required by `llama-cpp-2` / `whisper-rs` native builds)
- **Node.js & npm** (for the `ui/` TypeScript/Vite frontend)
- **Tauri CLI** (`cargo install tauri-cli` or `npm run tauri --prefix ui`)

### 1. Build the Optional Local Whisper Metal Bridge (Once)

To enable local offline Whisper speech recognition without GGML symbol conflicts with `llama-cpp-2`, build `libwhisper_bridge.dylib` once:

```bash
./scripts/build_whisper_bridge.sh
```

### 2. Run in Development Mode

Start the full Tauri 2.0 desktop app (starts Vite dev server + Rust backend):

```bash
cargo tauri dev
```

Or run the TypeScript UI standalone in mock preview mode:

```bash
cd ui && npm install && npm run dev
```

### 3. Run Workspace Checks & Tests

```bash
cargo check --workspace
cargo test --workspace
cd ui && npx tsc --noEmit
```

---

## Bring Your Own Model (BYOM) Guide

No model weights are bundled in this repository. Coral&Kai Companion discovers and imports your local models from `~/Library/Application Support/com.coralkai.companion/` (or directly via the in-app **Models** drawer):

- **Local LLM (`.gguf`)**: Import any GGUF chat model via the **Models** drawer (`models/llm/`). On 8 GB Apple Silicon Macs, quantized 3B–4B Q4_K_M models are recommended by the RAM guard.
- **Speech Recognition / Whisper ASR (`.bin`)**: Import a GGML Whisper model (e.g., `ggml-base.bin` or `ggml-small-q5_1.bin`) via the **Models** drawer (`models/asr/`).
- **Semantic Embeddings (Optional `.onnx` + `tokenizer.json`)**: Import an ONNX embedding model and tokenizer (`models/embed/`). Without an ONNX model, `soul-memory` automatically uses its built-in 384-dim character n-gram fallback embedder.
- **Custom Live2D Avatar (`.model3.json`)**: Import a Live2D folder containing a `.model3.json` manifest via the **Models** drawer (`live2d/`).

Alternatively, you can run the optional user-side helper script to download open weights directly into `~/Library/Application Support/com.coralkai.companion/models`:

```bash
./scripts/provision_models.sh
```

> **Note:** Downloaded weights (`.gguf`, `.bin`, `.onnx`), compiled `.dylib` runtimes, `profiles/`, `models_config.json`, and `mcp_servers.json` are excluded by `.gitignore` and never committed to git.
