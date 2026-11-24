use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use brain_llm::LlmBrainManager;
use shared_types::{
    BrainEngine, BrainReply, ChatRole, ChatTurn, DreamDiary, EmotionState, EmotionTag,
    LanguagePref, MemoryEngine, MemoryFact, ModelInfo, ModelKind, ModelSource, RamSafetyTier,
    RelationshipState, SoulError, SoulResult, SystemRamStatus, VoiceEngine, VoiceMode, events,
    new_id, now_timestamp_ms,
};
use soul_memory::SoulMemoryStore;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;
use tracing::{info, warn};
use voice_io::VoicePipeline;

use crate::paths::{AppPaths, inspect_live2d_folder, scan_embed_models, scan_live2d_models};

/// Shared application state managed by Tauri across all 21 IPC commands and lifecycle hooks.
pub struct AppState {
    pub memory: Arc<SoulMemoryStore>,
    pub brain: Arc<LlmBrainManager>,
    pub voice: Arc<VoicePipeline>,
    pub paths: AppPaths,
    pub registered_embed_models: Mutex<Vec<ModelInfo>>,
    pub registered_live2d_models: Mutex<Vec<ModelInfo>>,
    pub active_live2d_id: Mutex<Option<String>>,
    pub desktop_pet_mode: Mutex<bool>,
    pub startup_done: Arc<AtomicBool>,
    pub startup_notify: Arc<Notify>,
    pub shutdown_done: Arc<AtomicBool>,
}

impl AppState {
    pub fn new(
        memory: Arc<SoulMemoryStore>,
        brain: Arc<LlmBrainManager>,
        voice: Arc<VoicePipeline>,
        paths: AppPaths,
    ) -> Self {
        Self {
            memory,
            brain,
            voice,
            paths,
            registered_embed_models: Mutex::new(Vec::new()),
            registered_live2d_models: Mutex::new(Vec::new()),
            active_live2d_id: Mutex::new(Some("default_companion".to_string())),
            desktop_pet_mode: Mutex::new(false),
            startup_done: Arc::new(AtomicBool::new(false)),
            startup_notify: Arc::new(Notify::new()),
            shutdown_done: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn current_ram_status(&self) -> SystemRamStatus {
        let mut status = self.brain.system_ram_status();
        status.active_asr_id = self.voice.active_asr_model().map(|m| m.id);
        status
    }
}

pub fn emit_ram_status(app: &AppHandle, state: &AppState) {
    let status = state.current_ram_status();
    let _ = app.emit(events::SYSTEM_RAM, status);
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

    // If background starter LLM load is still in progress on cold launch, wait briefly for it.
    if state.brain.active_model().is_none() && !state.startup_done.load(Ordering::Acquire) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            state.startup_notify.notified(),
        )
        .await;
    }

    let user_turn = ChatTurn {
        id: new_id(),
        role: ChatRole::User,
        content: trimmed.clone(),
        emotion: None,
        timestamp_ms: now_timestamp_ms(),
    };
    state.memory.append_turn(user_turn.clone()).await?;
    if let Err(err) = state.memory.extract_and_upsert_facts_from_turn(&user_turn).await {
        warn!("Fact extraction skipped for user turn: {err}");
    }

    let history = state.memory.recent_turns(20).await?;

    let mem_for_embed = Arc::clone(&state.memory);
    let text_for_embed = trimmed.clone();
    let query_embedding = tokio::task::spawn_blocking(move || mem_for_embed.embed_text(&text_for_embed))
        .await
        .ok()
        .and_then(Result::ok);

    let memory_ctx = state
        .memory
        .retrieve_context(&trimmed, query_embedding.as_deref(), 8, 4)
        .await?;

    let language_pref = state.voice.language_pref();
    let app_emotion = app.clone();
    let on_emotion = Box::new(move |emotion: EmotionState| {
        let _ = app_emotion.emit(events::AVATAR_EMOTION, &emotion);
    });

    let app_tokens = app.clone();
    let on_token = Box::new(move |chunk| {
        let _ = app_tokens.emit(events::CHAT_TOKEN, chunk);
    });

    let reply = state
        .brain
        .generate_reply_with_callbacks(
            &trimmed,
            &history,
            &memory_ctx,
            language_pref,
            on_emotion,
            on_token,
        )
        .await?;

    state.memory.append_turn(reply.turn.clone()).await?;

    // Gently evolve live relationship warmth/familiarity after a successful turn.
    if let Ok(mut rel) = state.memory.get_relationship_state().await {
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
        let _ = state.memory.update_relationship_state(rel).await;
    }

    // Trigger offline AVFoundation TTS + 60 FPS viseme streaming concurrently so IPC returns promptly.
    if !reply.turn.content.trim().is_empty() {
        let voice = Arc::clone(&state.voice);
        let spoken_text = reply.turn.content.clone();
        let spoken_emotion = reply.emotion.tag;
        let app_viseme = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = voice.stop_speaking().await;
            let app_cb = app_viseme.clone();
            let on_viseme = Box::new(move |frame| {
                let _ = app_cb.emit(events::AVATAR_VISEME, frame);
            });
            if let Err(err) = voice.speak(&spoken_text, spoken_emotion, on_viseme).await {
                warn!("Offline TTS playback error: {err}");
            }
        });
    }

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
        let custom_embed = state
            .registered_embed_models
            .lock()
            .map_err(|_| SoulError::Internal("registered_embed_models mutex poisoned".into()))?
            .clone();
        let onnx_active = state.memory.embedder_kind() == "onnx";
        results.extend(scan_embed_models(
            &state.paths.bundled_embed_dir,
            &state.paths.user_embed_dir,
            &custom_embed,
            onnx_active,
        ));
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
pub async fn register_custom_model(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: ModelKind,
    path: String,
    name: Option<String>,
) -> SoulResult<ModelInfo> {
    let p = PathBuf::from(&path);
    match kind {
        ModelKind::Llm => state.brain.register_custom_model(&p, name.as_deref()),
        ModelKind::Asr => state.voice.register_custom_asr_model(&p, name.as_deref()),
        ModelKind::Live2d => load_live2d_folder_inner(&app, &state, &p, name.as_deref()),
        ModelKind::Embed => register_custom_embed_model(&state, &p, name.as_deref()),
    }
}

fn register_custom_embed_model(
    state: &AppState,
    path: &Path,
    name: Option<&str>,
) -> SoulResult<ModelInfo> {
    if !path.exists() || !path.is_file() {
        return Err(SoulError::ModelNotFound(format!(
            "Embedding model file not found: {}",
            path.display()
        )));
    }
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
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
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("custom-embed")
                .to_string()
        });
    let info = ModelInfo {
        id: format!("custom:embed:{}", new_id()),
        name: display_name,
        kind: ModelKind::Embed,
        path: path.to_string_lossy().into_owned(),
        size_bytes,
        estimated_ram_mb,
        source: ModelSource::Custom,
        safety_tier,
        is_active: false,
        valid: true,
        notes: Some("Custom registered embedding model".to_string()),
    };

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
    let _ = app.emit(
        events::AVATAR_VISEME,
        shared_types::VisemeFrame {
            mouth_open_y: 0.0,
            mouth_form: 0.0,
            energy: 0.0,
            timestamp_ms: now_timestamp_ms(),
        },
    );
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
    let _ = app.emit(
        events::AVATAR_EMOTION,
        EmotionState {
            tag,
            intensity: if tag == EmotionTag::Neutral { 0.45 } else { 0.88 },
            reason: format!("Previewing {tag} expression & lip-sync"),
            timestamp_ms: now_timestamp_ms(),
        },
    );

    let voice = Arc::clone(&state.voice);
    let app_viseme = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = voice.stop_speaking().await;
        let app_cb = app_viseme.clone();
        let on_viseme = Box::new(move |frame| {
            let _ = app_cb.emit(events::AVATAR_VISEME, frame);
        });
        if let Err(err) = voice.speak(&trimmed, tag, on_viseme).await {
            warn!("Preview speech & visemes error: {err}");
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
        .memory
        .list_facts(limit.unwrap_or(100), include_superseded.unwrap_or(false))
        .await
}

#[tauri::command]
pub async fn list_dream_diaries(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> SoulResult<Vec<DreamDiary>> {
    state.memory.list_dream_diaries(limit.unwrap_or(30)).await
}

#[tauri::command]
pub async fn get_relationship_state(
    state: State<'_, AppState>,
) -> SoulResult<RelationshipState> {
    state.memory.get_relationship_state().await
}

#[tauri::command]
pub async fn consolidate_dreams_now(
    state: State<'_, AppState>,
) -> SoulResult<Option<DreamDiary>> {
    state.memory.consolidate_now().await
}

#[tauri::command]
pub async fn export_soul_memory(
    state: State<'_, AppState>,
    target_path: String,
) -> SoulResult<String> {
    state
        .memory
        .export_snapshot(&PathBuf::from(target_path))
        .await
}

#[tauri::command]
pub async fn import_soul_memory(
    state: State<'_, AppState>,
    source_path: String,
) -> SoulResult<usize> {
    state
        .memory
        .import_snapshot(&PathBuf::from(source_path))
        .await
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
    load_live2d_folder_inner(&app, &state, Path::new(&folder_path), None)
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
