use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use brain_llm::{
    CharacterPersona, InnerLifePromptContext, LlmBrainManager, McpClientManager, PluginActionSink,
    PluginContext, PluginRegistry, current_time_summary, load_mcp_config_safe,
};
use shared_types::{
    ActionResult, AvatarLogicalState, AvatarSnapshot, BrainEngine, BrainReply, CharacterId,
    ChatRole, ChatTurn, DiaryEntry, DreamDiary, EmotionProfileState, EmotionState, EmotionTag,
    InnerLoopConfig, InnerLoopStatus, LanguagePref, McpConfig, McpToolInfo, MemoryEngine,
    MemoryFact, ModelInfo, ModelKind, ModelSource, PersonaBio, PluginInfo, RamSafetyTier,
    RelationshipState, SoulError, SoulResult, SystemRamStatus, UserProfile, VisemeFrame,
    VoiceEngine, VoiceMode, events, new_id, now_timestamp_ms,
};
use soul_memory::{SoulMemoryStore, validate_onnx_embed_file};
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::{info, warn};
use voice_io::VoicePipeline;

use crate::avatar_state::AvatarStateServer;
use crate::config::{ModelsConfig, SavedModelEntry};
use crate::inner_loop::InnerLoopManager;
use crate::paths::{
    AppPaths, inspect_live2d_folder, sanitize_slug, scan_embed_models, scan_live2d_models,
};

const NO_LLM_BYOM_MSG: &str = "ยังไม่ได้นำเข้าและเปิดใช้งานโมเดล LLM (.gguf) กรุณาเลือกไฟล์ในเมนู Models ก่อนเริ่มสนทนา / No LLM model loaded; import and activate a .gguf model in Models first.";

/// Shared application state managed by Tauri across all IPC commands and lifecycle hooks.
pub struct AppState {
    pub coral_memory: Arc<SoulMemoryStore>,
    pub kai_memory: Arc<SoulMemoryStore>,
    pub active_character: Arc<RwLock<CharacterId>>,
    pub brain: Arc<LlmBrainManager>,
    pub voice: Arc<VoicePipeline>,
    pub avatar_state: Arc<AvatarStateServer>,
    pub inner_loop: Arc<InnerLoopManager>,
    pub plugin_registry: Arc<PluginRegistry>,
    pub mcp_manager: Arc<McpClientManager>,
    pub paths: AppPaths,
    pub models_config: Mutex<ModelsConfig>,
    pub registered_embed_models: Mutex<Vec<ModelInfo>>,
    pub registered_live2d_models: Mutex<Vec<ModelInfo>>,
    pub active_live2d_id: Mutex<Option<String>>,
    pub desktop_pet_mode: Mutex<bool>,
    pub shutdown_done: Arc<AtomicBool>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        coral_memory: Arc<SoulMemoryStore>,
        kai_memory: Arc<SoulMemoryStore>,
        active_character: CharacterId,
        brain: Arc<LlmBrainManager>,
        voice: Arc<VoicePipeline>,
        avatar_state: Arc<AvatarStateServer>,
        inner_loop: Arc<InnerLoopManager>,
        plugin_registry: Arc<PluginRegistry>,
        mcp_manager: Arc<McpClientManager>,
        paths: AppPaths,
        models_config: ModelsConfig,
        registered_embed_models: Vec<ModelInfo>,
        registered_live2d_models: Vec<ModelInfo>,
        active_live2d_id: String,
    ) -> Self {
        Self {
            coral_memory,
            kai_memory,
            active_character: Arc::new(RwLock::new(active_character)),
            brain,
            voice,
            avatar_state,
            inner_loop,
            plugin_registry,
            mcp_manager,
            paths,
            models_config: Mutex::new(models_config),
            registered_embed_models: Mutex::new(registered_embed_models),
            registered_live2d_models: Mutex::new(registered_live2d_models),
            active_live2d_id: Mutex::new(Some(active_live2d_id)),
            desktop_pet_mode: Mutex::new(false),
            shutdown_done: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Return the currently active character identity.
    pub fn active_character_id(&self) -> CharacterId {
        self.active_character
            .read()
            .map(|guard| *guard)
            .unwrap_or(CharacterId::Coral)
    }

    /// Return the isolated SQLite memory store for `character`.
    pub fn memory_for(&self, character: CharacterId) -> Arc<SoulMemoryStore> {
        match character {
            CharacterId::Coral => Arc::clone(&self.coral_memory),
            CharacterId::Kai => Arc::clone(&self.kai_memory),
        }
    }

    /// Return the strictly isolated SQLite memory store for the currently active character.
    pub fn active_memory(&self) -> Arc<SoulMemoryStore> {
        self.memory_for(self.active_character_id())
    }

    pub fn current_ram_status(&self) -> SystemRamStatus {
        let mut status = self.brain.system_ram_status();
        status.active_asr_id = self.voice.active_asr_model().map(|m| m.id);
        status
    }
}

/// Host persistence bridge connecting `DiaryWriterPlugin` & `BioUpdaterPlugin` directly
/// to the active character's `SoulMemoryStore` and `AvatarStateServer` (`Write` -> `Idle`).
pub struct TauriPluginSink {
    memory: Arc<SoulMemoryStore>,
    avatar: Arc<AvatarStateServer>,
    app: Option<AppHandle>,
}

impl TauriPluginSink {
    pub fn new(
        memory: Arc<SoulMemoryStore>,
        avatar: Arc<AvatarStateServer>,
        app: Option<AppHandle>,
    ) -> Self {
        Self {
            memory,
            avatar,
            app,
        }
    }

    fn transition_state(
        &self,
        logical_state: AvatarLogicalState,
        profile: Option<EmotionProfileState>,
    ) {
        let snap = self
            .avatar
            .set_logical_state(logical_state, None, profile);
        if let Some(ref app_handle) = self.app {
            AvatarStateServer::emit_snapshot(app_handle, &snap);
        }
    }
}

#[async_trait]
impl PluginActionSink for TauriPluginSink {
    async fn write_diary(
        &self,
        title: &str,
        content: &str,
        mood: EmotionTag,
    ) -> SoulResult<DiaryEntry> {
        self.transition_state(AvatarLogicalState::Write, None);

        let mem = Arc::clone(&self.memory);
        let title_owned = title.to_string();
        let content_owned = content.to_string();
        let res = tokio::task::spawn_blocking(move || {
            let entry =
                mem.add_or_merge_diary_entry(&title_owned, &content_owned, mood, "plugin", None)?;
            let profile = mem.get_emotion_profile().ok();
            Ok::<(DiaryEntry, Option<EmotionProfileState>), SoulError>((entry, profile))
        })
        .await
        .map_err(|e| SoulError::Internal(format!("Diary write task failed: {e}")));

        match res {
            Ok(Ok((entry, profile))) => {
                self.transition_state(AvatarLogicalState::Idle, profile);
                Ok(entry)
            }
            Ok(Err(err)) => {
                self.transition_state(AvatarLogicalState::Idle, None);
                Err(err)
            }
            Err(err) => {
                self.transition_state(AvatarLogicalState::Idle, None);
                Err(err)
            }
        }
    }

    async fn update_persona_bio(
        &self,
        traits: Option<String>,
        self_reflection: Option<String>,
        favorite_topics: Option<String>,
    ) -> SoulResult<PersonaBio> {
        self.transition_state(AvatarLogicalState::Write, None);

        let mem = Arc::clone(&self.memory);
        let res = tokio::task::spawn_blocking(move || {
            let mut bio = mem.get_persona_bio()?;
            if let Some(t) = traits {
                bio.traits = t;
            }
            if let Some(r) = self_reflection {
                bio.self_reflection = r;
            }
            if let Some(f) = favorite_topics {
                bio.favorite_topics = f;
            }
            mem.update_persona_bio(bio)
        })
        .await
        .map_err(|e| SoulError::Internal(format!("Bio update task failed: {e}")));

        self.transition_state(AvatarLogicalState::Idle, None);
        res?
    }

    async fn update_user_profile(
        &self,
        display_name: Option<String>,
        timezone: Option<String>,
        location: Option<String>,
        notes: Option<String>,
    ) -> SoulResult<UserProfile> {
        self.transition_state(AvatarLogicalState::Write, None);

        let mem = Arc::clone(&self.memory);
        let res = tokio::task::spawn_blocking(move || {
            let mut profile = mem.get_user_profile()?;
            if let Some(v) = display_name {
                profile.display_name = v;
            }
            if let Some(v) = timezone {
                profile.timezone = v;
            }
            if let Some(v) = location {
                profile.location = v;
            }
            if let Some(v) = notes {
                profile.notes = v;
            }
            mem.update_user_profile(profile)
        })
        .await
        .map_err(|e| SoulError::Internal(format!("User profile update task failed: {e}")));

        self.transition_state(AvatarLogicalState::Idle, None);
        res?
    }
}

struct ChatBusyGuard {
    flag: Arc<AtomicBool>,
}

impl ChatBusyGuard {
    fn acquire(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Release);
        Self { flag }
    }
}

impl Drop for ChatBusyGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

async fn build_plugin_context(
    memory: &Arc<SoulMemoryStore>,
    avatar: &Arc<AvatarStateServer>,
    app: Option<AppHandle>,
    character_id: CharacterId,
) -> PluginContext {
    let mem = Arc::clone(memory);
    let (user_profile, persona_bio) = tokio::task::spawn_blocking(move || {
        (mem.get_user_profile().ok(), mem.get_persona_bio().ok())
    })
    .await
    .unwrap_or((None, None));

    PluginContext {
        character_id,
        user_profile,
        persona_bio,
        sink: Some(Arc::new(TauriPluginSink::new(
            Arc::clone(memory),
            Arc::clone(avatar),
            app,
        ))),
    }
}

pub fn emit_ram_status(app: &AppHandle, state: &AppState) {
    let status = state.current_ram_status();
    let _ = app.emit(events::SYSTEM_RAM, status);
}

fn save_config_entry(state: &AppState, kind: ModelKind, info: &ModelInfo) -> SoulResult<()> {
    let mut cfg = state
        .models_config
        .lock()
        .map_err(|_| SoulError::Internal("models_config mutex poisoned".into()))?;
    cfg.upsert_entry(
        kind,
        SavedModelEntry {
            id: info.id.clone(),
            name: info.name.clone(),
            path: info.path.clone(),
        },
    );
    cfg.save(&state.paths.config_path)
}

fn remove_config_entry(state: &AppState, kind: ModelKind, id_or_path: &str) -> SoulResult<()> {
    let mut cfg = state
        .models_config
        .lock()
        .map_err(|_| SoulError::Internal("models_config mutex poisoned".into()))?;
    if cfg.remove_entry(kind, id_or_path) {
        cfg.save(&state.paths.config_path)?;
    }
    Ok(())
}

/// Build and validate a `ModelInfo` descriptor for a custom `.onnx` embedding model without loading weights.
pub fn build_custom_embed_info(
    path: &Path,
    name: Option<&str>,
    id_override: Option<&str>,
) -> SoulResult<ModelInfo> {
    validate_onnx_embed_file(path)?;
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let size_bytes = std::fs::metadata(&canon).map(|m| m.len()).unwrap_or(0);
    let estimated_ram_mb = ((size_bytes / (1024 * 1024)).saturating_add(128)).max(128);
    let safety_tier = if estimated_ram_mb <= 3200 {
        RamSafetyTier::Safe8Gb
    } else if estimated_ram_mb <= 4500 {
        RamSafetyTier::Tight8Gb
    } else {
        RamSafetyTier::Recommended16GbPlus
    };
    let display_name = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            canon
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("custom-embed")
                .to_string()
        });
    let id = id_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            let stem = canon
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("custom-embed");
            format!("custom:embed:{}", sanitize_slug(stem))
        });

    Ok(ModelInfo {
        id,
        name: display_name,
        kind: ModelKind::Embed,
        path: canon.to_string_lossy().into_owned(),
        size_bytes,
        estimated_ram_mb,
        source: ModelSource::Custom,
        safety_tier,
        is_active: false,
        valid: true,
        notes: Some("Custom ONNX embedding model (384-dim)".to_string()),
    })
}

fn list_embed_models_inner(state: &AppState) -> SoulResult<Vec<ModelInfo>> {
    let custom_embed = state
        .registered_embed_models
        .lock()
        .map_err(|_| SoulError::Internal("registered_embed_models mutex poisoned".into()))?
        .clone();
    let active_embed_path = state.active_memory().active_embed_model_path();
    Ok(scan_embed_models(
        &state.paths.bundled_embed_dir,
        &state.paths.user_embed_dir,
        &custom_embed,
        active_embed_path.as_deref(),
    ))
}

#[tauri::command]
pub async fn send_chat_message(
    app: AppHandle,
    state: State<'_, AppState>,
    message: String,
) -> SoulResult<BrainReply> {
    let trimmed = message.trim().to_string();
    if trimmed.is_empty() {
        return Err(SoulError::InvalidInput("Message cannot be empty".into()));
    }

    // Step 1: Record activity, set chat_busy = true, transition AvatarStateServer -> Think.
    state.inner_loop.record_user_activity();
    let _busy_guard = ChatBusyGuard::acquire(state.inner_loop.chat_busy_flag());
    let think_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Think, None, None);
    AvatarStateServer::emit_snapshot(&app, &think_snap);

    // Step 2: Enforce BYOM empty-state guard immediately when no LLM is loaded.
    if state.brain.active_model().is_none() {
        let idle_snap =
            state
                .avatar_state
                .set_logical_state(AvatarLogicalState::Idle, None, None);
        AvatarStateServer::emit_snapshot(&app, &idle_snap);
        return Err(SoulError::ModelNotFound(NO_LLM_BYOM_MSG.into()));
    }

    let active_char = state.active_character_id();
    let memory = state.active_memory();

    let user_turn = ChatTurn {
        id: new_id(),
        role: ChatRole::User,
        content: trimmed.clone(),
        emotion: None,
        timestamp_ms: now_timestamp_ms(),
    };
    memory.append_turn(user_turn.clone()).await?;
    if let Err(err) = memory.extract_and_upsert_facts_from_turn(&user_turn).await {
        warn!("Fact extraction skipped for user turn: {err}");
    }

    let history = memory.recent_turns(20).await?;

    let mem_for_embed = Arc::clone(&memory);
    let text_for_embed = trimmed.clone();
    let query_embedding =
        tokio::task::spawn_blocking(move || mem_for_embed.embed_text(&text_for_embed))
            .await
            .ok()
            .and_then(Result::ok);

    // Step 3: Gather memory context + enriched inner-life context on active character's store.
    let memory_ctx = memory
        .retrieve_context(&trimmed, query_embedding.as_deref(), 8, 4)
        .await?;

    let mem_for_inner = Arc::clone(&memory);
    let (persona_bio, user_profile, emotion_profile, recent_diary) =
        tokio::task::spawn_blocking(move || {
            (
                mem_for_inner.get_persona_bio().ok(),
                mem_for_inner.get_user_profile().ok(),
                mem_for_inner.get_emotion_profile().ok(),
                mem_for_inner
                    .list_diary_entries(1)
                    .ok()
                    .and_then(|v| v.into_iter().next()),
            )
        })
        .await
        .unwrap_or((None, None, None, None));

    let inner_life = InnerLifePromptContext {
        persona_bio: persona_bio.clone(),
        user_profile: user_profile.clone(),
        emotion_profile,
        recent_diary,
        current_time_summary: Some(current_time_summary()),
        enabled_actions: state.plugin_registry.enabled_actions(),
    };

    let language_pref = state.voice.language_pref();
    let app_emotion = app.clone();
    let avatar_emotion = Arc::clone(&state.avatar_state);
    let on_emotion = Box::new(move |emotion: EmotionState| {
        let _ = app_emotion.emit(events::AVATAR_EMOTION, &emotion);
        let snap = avatar_emotion.set_emotion(emotion, None);
        AvatarStateServer::emit_snapshot(&app_emotion, &snap);
    });

    let app_tokens = app.clone();
    let on_token = Box::new(move |chunk| {
        let _ = app_tokens.emit(events::CHAT_TOKEN, chunk);
    });

    // Step 4: Call generate_reply_with_inner_life.
    let (reply, invocations) = match state
        .brain
        .generate_reply_with_inner_life(
            &trimmed,
            &history,
            &memory_ctx,
            language_pref,
            &inner_life,
            on_emotion,
            on_token,
        )
        .await
    {
        Ok(res) => res,
        Err(err) => {
            let idle_snap =
                state
                    .avatar_state
                    .set_logical_state(AvatarLogicalState::Idle, None, None);
            AvatarStateServer::emit_snapshot(&app, &idle_snap);
            return Err(err);
        }
    };

    memory.append_turn(reply.turn.clone()).await?;

    // Gently evolve live relationship warmth/familiarity after a successful turn.
    if let Ok(mut rel) = memory.get_relationship_state().await {
        let mood_delta = match reply.emotion.tag {
            EmotionTag::Joy | EmotionTag::Empathetic | EmotionTag::Shy => 0.008,
            EmotionTag::Sadness => -0.004,
            _ => 0.003,
        };
        rel.trust = (rel.trust + 0.003).clamp(0.0, 1.0);
        rel.warmth = (rel.warmth + 0.005).clamp(0.0, 1.0);
        rel.familiarity = (rel.familiarity + 0.006).clamp(0.0, 1.0);
        rel.mood_baseline = (rel.mood_baseline + mood_delta).clamp(0.0, 1.0);
        rel.last_interaction_at = now_timestamp_ms();
        let _ = memory.update_relationship_state(rel).await;
    }

    // Step 5: Record emotion impulse in active_memory & update AvatarStateServer.
    let mem_for_impulse = Arc::clone(&memory);
    let impulse_tag = reply.emotion.tag;
    let impulse_intensity = reply.emotion.intensity;
    let updated_profile = tokio::task::spawn_blocking(move || {
        mem_for_impulse.record_emotion_impulse(impulse_tag, impulse_intensity)
    })
    .await
    .ok()
    .and_then(Result::ok);

    let emo_snap = state
        .avatar_state
        .set_emotion(reply.emotion.clone(), updated_profile);
    AvatarStateServer::emit_snapshot(&app, &emo_snap);

    // Step 6: Dispatch any parsed ActionInvocations cleanly (fault-isolated).
    if !invocations.is_empty() {
        let plugin_ctx = PluginContext {
            character_id: active_char,
            user_profile,
            persona_bio,
            sink: Some(Arc::new(TauriPluginSink::new(
                Arc::clone(&memory),
                Arc::clone(&state.avatar_state),
                Some(app.clone()),
            ))),
        };
        let results = state
            .plugin_registry
            .dispatch_invocations(&invocations, &plugin_ctx)
            .await;
        for res in results {
            if res.success {
                info!(plugin = %res.plugin_name, action = %res.action, summary = %res.summary, "Executed companion action");
            } else {
                warn!(plugin = %res.plugin_name, action = %res.action, summary = %res.summary, "Companion action did not succeed");
            }
        }
    }

    // Step 7: Trigger offline AVFoundation TTS + 60 FPS viseme streaming (Speak -> Idle).
    if !reply.turn.content.trim().is_empty() {
        let speak_snap =
            state
                .avatar_state
                .set_logical_state(AvatarLogicalState::Speak, None, None);
        AvatarStateServer::emit_snapshot(&app, &speak_snap);

        let voice = Arc::clone(&state.voice);
        let avatar_for_tts = Arc::clone(&state.avatar_state);
        let spoken_text = reply.turn.content.clone();
        let spoken_emotion = reply.emotion.tag;
        let app_viseme = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = voice.stop_speaking().await;
            let app_cb = app_viseme.clone();
            let avatar_cb = Arc::clone(&avatar_for_tts);
            let on_viseme = Box::new(move |frame: VisemeFrame| {
                let _ = app_cb.emit(events::AVATAR_VISEME, frame);
                let _ = avatar_cb.set_viseme(frame);
            });
            if let Err(err) = voice.speak(&spoken_text, spoken_emotion, on_viseme).await {
                warn!("Offline TTS playback error: {err}");
            }
            let _ = avatar_for_tts.set_viseme(VisemeFrame {
                mouth_open_y: 0.0,
                mouth_form: 0.0,
                energy: 0.0,
                timestamp_ms: now_timestamp_ms(),
            });
            if avatar_for_tts.snapshot().logical_state == AvatarLogicalState::Speak {
                let idle_snap =
                    avatar_for_tts.set_logical_state(AvatarLogicalState::Idle, None, None);
                AvatarStateServer::emit_snapshot(&app_viseme, &idle_snap);
            }
        });
    } else {
        let idle_snap =
            state
                .avatar_state
                .set_logical_state(AvatarLogicalState::Idle, None, None);
        AvatarStateServer::emit_snapshot(&app, &idle_snap);
    }

    state.inner_loop.record_user_activity();
    Ok(reply)
}

#[tauri::command]
pub async fn get_ram_status(state: State<'_, AppState>) -> SoulResult<SystemRamStatus> {
    Ok(state.current_ram_status())
}

#[tauri::command]
pub async fn list_models(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: Option<ModelKind>,
) -> SoulResult<Vec<ModelInfo>> {
    let mut results = Vec::new();

    if kind.is_none() || kind == Some(ModelKind::Llm) {
        results.extend(state.brain.scan_models()?);
    }

    if kind.is_none() || kind == Some(ModelKind::Asr) {
        results.extend(state.voice.scan_asr_models()?);
    }

    if kind.is_none() || kind == Some(ModelKind::Embed) {
        results.extend(list_embed_models_inner(&state)?);
    }

    if kind.is_none() || kind == Some(ModelKind::Live2d) {
        let custom_live2d = state
            .registered_live2d_models
            .lock()
            .map_err(|_| SoulError::Internal("registered_live2d_models mutex poisoned".into()))?
            .clone();
        let active_id = state
            .active_live2d_id
            .lock()
            .map_err(|_| SoulError::Internal("active_live2d_id mutex poisoned".into()))?
            .clone();
        let (allowed_dirs, live2d_models) = scan_live2d_models(
            &state.paths.bundled_live2d_dir,
            &state.paths.user_live2d_dir,
            &custom_live2d,
            active_id.as_deref(),
        );
        for dir in allowed_dirs {
            let _ = app.asset_protocol_scope().allow_directory(&dir, true);
        }
        results.extend(live2d_models);
    }

    Ok(results)
}

#[tauri::command]
pub async fn hot_swap_llm(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> SoulResult<ModelInfo> {
    let models = state.brain.scan_models()?;
    let target = models
        .into_iter()
        .find(|m| m.id == model_id && m.kind == ModelKind::Llm)
        .ok_or_else(|| SoulError::ModelNotFound(format!("LLM model not found: {model_id}")))?;

    // LlmBrainManager::hot_swap strictly unloads & drops previous GGUF/Metal context BEFORE loading.
    state.brain.hot_swap(&target).await?;
    emit_ram_status(&app, &state);

    let mut active = state.brain.active_model().unwrap_or(target);
    active.is_active = true;
    info!(model_id = %active.id, name = %active.name, "Hot-swapped active LLM");
    Ok(active)
}

#[tauri::command]
pub async fn switch_asr_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> SoulResult<ModelInfo> {
    let active = state.voice.switch_asr_model(&model_id).await?;
    emit_ram_status(&app, &state);
    info!(model_id = %active.id, name = %active.name, "Switched active Whisper ASR model");
    Ok(active)
}

#[tauri::command]
pub async fn switch_embed_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> SoulResult<ModelInfo> {
    let models = list_embed_models_inner(&state)?;
    let target = models
        .into_iter()
        .find(|m| m.id == model_id && m.kind == ModelKind::Embed)
        .ok_or_else(|| {
            SoulError::ModelNotFound(format!("Embedding model not found: {model_id}"))
        })?;

    let active_mem = state.active_memory();
    let coral_mem = Arc::clone(&state.coral_memory);
    let kai_mem = Arc::clone(&state.kai_memory);
    let reload_path = PathBuf::from(&target.path);
    tokio::task::spawn_blocking(move || -> SoulResult<()> {
        active_mem.reload_embedder_from_file(&reload_path)?;
        // Keep both character stores synchronized on the selected ONNX embedder.
        if !Arc::ptr_eq(&active_mem, &coral_mem) {
            coral_mem.reload_embedder_from_file(&reload_path)?;
        }
        if !Arc::ptr_eq(&active_mem, &kai_mem) {
            kai_mem.reload_embedder_from_file(&reload_path)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| SoulError::Internal(format!("Embedder load task failed: {e}")))??;

    emit_ram_status(&app, &state);

    let active = list_embed_models_inner(&state)?
        .into_iter()
        .find(|m| m.id == model_id || m.is_active)
        .unwrap_or_else(|| {
            let mut m = target;
            m.is_active = true;
            m.valid = true;
            m
        });
    info!(model_id = %active.id, name = %active.name, "Switched active ONNX embedding model");
    Ok(active)
}

#[tauri::command]
pub async fn register_custom_model(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: ModelKind,
    path: String,
    name: Option<String>,
) -> SoulResult<ModelInfo> {
    let p = PathBuf::from(&path);
    let info = match kind {
        ModelKind::Llm => state.brain.register_custom_model(&p, name.as_deref())?,
        ModelKind::Asr => state.voice.register_custom_asr_model(&p, name.as_deref())?,
        ModelKind::Live2d => load_live2d_folder_inner(&app, &state, &p, name.as_deref())?,
        ModelKind::Embed => register_custom_embed_model(&state, &p, name.as_deref())?,
    };

    if info.source == ModelSource::Custom {
        save_config_entry(&state, kind, &info)?;
    }

    Ok(info)
}

#[tauri::command]
pub async fn remove_custom_model(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: ModelKind,
    model_id: String,
) -> SoulResult<()> {
    match kind {
        ModelKind::Llm => {
            let active_matches = state
                .brain
                .active_model()
                .is_some_and(|m| m.id == model_id || m.path == model_id);
            let removed_path = state
                .brain
                .scan_models()?
                .into_iter()
                .find(|m| {
                    m.source == ModelSource::Custom && (m.id == model_id || m.path == model_id)
                })
                .map(|m| m.path);
            state.brain.remove_custom_model(&model_id)?;
            if active_matches {
                state.brain.unload_model().await?;
            }
            remove_config_entry(&state, ModelKind::Llm, &model_id)?;
            if let Some(p) = removed_path {
                remove_config_entry(&state, ModelKind::Llm, &p)?;
            }
        }
        ModelKind::Asr => {
            let active_matches = state
                .voice
                .active_asr_model()
                .is_some_and(|m| m.id == model_id || m.path == model_id);
            let removed_path = state
                .voice
                .scan_asr_models()?
                .into_iter()
                .find(|m| {
                    m.source == ModelSource::Custom && (m.id == model_id || m.path == model_id)
                })
                .map(|m| m.path);
            state.voice.remove_custom_model(&model_id)?;
            if active_matches {
                state.voice.unload_asr_model().await?;
            }
            remove_config_entry(&state, ModelKind::Asr, &model_id)?;
            if let Some(p) = removed_path {
                remove_config_entry(&state, ModelKind::Asr, &p)?;
            }
        }
        ModelKind::Embed => {
            let mut removed_path = None;
            {
                let mut guard = state
                    .registered_embed_models
                    .lock()
                    .map_err(|_| {
                        SoulError::Internal("registered_embed_models mutex poisoned".into())
                    })?;
                guard.retain(|m| {
                    if m.id == model_id || m.path == model_id {
                        removed_path = Some(m.path.clone());
                        false
                    } else {
                        true
                    }
                });
            }
            remove_config_entry(&state, ModelKind::Embed, &model_id)?;
            if let Some(p) = removed_path {
                remove_config_entry(&state, ModelKind::Embed, &p)?;
            }
        }
        ModelKind::Live2d => {
            let mut removed_path = None;
            {
                let mut guard = state
                    .registered_live2d_models
                    .lock()
                    .map_err(|_| {
                        SoulError::Internal("registered_live2d_models mutex poisoned".into())
                    })?;
                guard.retain(|m| {
                    if m.id == model_id || m.path == model_id {
                        removed_path = Some(m.path.clone());
                        false
                    } else {
                        true
                    }
                });
            }
            {
                let mut active = state
                    .active_live2d_id
                    .lock()
                    .map_err(|_| SoulError::Internal("active_live2d_id mutex poisoned".into()))?;
                if active.as_deref() == Some(model_id.as_str()) {
                    let fallback = match state.brain.persona() {
                        CharacterPersona::Kai => "kai_companion",
                        CharacterPersona::Coral => "coral_companion",
                    };
                    *active = Some(fallback.to_string());
                }
            }
            remove_config_entry(&state, ModelKind::Live2d, &model_id)?;
            if let Some(p) = removed_path {
                remove_config_entry(&state, ModelKind::Live2d, &p)?;
            }
        }
    }

    emit_ram_status(&app, &state);
    Ok(())
}

#[tauri::command]
pub async fn set_character_persona(
    app: AppHandle,
    state: State<'_, AppState>,
    character: String,
) -> SoulResult<String> {
    let persona = CharacterPersona::from_id(&character).ok_or_else(|| {
        SoulError::InvalidInput(format!(
            "Unknown character persona '{character}'; expected 'coral' or 'kai'"
        ))
    })?;
    let char_id = match persona {
        CharacterPersona::Coral => CharacterId::Coral,
        CharacterPersona::Kai => CharacterId::Kai,
    };
    let is_male = persona == CharacterPersona::Kai;
    let live2d_id = if is_male {
        "kai_companion"
    } else {
        "coral_companion"
    };

    {
        let mut active_char = state
            .active_character
            .write()
            .map_err(|_| SoulError::Internal("active_character lock poisoned".into()))?;
        *active_char = char_id;
    }

    state.brain.set_persona(persona);
    state.voice.set_voice_persona(is_male);

    {
        let mut active_live2d = state
            .active_live2d_id
            .lock()
            .map_err(|_| SoulError::Internal("active_live2d_id mutex poisoned".into()))?;
        *active_live2d = Some(live2d_id.to_string());
    }

    {
        let mut cfg = state
            .models_config
            .lock()
            .map_err(|_| SoulError::Internal("models_config mutex poisoned".into()))?;
        cfg.active_character = persona.as_id().to_string();
        cfg.save(&state.paths.config_path)?;
    }

    let mem = state.active_memory();
    let profile = tokio::task::spawn_blocking(move || mem.get_emotion_profile())
        .await
        .map_err(|e| SoulError::Internal(format!("Emotion profile load failed: {e}")))??;

    let snapshot = state.avatar_state.switch_character(char_id, profile);
    AvatarStateServer::emit_snapshot(&app, &snapshot);
    let _ = app.emit(events::AVATAR_EMOTION, &snapshot.emotion);

    info!(character = %persona.as_id(), live2d_id = %live2d_id, "Updated active character persona & isolated memory profile");
    Ok(persona.as_id().to_string())
}

fn register_custom_embed_model(
    state: &AppState,
    path: &Path,
    name: Option<&str>,
) -> SoulResult<ModelInfo> {
    let mut info = build_custom_embed_info(path, name, None)?;
    if let Some(active_path) = state.active_memory().active_embed_model_path() {
        let active_canon = std::fs::canonicalize(&active_path).unwrap_or(active_path);
        let info_canon =
            std::fs::canonicalize(&info.path).unwrap_or_else(|_| PathBuf::from(&info.path));
        if active_canon == info_canon {
            info.is_active = true;
        }
    }

    let mut guard = state
        .registered_embed_models
        .lock()
        .map_err(|_| SoulError::Internal("registered_embed_models mutex poisoned".into()))?;
    if let Some(existing) = guard.iter_mut().find(|m| m.path == info.path) {
        *existing = info.clone();
    } else {
        guard.push(info.clone());
    }
    Ok(info)
}

#[tauri::command]
pub async fn set_voice_mode(state: State<'_, AppState>, mode: VoiceMode) -> SoulResult<()> {
    state.voice.set_voice_mode(mode).await?;
    if mode == VoiceMode::Muted {
        state.voice.stop_listening().await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn set_language_pref(
    state: State<'_, AppState>,
    pref: LanguagePref,
) -> SoulResult<()> {
    state.voice.set_language_pref(pref).await?;
    state.brain.set_language_pref(pref)?;
    Ok(())
}

#[tauri::command]
pub async fn start_listening(app: AppHandle, state: State<'_, AppState>) -> SoulResult<()> {
    state.inner_loop.record_user_activity();
    let app_handle = app.clone();
    let on_transcript = Box::new(move |evt| {
        let _ = app_handle.emit(events::VOICE_TRANSCRIPT, evt);
    });
    state.voice.start_listening(on_transcript).await
}

#[tauri::command]
pub async fn stop_listening(state: State<'_, AppState>) -> SoulResult<()> {
    state.voice.stop_listening().await
}

#[tauri::command]
pub async fn stop_speaking(app: AppHandle, state: State<'_, AppState>) -> SoulResult<()> {
    state.voice.stop_speaking().await?;
    let zero_viseme = VisemeFrame {
        mouth_open_y: 0.0,
        mouth_form: 0.0,
        energy: 0.0,
        timestamp_ms: now_timestamp_ms(),
    };
    let _ = app.emit(events::AVATAR_VISEME, zero_viseme);
    let _ = state.avatar_state.set_viseme(zero_viseme);
    if state.avatar_state.snapshot().logical_state == AvatarLogicalState::Speak {
        let idle_snap =
            state
                .avatar_state
                .set_logical_state(AvatarLogicalState::Idle, None, None);
        AvatarStateServer::emit_snapshot(&app, &idle_snap);
    }
    Ok(())
}

#[tauri::command]
pub async fn preview_speech_and_visemes(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    emotion: Option<EmotionTag>,
) -> SoulResult<()> {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err(SoulError::InvalidInput(
            "Preview text cannot be empty".into(),
        ));
    }

    let tag = emotion.unwrap_or(EmotionTag::Joy);
    let emo_state = EmotionState {
        tag,
        intensity: if tag == EmotionTag::Neutral { 0.45 } else { 0.88 },
        reason: format!("Previewing {tag} expression & lip-sync"),
        timestamp_ms: now_timestamp_ms(),
    };
    let _ = app.emit(events::AVATAR_EMOTION, &emo_state);

    let speak_snap =
        state
            .avatar_state
            .set_logical_state(AvatarLogicalState::Speak, Some(emo_state), None);
    AvatarStateServer::emit_snapshot(&app, &speak_snap);

    let voice = Arc::clone(&state.voice);
    let avatar_for_tts = Arc::clone(&state.avatar_state);
    let app_viseme = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = voice.stop_speaking().await;
        let app_cb = app_viseme.clone();
        let avatar_cb = Arc::clone(&avatar_for_tts);
        let on_viseme = Box::new(move |frame: VisemeFrame| {
            let _ = app_cb.emit(events::AVATAR_VISEME, frame);
            let _ = avatar_cb.set_viseme(frame);
        });
        if let Err(err) = voice.speak(&trimmed, tag, on_viseme).await {
            warn!("Preview speech & visemes error: {err}");
        }
        let _ = avatar_for_tts.set_viseme(VisemeFrame {
            mouth_open_y: 0.0,
            mouth_form: 0.0,
            energy: 0.0,
            timestamp_ms: now_timestamp_ms(),
        });
        if avatar_for_tts.snapshot().logical_state == AvatarLogicalState::Speak {
            let idle_snap =
                avatar_for_tts.set_logical_state(AvatarLogicalState::Idle, None, None);
            AvatarStateServer::emit_snapshot(&app_viseme, &idle_snap);
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn list_memory_facts(
    state: State<'_, AppState>,
    limit: Option<usize>,
    include_superseded: Option<bool>,
) -> SoulResult<Vec<MemoryFact>> {
    state
        .active_memory()
        .list_facts(limit.unwrap_or(100), include_superseded.unwrap_or(false))
        .await
}

#[tauri::command]
pub async fn list_dream_diaries(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> SoulResult<Vec<DreamDiary>> {
    state
        .active_memory()
        .list_dream_diaries(limit.unwrap_or(30))
        .await
}

#[tauri::command]
pub async fn get_relationship_state(
    state: State<'_, AppState>,
) -> SoulResult<RelationshipState> {
    state.active_memory().get_relationship_state().await
}

#[tauri::command]
pub async fn consolidate_dreams_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> SoulResult<Option<DreamDiary>> {
    let write_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Write, None, None);
    AvatarStateServer::emit_snapshot(&app, &write_snap);

    let mem = state.active_memory();
    let res = mem.consolidate_now().await;

    let profile = tokio::task::spawn_blocking(move || mem.get_emotion_profile())
        .await
        .ok()
        .and_then(Result::ok);
    let idle_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Idle, None, profile);
    AvatarStateServer::emit_snapshot(&app, &idle_snap);

    res
}

#[tauri::command]
pub async fn export_soul_memory(
    state: State<'_, AppState>,
    target_path: String,
) -> SoulResult<String> {
    state
        .active_memory()
        .export_snapshot(&PathBuf::from(target_path))
        .await
}

#[tauri::command]
pub async fn import_soul_memory(
    state: State<'_, AppState>,
    source_path: String,
) -> SoulResult<usize> {
    state
        .active_memory()
        .import_snapshot(&PathBuf::from(source_path))
        .await
}

#[tauri::command]
pub async fn get_emotion_profile(
    app: AppHandle,
    state: State<'_, AppState>,
) -> SoulResult<EmotionProfileState> {
    let mem = state.active_memory();
    let profile = tokio::task::spawn_blocking(move || mem.get_emotion_profile())
        .await
        .map_err(|e| SoulError::Internal(format!("Emotion profile task failed: {e}")))??;
    let snap = state.avatar_state.update_emotion_profile(profile.clone());
    AvatarStateServer::emit_snapshot(&app, &snap);
    Ok(profile)
}

#[tauri::command]
pub async fn list_diary_entries(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> SoulResult<Vec<DiaryEntry>> {
    let mem = state.active_memory();
    let max_items = limit.unwrap_or(30);
    tokio::task::spawn_blocking(move || mem.list_diary_entries(max_items))
        .await
        .map_err(|e| SoulError::Internal(format!("Diary list task failed: {e}")))?
}

#[tauri::command]
pub async fn add_diary_entry(
    app: AppHandle,
    state: State<'_, AppState>,
    title: String,
    content: String,
    mood: Option<EmotionTag>,
) -> SoulResult<DiaryEntry> {
    let trimmed_title = title.trim().to_string();
    let trimmed_content = content.trim().to_string();
    if trimmed_title.is_empty() || trimmed_content.is_empty() {
        return Err(SoulError::InvalidInput(
            "Diary title and content cannot be empty.".into(),
        ));
    }

    let resolved_mood = mood.unwrap_or_else(|| state.avatar_state.snapshot().emotion.tag);
    let write_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Write, None, None);
    AvatarStateServer::emit_snapshot(&app, &write_snap);

    let mem = state.active_memory();
    let res = tokio::task::spawn_blocking(move || {
        let entry = mem.add_or_merge_diary_entry(
            &trimmed_title,
            &trimmed_content,
            resolved_mood,
            "manual",
            None,
        )?;
        let profile = mem.record_emotion_impulse(resolved_mood, 0.75).ok();
        Ok::<(DiaryEntry, Option<EmotionProfileState>), SoulError>((entry, profile))
    })
    .await
    .map_err(|e| SoulError::Internal(format!("Diary write task failed: {e}")));

    match res {
        Ok(Ok((entry, profile))) => {
            let idle_snap =
                state
                    .avatar_state
                    .set_logical_state(AvatarLogicalState::Idle, None, profile);
            AvatarStateServer::emit_snapshot(&app, &idle_snap);
            Ok(entry)
        }
        Ok(Err(err)) => {
            let idle_snap =
                state
                    .avatar_state
                    .set_logical_state(AvatarLogicalState::Idle, None, None);
            AvatarStateServer::emit_snapshot(&app, &idle_snap);
            Err(err)
        }
        Err(err) => {
            let idle_snap =
                state
                    .avatar_state
                    .set_logical_state(AvatarLogicalState::Idle, None, None);
            AvatarStateServer::emit_snapshot(&app, &idle_snap);
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn get_persona_bio(state: State<'_, AppState>) -> SoulResult<PersonaBio> {
    let mem = state.active_memory();
    tokio::task::spawn_blocking(move || mem.get_persona_bio())
        .await
        .map_err(|e| SoulError::Internal(format!("Persona bio task failed: {e}")))?
}

#[tauri::command]
pub async fn update_persona_bio(
    app: AppHandle,
    state: State<'_, AppState>,
    bio: PersonaBio,
) -> SoulResult<PersonaBio> {
    let write_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Write, None, None);
    AvatarStateServer::emit_snapshot(&app, &write_snap);

    let mem = state.active_memory();
    let res = tokio::task::spawn_blocking(move || mem.update_persona_bio(bio))
        .await
        .map_err(|e| SoulError::Internal(format!("Persona bio update failed: {e}")));

    let idle_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Idle, None, None);
    AvatarStateServer::emit_snapshot(&app, &idle_snap);

    res?
}

#[tauri::command]
pub async fn get_user_profile(state: State<'_, AppState>) -> SoulResult<UserProfile> {
    let mem = state.active_memory();
    tokio::task::spawn_blocking(move || mem.get_user_profile())
        .await
        .map_err(|e| SoulError::Internal(format!("User profile task failed: {e}")))?
}

#[tauri::command]
pub async fn update_user_profile(
    app: AppHandle,
    state: State<'_, AppState>,
    profile: UserProfile,
) -> SoulResult<UserProfile> {
    let write_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Write, None, None);
    AvatarStateServer::emit_snapshot(&app, &write_snap);

    let mem = state.active_memory();
    let res = tokio::task::spawn_blocking(move || mem.update_user_profile(profile))
        .await
        .map_err(|e| SoulError::Internal(format!("User profile update failed: {e}")));

    let idle_snap = state
        .avatar_state
        .set_logical_state(AvatarLogicalState::Idle, None, None);
    AvatarStateServer::emit_snapshot(&app, &idle_snap);

    res?
}

#[tauri::command]
pub async fn get_avatar_state(state: State<'_, AppState>) -> SoulResult<AvatarSnapshot> {
    Ok(state.avatar_state.snapshot())
}

#[tauri::command]
pub async fn set_avatar_logical_state(
    app: AppHandle,
    state: State<'_, AppState>,
    logical_state: AvatarLogicalState,
    emotion: Option<EmotionTag>,
    intensity: Option<f32>,
) -> SoulResult<AvatarSnapshot> {
    if logical_state == AvatarLogicalState::Sleep {
        let _ = state.voice.stop_speaking().await;
    }

    let (emo_state, emo_profile) = if let Some(tag) = emotion {
        let clamped_intensity = intensity.unwrap_or(0.85).clamp(0.0, 1.0);
        let mem = state.active_memory();
        let profile = tokio::task::spawn_blocking(move || {
            mem.record_emotion_impulse(tag, clamped_intensity)
        })
        .await
        .ok()
        .and_then(Result::ok);

        let emo = EmotionState {
            tag,
            intensity: clamped_intensity,
            reason: format!("State changed to {logical_state}"),
            timestamp_ms: now_timestamp_ms(),
        };
        (Some(emo), profile)
    } else {
        (None, None)
    };

    if let Some(ref emo) = emo_state {
        let _ = app.emit(events::AVATAR_EMOTION, emo);
    }

    let snapshot = state
        .avatar_state
        .set_logical_state(logical_state, emo_state, emo_profile);
    AvatarStateServer::emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub async fn get_inner_loop_status(state: State<'_, AppState>) -> SoulResult<InnerLoopStatus> {
    Ok(state.inner_loop.status())
}

#[tauri::command]
pub async fn update_inner_loop_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: InnerLoopConfig,
) -> SoulResult<InnerLoopStatus> {
    let status = state.inner_loop.update_config(config);
    {
        let mut cfg = state
            .models_config
            .lock()
            .map_err(|_| SoulError::Internal("models_config mutex poisoned".into()))?;
        cfg.inner_loop = status.config.clone();
        cfg.save(&state.paths.config_path)?;
    }
    let _ = app.emit(events::INNER_LOOP_STATUS, &status);
    Ok(status)
}

#[tauri::command]
pub async fn trigger_inner_loop_beat(
    app: AppHandle,
    state: State<'_, AppState>,
) -> SoulResult<InnerLoopStatus> {
    let active_char = state.active_character_id();
    let mem = state.active_memory();
    let actions = state.plugin_registry.enabled_actions();
    Ok(state
        .inner_loop
        .run_beat(
            Some(&app),
            active_char,
            &mem,
            &state.brain,
            &state.voice,
            &state.avatar_state,
            actions,
        )
        .await)
}

#[tauri::command]
pub async fn list_plugins(state: State<'_, AppState>) -> SoulResult<Vec<PluginInfo>> {
    Ok(state.plugin_registry.list_plugins())
}

#[tauri::command]
pub async fn set_plugin_enabled(
    state: State<'_, AppState>,
    name: String,
    enabled: bool,
) -> SoulResult<Vec<PluginInfo>> {
    if !state.plugin_registry.set_enabled(&name, enabled) {
        return Err(SoulError::InvalidInput(format!(
            "Unknown plugin '{name}'"
        )));
    }
    Ok(state.plugin_registry.list_plugins())
}

#[tauri::command]
pub async fn execute_plugin_action(
    app: AppHandle,
    state: State<'_, AppState>,
    action: String,
    params: Option<serde_json::Value>,
) -> SoulResult<ActionResult> {
    let active_char = state.active_character_id();
    let mem = state.active_memory();
    let ctx = build_plugin_context(&mem, &state.avatar_state, Some(app), active_char).await;
    let payload = params.unwrap_or_else(|| serde_json::json!({}));
    Ok(state.plugin_registry.dispatch(&action, &payload, &ctx).await)
}

#[tauri::command]
pub async fn get_mcp_config(state: State<'_, AppState>) -> SoulResult<McpConfig> {
    Ok(state.mcp_manager.config())
}

#[tauri::command]
pub async fn update_mcp_config(
    state: State<'_, AppState>,
    config: McpConfig,
) -> SoulResult<McpConfig> {
    let mut effective = config;
    if let Some(ref path) = effective.config_path
        && effective.servers.is_empty()
        && !path.trim().is_empty()
    {
        let mut loaded = load_mcp_config_safe(Path::new(path));
        loaded.enabled = effective.enabled;
        loaded.config_path = Some(path.clone());
        effective = loaded;
    }

    state.mcp_manager.set_config(effective.clone());
    {
        let mut cfg = state
            .models_config
            .lock()
            .map_err(|_| SoulError::Internal("models_config mutex poisoned".into()))?;
        cfg.mcp = effective.clone();
        cfg.save(&state.paths.config_path)?;
    }

    if effective.enabled {
        let _ = tokio::time::timeout(Duration::from_secs(6), state.mcp_manager.refresh_tools()).await;
    }

    Ok(state.mcp_manager.config())
}

#[tauri::command]
pub async fn list_mcp_tools(state: State<'_, AppState>) -> SoulResult<Vec<McpToolInfo>> {
    Ok(state.mcp_manager.list_tools())
}

#[tauri::command]
pub async fn set_desktop_pet_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> SoulResult<bool> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_always_on_top(enabled)
            .map_err(|e| SoulError::Internal(e.to_string()))?;
        window
            .set_decorations(!enabled)
            .map_err(|e| SoulError::Internal(e.to_string()))?;
        window
            .set_shadow(!enabled)
            .map_err(|e| SoulError::Internal(e.to_string()))?;
    }
    let mut guard = state
        .desktop_pet_mode
        .lock()
        .map_err(|_| SoulError::Internal("desktop_pet_mode mutex poisoned".into()))?;
    *guard = enabled;
    Ok(enabled)
}

#[tauri::command]
pub async fn list_live2d_models(
    app: AppHandle,
    state: State<'_, AppState>,
) -> SoulResult<Vec<ModelInfo>> {
    list_models(app, state, Some(ModelKind::Live2d)).await
}

#[tauri::command]
pub async fn load_live2d_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    folder_path: String,
) -> SoulResult<ModelInfo> {
    let info = load_live2d_folder_inner(&app, &state, Path::new(&folder_path), None)?;
    if info.source == ModelSource::Custom {
        save_config_entry(&state, ModelKind::Live2d, &info)?;
    }
    Ok(info)
}

fn load_live2d_folder_inner(
    app: &AppHandle,
    state: &AppState,
    folder_path: &Path,
    name_override: Option<&str>,
) -> SoulResult<ModelInfo> {
    let (canon_folder, mut info) =
        inspect_live2d_folder(folder_path, ModelSource::Custom, None)?;
    if let Some(override_name) = name_override.map(str::trim).filter(|s| !s.is_empty()) {
        info.name = override_name.to_string();
    }
    info.is_active = true;

    let _ = app
        .asset_protocol_scope()
        .allow_directory(&canon_folder, true);

    {
        let mut active_guard = state
            .active_live2d_id
            .lock()
            .map_err(|_| SoulError::Internal("active_live2d_id mutex poisoned".into()))?;
        *active_guard = Some(info.id.clone());
    }

    // Check if this folder already corresponds to a bundled/user model before appending to custom.
    let (_, existing_bundled_or_user) = scan_live2d_models(
        &state.paths.bundled_live2d_dir,
        &state.paths.user_live2d_dir,
        &[],
        Some(&info.id),
    );
    if let Some(found) = existing_bundled_or_user
        .into_iter()
        .find(|m| m.path == info.path)
    {
        return Ok(found);
    }

    let mut models = state
        .registered_live2d_models
        .lock()
        .map_err(|_| SoulError::Internal("registered_live2d_models mutex poisoned".into()))?;
    for m in models.iter_mut() {
        m.is_active = false;
    }
    if let Some(existing) = models.iter_mut().find(|m| m.path == info.path) {
        *existing = info.clone();
    } else {
        models.push(info.clone());
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn per_character_coral_vs_kai_sqlite_profile_isolation() {
        let dir = std::env::temp_dir().join(format!("soulmate2d_char_iso_{}", new_id()));
        let coral_db = dir.join("profiles/coral/soul_memory.sqlite");
        let kai_db = dir.join("profiles/kai/soul_memory.sqlite");

        let coral_store = Arc::new(
            SoulMemoryStore::open_for_character(&coral_db, None, CharacterId::Coral)
                .expect("open coral store"),
        );
        let kai_store = Arc::new(
            SoulMemoryStore::open_for_character(&kai_db, None, CharacterId::Kai)
                .expect("open kai store"),
        );

        let paths = AppPaths {
            db_path: coral_db.clone(),
            coral_db_path: coral_db.clone(),
            kai_db_path: kai_db.clone(),
            config_path: dir.join("models_config.json"),
            bundled_llm_dir: dir.join("b_llm"),
            bundled_asr_dir: dir.join("b_asr"),
            bundled_embed_dir: dir.join("b_embed"),
            bundled_live2d_dir: dir.join("b_live2d"),
            user_llm_dir: dir.join("u_llm"),
            user_asr_dir: dir.join("u_asr"),
            user_embed_dir: dir.join("u_embed"),
            user_live2d_dir: dir.join("u_live2d"),
        };

        let brain = Arc::new(LlmBrainManager::with_dirs(&paths.bundled_llm_dir, &paths.user_llm_dir));
        let voice = Arc::new(VoicePipeline::with_asr_dirs(None, None));
        let initial_prof = coral_store.get_emotion_profile().unwrap();
        let avatar = Arc::new(AvatarStateServer::new(CharacterId::Coral, initial_prof));
        let inner_loop = Arc::new(InnerLoopManager::new(InnerLoopConfig::default()));
        let plugin_registry = Arc::new(PluginRegistry::new_with_builtins());
        let mcp_manager = Arc::new(McpClientManager::new(McpConfig::default()));

        let state = AppState::new(
            Arc::clone(&coral_store),
            Arc::clone(&kai_store),
            CharacterId::Coral,
            brain,
            voice,
            avatar,
            inner_loop,
            plugin_registry,
            mcp_manager,
            paths,
            ModelsConfig::default(),
            Vec::new(),
            Vec::new(),
            "coral_companion".to_string(),
        );

        // Write fact, diary, and emotion impulse exclusively to Coral's active store.
        let coral_mem = state.active_memory();
        assert_eq!(coral_mem.character_id(), CharacterId::Coral);
        coral_mem
            .upsert_fact(
                MemoryFact {
                    id: "f-coral-1".into(),
                    category: "preference".into(),
                    key: "tea".into(),
                    value: "Coral loves matcha".into(),
                    confidence: 0.95,
                    importance: 0.8,
                    memory_heat: 0.7,
                    access_count: 1,
                    created_at: 1_000,
                    last_accessed_at: 1_000,
                    superseded_by: None,
                    superseded_at: None,
                },
                None,
            )
            .await
            .unwrap();
        coral_mem
            .add_or_merge_diary_entry("Coral Day", "Had tea together", EmotionTag::Joy, "manual", None)
            .unwrap();
        let coral_emo = coral_mem.record_emotion_impulse(EmotionTag::Joy, 0.92).unwrap();
        assert_eq!(coral_emo.character_id, CharacterId::Coral);
        assert_eq!(coral_emo.dominant, EmotionTag::Joy);

        // Switch active character to Kai and verify complete SQLite isolation.
        {
            let mut active = state.active_character.write().unwrap();
            *active = CharacterId::Kai;
        }
        let kai_mem = state.active_memory();
        assert_eq!(kai_mem.character_id(), CharacterId::Kai);
        assert!(kai_mem.list_facts(50, true).await.unwrap().is_empty());
        assert!(kai_mem.list_diary_entries(20).unwrap().is_empty());
        assert_eq!(kai_mem.get_persona_bio().unwrap().character_id, CharacterId::Kai);

        // Write Kai-only data and verify Coral's database remains untouched.
        kai_mem
            .add_or_merge_diary_entry("Kai Day", "Listened to lo-fi", EmotionTag::Thinking, "manual", None)
            .unwrap();

        let coral_diaries = coral_mem.list_diary_entries(20).unwrap();
        assert_eq!(coral_diaries.len(), 1);
        assert_eq!(coral_diaries[0].title, "Coral Day");

        let kai_diaries = kai_mem.list_diary_entries(20).unwrap();
        assert_eq!(kai_diaries.len(), 1);
        assert_eq!(kai_diaries[0].title, "Kai Day");

        assert!(coral_db.exists());
        assert!(kai_db.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
