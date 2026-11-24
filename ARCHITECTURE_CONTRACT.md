# SoulMate2D — Architecture & Phase 2 Contract

## 1. Environment & Toolchain Setup

Every subshell starts with default `PATH`, so all agents and build scripts MUST prepend:

```zsh
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
```

- **Target platform**: `aarch64-apple-darwin` (Apple Silicon arm64, macOS >= 13.0)
- **Shared Cargo target dir** (configured in `.cargo/config.toml`):
  - `target-dir = "/Users/goshi/.gemini/antigravity/scratch/soulmate2d-target"`
  - `jobs = 4` (keeps peak build RAM safe on 8GB Unified Memory)
  - `LIBCLANG_PATH = "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib"`
  - `MACOSX_DEPLOYMENT_TARGET = "13.0"`
  - `GGML_METAL = "1"`
- **TLS / Downloads**: Python 3.14 SSL fails (`CERTIFICATE_VERIFY_FAILED`) in this environment. Always use `/usr/bin/curl` for HTTPS API calls and model downloads; use `python3` only for local JSON/PNG/binary generation.

---

## 2. 8GB M1 Unified Memory Constraints

- **8GB-class detection**: `total_ram_mb <= 9216` sets `is_8gb_class = true` and `budget_ceiling_mb = 4800` MB (for 16GB+ hosts, ceiling is 70% of total RAM).
- **RAM safety tiers** (`RamSafetyTier`):
  - `safe8Gb`: `estimatedRamMb <= 3200` (recommended default for 8GB Macs; leaves headroom for macOS, WebKit, Live2D WebGL, ASR, and SQLite)
  - `tight8Gb`: `3201 <= estimatedRamMb <= 4500` (requires warning / strict memory discipline)
  - `recommended16GbPlus`: `estimatedRamMb > 4500` (blocked or warned on 8GB class hardware)
- **Strict hot-swap discipline**:
  - `hot_swap_llm` MUST call `brain.unload_model().await` and ensure the previous GGUF/Metal context is fully dropped before `brain.load_model(&target).await` allocates the replacement model. Never hold two LLMs resident simultaneously.
  - Keep `llama.cpp` context size conservative (`n_ctx <= 4096`) and offload layers via Metal (`GGML_METAL=1`).

---

## 3. Zero-Conflict Crate Ownership & Phase 2 Worktrees

All 5 Phase 2 worktrees branch from `main` and live under `/Users/goshi/.gemini/antigravity/scratch/soulmate2d-wt/`:

| Worktree Path | Branch | Owns (Edit Scope) | Must NOT Edit |
| :--- | :--- | :--- | :--- |
| `../soulmate2d-wt/asset-provisioner` | `phase2/asset-provisioner` | `scripts/**`, `src-tauri/resources/models/**/manifest.json`, `src-tauri/resources/models/**/*.json`, `src-tauri/resources/live2d/**` | `Cargo.toml`, `crates/**`, `src-tauri/src/**`, `ui/**` |
| `../soulmate2d-wt/memory-soul` | `phase2/memory-soul` | `crates/soul-memory/**` | Root `Cargo.toml`, `crates/shared-types/**`, other crates |
| `../soulmate2d-wt/brain-hotswap` | `phase2/brain-hotswap` | `crates/brain-llm/**` | Root `Cargo.toml`, `crates/shared-types/**`, other crates |
| `../soulmate2d-wt/voice-pipeline` | `phase2/voice-pipeline` | `crates/voice-io/**` | Root `Cargo.toml`, `crates/shared-types/**`, other crates |
| `../soulmate2d-wt/avatar-ui` | `phase2/avatar-ui` | `ui/**` (and coordinated `src-tauri/**` integration) | Root `Cargo.toml`, `crates/shared-types/**`, domain crates |

**Important dependency rule**: Crate-specific dependencies (`llama-cpp-2`, `whisper-rs`, `rusqlite`, `sqlite-vec`, `cpal`, `tts`, etc.) MUST be declared in each crate's own `Cargo.toml`, never in the root `Cargo.toml`.

---

## 4. Shared Trait Contracts (`crates/shared-types`)

All crates implement traits defined in `crates/shared-types/src/lib.rs` and stay completely independent of `tauri`:

- `MemoryEngine` (implemented by `soul_memory::SoulMemoryStore`):
  - `append_turn(turn: ChatTurn)`
  - `recent_turns(limit: usize)`
  - `retrieve_context(query: &str, query_embedding: Option<&[f32]>, fact_limit: usize, episode_limit: usize) -> MemoryRetrievalContext`
  - `upsert_fact(fact: MemoryFact, embedding: Option<&[f32]>)`
  - `list_facts(limit: usize, include_superseded: bool) -> Vec<MemoryFact>`
  - `store_episode(episode: EpisodeRecord, embedding: Option<&[f32]>)`
  - `list_dream_diaries(limit: usize) -> Vec<DreamDiary>`
  - `store_dream_diary(diary: DreamDiary)`
  - `get_relationship_state() -> RelationshipState`
  - `update_relationship_state(state: RelationshipState)`
  - `consolidate_now() -> Option<DreamDiary>`
  - `export_snapshot(target_path: &Path) -> String`
  - `import_snapshot(source_path: &Path) -> usize`
- `BrainEngine` (implemented by `brain_llm::LlmBrainManager`):
  - `load_model(model: &ModelInfo)` / `unload_model()` / `active_model() -> Option<ModelInfo>`
  - `generate_reply(user_message, history, memory_ctx, on_token: Box<dyn FnMut(TokenChunk) + Send>) -> BrainReply`
  - `embed_text(text: &str) -> Vec<f32>`
  - `system_ram_status() -> SystemRamStatus`
- `VoiceEngine` (implemented by `voice_io::VoicePipeline`):
  - `load_asr_model(model: &ModelInfo)` / `active_asr_model() -> Option<ModelInfo>`
  - `set_voice_mode(mode: VoiceMode)` / `voice_mode() -> VoiceMode`
  - `set_language_pref(pref: LanguagePref)` / `language_pref() -> LanguagePref`
  - `start_listening(on_transcript: Box<dyn FnMut(TranscriptEvent) + Send>)` / `stop_listening()`
  - `speak(text: &str, emotion: EmotionTag, on_viseme: Box<dyn FnMut(VisemeFrame) + Send>)` / `stop_speaking()`

All timestamps (`timestampMs`, `createdAt`, `lastAccessedAt`, `supersededAt`, `lastInteractionAt`) are UNIX epoch milliseconds (`i64` in Rust, `number` in TypeScript).

---

## 5. Tauri IPC Contract (`src-tauri` <-> `ui/src/types/ipc.ts`)

### 5.1 Events

| Event Name | Payload Type | Emitted By |
| :--- | :--- | :--- |
| `chat://token` | `TokenChunk` | `send_chat_message` |
| `avatar://emotion` | `EmotionState` | `send_chat_message` |
| `avatar://viseme` | `VisemeFrame` | `preview_speech_and_visemes` / TTS playback |
| `voice://transcript` | `TranscriptEvent` | `start_listening` ASR stream |
| `system://ram` | `SystemRamStatus` | `hot_swap_llm` / telemetry |

### 5.2 Commands

Tauri v2 converts snake_case Rust parameters to camelCase JSON fields in `invoke(cmd, args)`.

| Command | JS Arguments | Return Type |
| :--- | :--- | :--- |
| `send_chat_message` | `{ message: string }` | `BrainReply` |
| `get_ram_status` | `{}` | `SystemRamStatus` |
| `list_models` | `{ kind?: ModelKind \| null }` | `ModelInfo[]` |
| `hot_swap_llm` | `{ modelId: string }` | `ModelInfo` |
| `switch_asr_model` | `{ modelId: string }` | `ModelInfo` |
| `register_custom_model` | `{ kind: ModelKind, path: string, name?: string \| null }` | `ModelInfo` |
| `set_voice_mode` | `{ mode: VoiceMode }` | `void` |
| `set_language_pref` | `{ pref: LanguagePref }` | `void` |
| `start_listening` | `{}` | `void` |
| `stop_listening` | `{}` | `void` |
| `stop_speaking` | `{}` | `void` |
| `preview_speech_and_visemes` | `{ text: string, emotion?: EmotionTag \| null }` | `void` |
| `list_memory_facts` | `{ limit?: number \| null, includeSuperseded?: boolean \| null }` | `MemoryFact[]` |
| `list_dream_diaries` | `{ limit?: number \| null }` | `DreamDiary[]` |
| `get_relationship_state` | `{}` | `RelationshipState` |
| `consolidate_dreams_now` | `{}` | `DreamDiary \| null` |
| `export_soul_memory` | `{ targetPath: string }` | `string` |
| `import_soul_memory` | `{ sourcePath: string }` | `number` |
| `set_desktop_pet_mode` | `{ enabled: boolean }` | `boolean` |
| `list_live2d_models` | `{}` | `ModelInfo[]` |
| `load_live2d_folder` | `{ folderPath: string }` | `ModelInfo` |

### 5.3 Error Wire Format

`SoulError` serializes as a tagged union:

```json
{ "type": "insufficientRam", "detail": { "neededMb": 5200, "availableMb": 3100 } }
```

All other variants carry string `detail` (`memory`, `llm`, `voice`, `modelNotFound`, `invalidInput`, `io`, `serialization`, `notImplemented`, `internal`).

---

## 6. Resource & Packaging Layout

```text
soulmate2d/
├── .cargo/config.toml              # arm64 target, shared target-dir, Metal + libclang env
├── Cargo.toml                      # workspace root (edition 2024, shared deps only)
├── ARCHITECTURE_CONTRACT.md
├── scripts/                        # model/asset download & validation scripts (tracked)
├── crates/
│   ├── shared-types/               # shared types, events, traits, SoulError
│   ├── soul-memory/                # SQLite + sqlite-vec + dream diary
│   ├── brain-llm/                  # llama.cpp Metal + hot-swap + RAM guard
│   └── voice-io/                   # cpal + VAD + Whisper ASR + TTS visemes
├── src-tauri/
│   ├── tauri.conf.json             # productName SoulMate2D, transparent macOSPrivateApi
│   ├── capabilities/default.json   # window + dialog permissions
│   ├── Info.plist                  # NSMicrophoneUsageDescription
│   ├── entitlements.plist          # com.apple.security.device.audio-input
│   ├── icons/                      # icon.png (512x512 RGBA), icon.icns
│   └── resources/
│       ├── models/
│       │   ├── llm/                # .gguf ignored; manifest.json / .gitkeep tracked
│       │   ├── asr/                # .bin / .onnx ignored; manifest.json / .gitkeep tracked
│       │   └── embed/              # .gguf / .onnx ignored; tokenizer/config .json & .gitkeep tracked
│       └── live2d/                 # bundled Live2D Cubism models (tracked)
└── ui/
    ├── package.json
    ├── tsconfig.json
    ├── vite.config.ts
    ├── index.html
    └── src/
        ├── main.ts
        └── types/ipc.ts
```
