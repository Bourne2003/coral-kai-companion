use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use async_trait::async_trait;
use shared_types::{
    ActionInvocation, BrainEngine, BrainReply, ChatRole, ChatTurn, LanguagePref,
    MemoryRetrievalContext, ModelInfo, SoulError, SoulResult, SystemRamStatus, TokenCallback,
    TokenChunk, new_id, now_timestamp_ms,
};
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};

use crate::backend::{LlamaCppMetalBackend, LoadedModelInstance, ModelBackend};
use crate::embed::compute_local_embedding;
use crate::parser::{EmotionCallback, EmotionStreamParser};
use crate::plugins::parse_action_directives;
use crate::prompt::{
    CharacterPersona, InnerLifePromptContext, build_chat_messages,
    build_soul_system_prompt_enriched, build_soul_system_prompt_for_persona,
};
use crate::ram_guard::{
    RamProbe, SysinfoRamProbe, adaptive_params_for_ram, classify_ram_safety_tier,
    estimate_gguf_ram_mb, validate_footprint_against_budget, validate_static_llm_ceiling,
};
use crate::scanner::{register_custom_gguf, scan_llm_dirs, validate_gguf_magic};

const MAX_HISTORY_TURNS: usize = 16;
const DEFAULT_MAX_GEN_TOKENS: u32 = 256;

/// Wrapper around an active loaded LLM instance so `RwLock<Option<LoadedLlm>>`
/// owns and deterministically drops the Metal KV-cache and weight buffers.
pub struct LoadedLlm(Box<dyn LoadedModelInstance>);

/// Local GGUF model manager handling adaptive 8GB/16GB+ tuning, dual-folder discovery,
/// zero-leak hot-swapping, warm bilingual soul prompting, and streaming emotion/think parsing.
pub struct LlmBrainManager {
    swap_mutex: AsyncMutex<()>,
    loaded: Arc<AsyncRwLock<Option<LoadedLlm>>>,
    active_snapshot: StdRwLock<Option<ModelInfo>>,
    custom_models: StdMutex<Vec<ModelInfo>>,
    language_pref: StdMutex<LanguagePref>,
    persona: AtomicU8,
    bundled_llm_dir: PathBuf,
    user_llm_dir: PathBuf,
    ram_probe: Arc<dyn RamProbe>,
    backend: Arc<dyn ModelBackend>,
}

impl Default for LlmBrainManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for LlmBrainManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmBrainManager")
            .field("bundled_llm_dir", &self.bundled_llm_dir)
            .field("user_llm_dir", &self.user_llm_dir)
            .field("active_model", &self.active_model())
            .finish()
    }
}

impl LlmBrainManager {
    /// Creates a new `LlmBrainManager` backed by `LlamaCppMetalBackend` and `sysinfo` RAM telemetry.
    pub fn new() -> Self {
        Self::with_dirs(default_bundled_llm_dir(), default_user_llm_dir())
    }

    /// Creates a new `LlmBrainManager` scanning explicit bundled and user LLM directories.
    pub fn with_dirs(bundled_llm_dir: impl Into<PathBuf>, user_llm_dir: impl Into<PathBuf>) -> Self {
        Self::with_components(
            bundled_llm_dir.into(),
            user_llm_dir.into(),
            Arc::new(SysinfoRamProbe),
            Arc::new(LlamaCppMetalBackend),
        )
    }

    /// Creates a manager with custom `RamProbe` and `ModelBackend` implementations (useful for tests).
    pub fn with_components(
        bundled_llm_dir: PathBuf,
        user_llm_dir: PathBuf,
        ram_probe: Arc<dyn RamProbe>,
        backend: Arc<dyn ModelBackend>,
    ) -> Self {
        Self {
            swap_mutex: AsyncMutex::new(()),
            loaded: Arc::new(AsyncRwLock::new(None)),
            active_snapshot: StdRwLock::new(None),
            custom_models: StdMutex::new(Vec::new()),
            language_pref: StdMutex::new(LanguagePref::Auto),
            persona: AtomicU8::new(0),
            bundled_llm_dir,
            user_llm_dir,
            ram_probe,
            backend,
        }
    }

    /// Sets the active companion character persona (`Coral` or `Kai`).
    pub fn set_persona(&self, persona: CharacterPersona) {
        let code = match persona {
            CharacterPersona::Coral => 0,
            CharacterPersona::Kai => 1,
        };
        self.persona.store(code, Ordering::Release);
    }

    /// Returns the currently active companion character persona.
    pub fn persona(&self) -> CharacterPersona {
        match self.persona.load(Ordering::Acquire) {
            1 => CharacterPersona::Kai,
            _ => CharacterPersona::Coral,
        }
    }

    /// Sets the default language preference used when `BrainEngine::generate_reply` is called.
    pub fn set_language_pref(&self, pref: LanguagePref) -> SoulResult<()> {
        let mut guard = self
            .language_pref
            .lock()
            .map_err(|_| SoulError::Internal("language_pref mutex poisoned".into()))?;
        *guard = pref;
        Ok(())
    }

    /// Returns the currently configured language preference.
    pub fn language_pref(&self) -> LanguagePref {
        self.language_pref
            .lock()
            .map(|g| *g)
            .unwrap_or(LanguagePref::Auto)
    }

    /// Scans `bundled_llm_dir`, `user_llm_dir`, and registered custom `.gguf` models,
    /// validating GGUF 4-byte magic headers and classifying RAM safety tiers.
    pub fn scan_models(&self) -> SoulResult<Vec<ModelInfo>> {
        let snap = self.ram_probe.snapshot();
        let tuning = adaptive_params_for_ram(snap.total_ram_mb);
        let custom = self
            .custom_models
            .lock()
            .map_err(|_| SoulError::Internal("custom_models mutex poisoned".into()))?
            .clone();
        let active_path = self.active_model().map(|m| PathBuf::from(m.path));
        scan_llm_dirs(
            &self.bundled_llm_dir,
            &self.user_llm_dir,
            &custom,
            active_path.as_deref(),
            tuning.n_ctx,
        )
    }

    /// Validates a `.gguf` file on disk and adds it to the custom model registry.
    pub fn register_custom_model(
        &self,
        path: &Path,
        name: Option<&str>,
    ) -> SoulResult<ModelInfo> {
        let snap = self.ram_probe.snapshot();
        let tuning = adaptive_params_for_ram(snap.total_ram_mb);
        let info = register_custom_gguf(path, name, tuning.n_ctx)?;

        let mut custom = self
            .custom_models
            .lock()
            .map_err(|_| SoulError::Internal("custom_models mutex poisoned".into()))?;

        if let Some(existing) = custom.iter_mut().find(|m| m.path == info.path) {
            *existing = info.clone();
        } else {
            custom.push(info.clone());
        }

        let mut out = info;
        if let Some(active) = self.active_model()
            && active.path == out.path
        {
            out.is_active = true;
        }
        Ok(out)
    }

    /// Restores persisted custom `.gguf` entries by validating file existence and GGUF magic
    /// headers without loading model weights into RAM. Missing or invalid entries are skipped.
    pub fn restore_custom_models(&self, entries: &[(PathBuf, Option<String>)]) -> Vec<ModelInfo> {
        let mut restored = Vec::new();
        for (path, name) in entries {
            if let Ok(info) = self.register_custom_model(path, name.as_deref()) {
                restored.push(info);
            }
        }
        restored
    }

    /// Removes a previously registered custom `.gguf` entry by model `id` or file `path`.
    pub fn remove_custom_model(&self, model_id_or_path: &str) -> SoulResult<bool> {
        let mut custom = self
            .custom_models
            .lock()
            .map_err(|_| SoulError::Internal("custom_models mutex poisoned".into()))?;
        let target_canon = std::fs::canonicalize(model_id_or_path).ok();
        let before = custom.len();
        custom.retain(|m| {
            if m.id == model_id_or_path || m.path == model_id_or_path {
                return false;
            }
            if let Some(ref tc) = target_canon
                && std::fs::canonicalize(&m.path).ok().as_ref() == Some(tc)
            {
                return false;
            }
            true
        });
        Ok(custom.len() < before)
    }

    /// Atomically unloads any currently active LLM (dropping all Metal KV-cache & weights)
    /// BEFORE enforcing the RAM budget and loading `model`.
    pub async fn hot_swap(&self, model: &ModelInfo) -> SoulResult<()> {
        self.swap_model_inner(model).await
    }

    /// Streams a reply with immediate `on_emotion` dispatch (before spoken tokens arrive)
    /// and clean `on_token` chunks with `<think>...</think>` and `[emotion: ...]` stripped.
    pub async fn generate_reply_with_callbacks(
        &self,
        user_message: &str,
        history: &[ChatTurn],
        memory_ctx: &MemoryRetrievalContext,
        language_pref: LanguagePref,
        on_emotion: EmotionCallback,
        on_token: TokenCallback,
    ) -> SoulResult<BrainReply> {
        let trimmed = user_message.trim();
        if trimmed.is_empty() {
            return Err(SoulError::InvalidInput(
                "User message cannot be empty".to_string(),
            ));
        }

        const NO_LLM_MSG: &str = "ยังไม่ได้นำเข้าและเปิดใช้งานโมเดล LLM (.gguf) กรุณาเลือกไฟล์โมเดลในเมนู Models ก่อนเริ่มสนทนา / No LLM model loaded; import and activate a .gguf file in Models first.";

        let guard = Arc::clone(&self.loaded).read_owned().await;
        if guard.is_none() {
            return Err(SoulError::ModelNotFound(NO_LLM_MSG.to_string()));
        }

        let system_prompt =
            build_soul_system_prompt_for_persona(trimmed, memory_ctx, language_pref, self.persona());
        let messages = build_chat_messages(&system_prompt, history, trimmed, MAX_HISTORY_TURNS);

        let (emotion, clean_reply) = tokio::task::spawn_blocking(move || -> SoulResult<_> {
            let loaded = guard
                .as_ref()
                .ok_or_else(|| SoulError::ModelNotFound(NO_LLM_MSG.to_string()))?;

            let mut parser = EmotionStreamParser::new(on_emotion, on_token);
            loaded
                .0
                .generate(&messages, DEFAULT_MAX_GEN_TOKENS, &mut |piece| {
                    parser.push(piece);
                })?;

            Ok(parser.finish())
        })
        .await
        .map_err(|err| SoulError::Internal(format!("Inference task failed: {err}")))??;

        let turn = ChatTurn {
            id: new_id(),
            role: ChatRole::Assistant,
            content: clean_reply,
            emotion: Some(emotion.tag),
            timestamp_ms: now_timestamp_ms(),
        };

        Ok(BrainReply { turn, emotion })
    }

    /// Single-message-chain generation supporting enriched inner-life context (self-bio,
    /// user profile, decayed emotion baseline, latest diary, wall-clock time, and optional
    /// companion actions). Returns both the clean spoken [`BrainReply`] and any extracted
    /// [`ActionInvocation`]s without leaking `[action: ...]` tags into streamed tokens.
    #[allow(clippy::too_many_arguments)]
    pub async fn generate_reply_with_inner_life(
        &self,
        user_message: &str,
        history: &[ChatTurn],
        memory_ctx: &MemoryRetrievalContext,
        language_pref: LanguagePref,
        inner_life: &InnerLifePromptContext,
        on_emotion: EmotionCallback,
        on_token: TokenCallback,
    ) -> SoulResult<(BrainReply, Vec<ActionInvocation>)> {
        let trimmed = user_message.trim();
        if trimmed.is_empty() {
            return Err(SoulError::InvalidInput(
                "User message cannot be empty".to_string(),
            ));
        }

        const NO_LLM_MSG: &str = "ยังไม่ได้นำเข้าและเปิดใช้งานโมเดล LLM (.gguf) กรุณาเลือกไฟล์โมเดลในเมนู Models ก่อนเริ่มสนทนา / No LLM model loaded; import and activate a .gguf file in Models first.";

        let guard = Arc::clone(&self.loaded).read_owned().await;
        if guard.is_none() {
            return Err(SoulError::ModelNotFound(NO_LLM_MSG.to_string()));
        }

        let system_prompt = build_soul_system_prompt_enriched(
            trimmed,
            memory_ctx,
            language_pref,
            self.persona(),
            inner_life,
        );
        let messages = build_chat_messages(&system_prompt, history, trimmed, MAX_HISTORY_TURNS);

        let (emotion, raw_clean_reply) = tokio::task::spawn_blocking(move || -> SoulResult<_> {
            let loaded = guard
                .as_ref()
                .ok_or_else(|| SoulError::ModelNotFound(NO_LLM_MSG.to_string()))?;

            let mut filter = ActionTokenStreamFilter::new(on_token);
            let filtered_token_cb: TokenCallback = Box::new(move |chunk| filter.handle_chunk(chunk));

            let mut parser = EmotionStreamParser::new(on_emotion, filtered_token_cb);
            loaded
                .0
                .generate(&messages, DEFAULT_MAX_GEN_TOKENS, &mut |piece| {
                    parser.push(piece);
                })?;

            Ok(parser.finish())
        })
        .await
        .map_err(|err| SoulError::Internal(format!("Inference task failed: {err}")))??;

        let (clean_spoken, invocations) = parse_action_directives(&raw_clean_reply);

        let turn = ChatTurn {
            id: new_id(),
            role: ChatRole::Assistant,
            content: clean_spoken,
            emotion: Some(emotion.tag),
            timestamp_ms: now_timestamp_ms(),
        };

        Ok((BrainReply { turn, emotion }, invocations))
    }

    async fn swap_model_inner(&self, model: &ModelInfo) -> SoulResult<()> {
        if !model.valid {
            return Err(SoulError::InvalidInput(format!(
                "Refusing to load invalid model '{}'",
                model.name
            )));
        }

        let model_path = Path::new(&model.path);
        if !model_path.exists() || !model_path.is_file() {
            return Err(SoulError::ModelNotFound(format!(
                "GGUF file not found: {}",
                model.path
            )));
        }
        if !validate_gguf_magic(model_path)? {
            return Err(SoulError::InvalidInput(format!(
                "File lacks GGUF magic header: {}",
                model.path
            )));
        }

        let _swap_guard = self.swap_mutex.lock().await;

        let pre_snap = self.ram_probe.snapshot();
        let pre_tuning = adaptive_params_for_ram(pre_snap.total_ram_mb);
        let size_bytes = std::fs::metadata(model_path)?.len();
        let estimated_ram_mb = if model.estimated_ram_mb > 0 {
            model.estimated_ram_mb
        } else {
            estimate_gguf_ram_mb(size_bytes, pre_tuning.n_ctx)
        };

        validate_static_llm_ceiling(
            pre_snap.total_ram_mb,
            estimated_ram_mb,
            pre_snap.process_ram_mb,
        )?;

        let mut write_guard = self.loaded.write().await;
        if let Some(old) = write_guard.take() {
            *write_guard = None;
            self.set_active_snapshot(None);
            drop(old);
        }

        let post_snap = self.ram_probe.snapshot();
        let tuning = adaptive_params_for_ram(post_snap.total_ram_mb);
        validate_footprint_against_budget(post_snap, estimated_ram_mb)?;

        let mut next_info = model.clone();
        next_info.size_bytes = size_bytes;
        next_info.estimated_ram_mb = estimated_ram_mb;
        next_info.safety_tier = classify_ram_safety_tier(estimated_ram_mb);
        next_info.is_active = true;
        next_info.valid = true;

        let backend = Arc::clone(&self.backend);
        let load_info = next_info.clone();
        let loaded_instance = tokio::task::spawn_blocking(move || backend.load(&load_info, tuning))
            .await
            .map_err(|err| SoulError::Internal(format!("Model load task failed: {err}")))??;

        *write_guard = Some(LoadedLlm(loaded_instance));
        self.set_active_snapshot(Some(next_info));
        Ok(())
    }

    fn set_active_snapshot(&self, next: Option<ModelInfo>) {
        if let Ok(mut guard) = self.active_snapshot.write() {
            *guard = next;
        }
    }
}

#[async_trait]
impl BrainEngine for LlmBrainManager {
    async fn load_model(&self, model: &ModelInfo) -> SoulResult<()> {
        self.swap_model_inner(model).await
    }

    async fn unload_model(&self) -> SoulResult<()> {
        let _swap_guard = self.swap_mutex.lock().await;
        let mut write_guard = self.loaded.write().await;
        let old = write_guard.take();
        *write_guard = None;
        self.set_active_snapshot(None);
        drop(old);
        Ok(())
    }

    fn active_model(&self) -> Option<ModelInfo> {
        self.active_snapshot
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    async fn generate_reply(
        &self,
        user_message: &str,
        history: &[ChatTurn],
        memory_ctx: &MemoryRetrievalContext,
        on_token: TokenCallback,
    ) -> SoulResult<BrainReply> {
        self.generate_reply_with_callbacks(
            user_message,
            history,
            memory_ctx,
            self.language_pref(),
            Box::new(|_| {}),
            on_token,
        )
        .await
    }

    async fn embed_text(&self, text: &str) -> SoulResult<Vec<f32>> {
        compute_local_embedding(text)
    }

    fn system_ram_status(&self) -> SystemRamStatus {
        let snap = self.ram_probe.snapshot();
        let tuning = adaptive_params_for_ram(snap.total_ram_mb);

        SystemRamStatus {
            total_ram_mb: snap.total_ram_mb,
            available_ram_mb: snap.available_ram_mb,
            process_ram_mb: snap.process_ram_mb,
            budget_ceiling_mb: tuning.budget_ceiling_mb,
            is_8gb_class: tuning.is_8gb_class,
            active_llm_id: self.active_model().map(|m| m.id),
            active_asr_id: None,
        }
    }
}

fn default_bundled_llm_dir() -> PathBuf {
    let candidates = [
        PathBuf::from("src-tauri/resources/models/llm"),
        PathBuf::from("resources/models/llm"),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }
    PathBuf::from("src-tauri/resources/models/llm")
}

fn default_user_llm_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library/Application Support/com.coralkai.companion/models/llm");
    }
    PathBuf::from("models/llm")
}

struct ActionTokenStreamFilter {
    inner: TokenCallback,
    buf: String,
    in_action: bool,
    emitted_full: String,
}

impl ActionTokenStreamFilter {
    fn new(inner: TokenCallback) -> Self {
        Self {
            inner,
            buf: String::new(),
            in_action: false,
            emitted_full: String::new(),
        }
    }

    fn handle_chunk(&mut self, chunk: TokenChunk) {
        if !chunk.is_final {
            if !chunk.text.is_empty() {
                self.buf.push_str(&chunk.text);
                self.drain(false);
            }
            return;
        }

        self.drain(true);
        let final_full = self.emitted_full.trim_end().to_string();
        self.emitted_full = final_full.clone();
        (self.inner)(TokenChunk {
            text: String::new(),
            is_final: true,
            full_text: final_full,
        });
    }

    fn emit(&mut self, slice: &str) {
        if slice.is_empty() {
            return;
        }
        self.emitted_full.push_str(slice);
        (self.inner)(TokenChunk {
            text: slice.to_string(),
            is_final: false,
            full_text: self.emitted_full.clone(),
        });
    }

    fn drain(&mut self, at_eof: bool) {
        loop {
            if self.in_action {
                let Some(colon_pos) = self.buf.find(':') else {
                    break;
                };
                if let Some(rel_close) = find_balanced_action_close(&self.buf[colon_pos + 1..]) {
                    let close_idx = colon_pos + 1 + rel_close;
                    self.buf.drain(..=close_idx);
                    self.in_action = false;
                    if self.emitted_full.ends_with(' ') && self.buf.starts_with(' ') {
                        let spaces = self.buf.len() - self.buf.trim_start_matches(' ').len();
                        self.buf.drain(..spaces);
                    }
                    continue;
                }
                if at_eof || self.buf.len() > 4096 {
                    self.buf.clear();
                    self.in_action = false;
                }
                break;
            }

            let Some(open_idx) = self.buf.find('[') else {
                let ready = std::mem::take(&mut self.buf);
                self.emit(&ready);
                break;
            };

            if open_idx > 0 {
                let before = self.buf[..open_idx].to_string();
                self.buf.drain(..open_idx);
                self.emit(&before);
            }

            let after = self.buf[1..].trim_start_matches([' ', '\t']);
            let lower = after.to_ascii_lowercase();
            if lower.starts_with("action:") {
                self.in_action = true;
                continue;
            }
            if !at_eof && self.buf.len() < 32 && "action:".starts_with(&lower) {
                break;
            }

            self.buf.drain(..1);
            self.emit("[");
        }
    }
}

fn find_balanced_action_close(slice: &str) -> Option<usize> {
    let mut in_quote: Option<char> = None;
    let mut escaped = false;
    let mut brace_depth: usize = 0;
    let mut bracket_depth: usize = 0;

    for (idx, ch) in slice.char_indices() {
        if let Some(q) = in_quote {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == q {
                in_quote = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' => in_quote = Some(ch),
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => {
                if brace_depth == 0 && bracket_depth == 0 {
                    return Some(idx);
                }
                bracket_depth = bracket_depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    None
}
