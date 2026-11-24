use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shared_types::{
    BrainEngine, BrainReply, ChatRole, ChatTurn, DreamDiary, EmotionTag, LanguagePref,
    MemoryEngine, MemoryFact, ModelInfo, ModelKind, ModelSource, RamSafetyTier, RelationshipState,
    SoulError, SoulResult, SystemRamStatus, VoiceEngine, VoiceMode, events, new_id,
    now_timestamp_ms,
};
use tauri::{AppHandle, Emitter, Manager, State};

/// Shared application state managed by Tauri.
pub struct AppState {
    pub memory: Arc<dyn MemoryEngine>,
    pub brain: Arc<dyn BrainEngine>,
    pub voice: Arc<dyn VoiceEngine>,
    pub registered_models: Mutex<Vec<ModelInfo>>,
    pub desktop_pet_mode: Mutex<bool>,
}

impl AppState {
    pub fn new(
        memory: Arc<dyn MemoryEngine>,
        brain: Arc<dyn BrainEngine>,
        voice: Arc<dyn VoiceEngine>,
    ) -> Self {
        Self {
            memory,
            brain,
            voice,
            registered_models: Mutex::new(Vec::new()),
            desktop_pet_mode: Mutex::new(false),
        }
    }
}

#[tauri::command]
pub async fn send_chat_message(
    app: AppHandle,
    state: State<'_, AppState>,
    message: String,
) -> SoulResult<BrainReply> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(SoulError::InvalidInput("Message cannot be empty".into()));
    }

    let user_turn = ChatTurn {
        id: new_id(),
        role: ChatRole::User,
        content: trimmed.to_string(),
        emotion: None,
        timestamp_ms: now_timestamp_ms(),
    };
    state.memory.append_turn(user_turn).await?;

    let history = state.memory.recent_turns(20).await?;
    let query_embedding = state.brain.embed_text(trimmed).await.ok();
    let memory_ctx = state
        .memory
        .retrieve_context(trimmed, query_embedding.as_deref(), 8, 4)
        .await?;

    let app_handle = app.clone();
    let on_token = Box::new(move |chunk| {
        let _ = app_handle.emit(events::CHAT_TOKEN, chunk);
    });

    let reply = state
        .brain
        .generate_reply(trimmed, &history, &memory_ctx, on_token)
        .await?;

    let _ = app.emit(events::AVATAR_EMOTION, &reply.emotion);
    state.memory.append_turn(reply.turn.clone()).await?;
    Ok(reply)
}

#[tauri::command]
pub async fn get_ram_status(state: State<'_, AppState>) -> SoulResult<SystemRamStatus> {
    let mut status = state.brain.system_ram_status();
    status.active_asr_id = state.voice.active_asr_model().map(|m| m.id);
    Ok(status)
}

#[tauri::command]
pub async fn list_models(
    state: State<'_, AppState>,
    kind: Option<ModelKind>,
) -> SoulResult<Vec<ModelInfo>> {
    let models = state
        .registered_models
        .lock()
        .map_err(|_| SoulError::Internal("registered_models mutex poisoned".into()))?;
    let active_llm = state.brain.active_model().map(|m| m.id);
    let active_asr = state.voice.active_asr_model().map(|m| m.id);

    let filtered = models
        .iter()
        .filter(|m| kind.is_none_or(|k| m.kind == k))
        .cloned()
        .map(|mut m| {
            m.is_active = match m.kind {
                ModelKind::Llm => active_llm.as_deref() == Some(m.id.as_str()),
                ModelKind::Asr => active_asr.as_deref() == Some(m.id.as_str()),
                _ => m.is_active,
            };
            m
        })
        .collect();
    Ok(filtered)
}

#[tauri::command]
pub async fn hot_swap_llm(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> SoulResult<ModelInfo> {
    let target = {
        let models = state
            .registered_models
            .lock()
            .map_err(|_| SoulError::Internal("registered_models mutex poisoned".into()))?;
        models
            .iter()
            .find(|m| m.id == model_id && m.kind == ModelKind::Llm)
            .cloned()
            .ok_or_else(|| SoulError::ModelNotFound(format!("LLM model not found: {model_id}")))?
    };

    state.brain.unload_model().await?;
    state.brain.load_model(&target).await?;

    let mut status = state.brain.system_ram_status();
    status.active_asr_id = state.voice.active_asr_model().map(|m| m.id);
    let _ = app.emit(events::SYSTEM_RAM, status);

    let mut active = target;
    active.is_active = true;
    Ok(active)
}

#[tauri::command]
pub async fn switch_asr_model(
    state: State<'_, AppState>,
    model_id: String,
) -> SoulResult<ModelInfo> {
    let target = {
        let models = state
            .registered_models
            .lock()
            .map_err(|_| SoulError::Internal("registered_models mutex poisoned".into()))?;
        models
            .iter()
            .find(|m| m.id == model_id && m.kind == ModelKind::Asr)
            .cloned()
            .ok_or_else(|| SoulError::ModelNotFound(format!("ASR model not found: {model_id}")))?
    };

    state.voice.load_asr_model(&target).await?;
    let mut active = target;
    active.is_active = true;
    Ok(active)
}

#[tauri::command]
pub async fn register_custom_model(
    state: State<'_, AppState>,
    kind: ModelKind,
    path: String,
    name: Option<String>,
) -> SoulResult<ModelInfo> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(SoulError::ModelNotFound(format!(
            "Custom model path does not exist: {path}"
        )));
    }

    let size_bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    let estimated_ram_mb = (size_bytes / (1024 * 1024)).saturating_add(512);
    let safety_tier = if estimated_ram_mb <= 3200 {
        RamSafetyTier::Safe8Gb
    } else if estimated_ram_mb <= 4500 {
        RamSafetyTier::Tight8Gb
    } else {
        RamSafetyTier::Recommended16GbPlus
    };

    let display_name = name.unwrap_or_else(|| {
        p.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("custom-model")
            .to_string()
    });

    let info = ModelInfo {
        id: new_id(),
        name: display_name,
        kind,
        path,
        size_bytes,
        estimated_ram_mb,
        source: ModelSource::Custom,
        safety_tier,
        is_active: false,
        valid: true,
        notes: None,
    };

    let mut models = state
        .registered_models
        .lock()
        .map_err(|_| SoulError::Internal("registered_models mutex poisoned".into()))?;
    models.push(info.clone());
    Ok(info)
}

#[tauri::command]
pub async fn set_voice_mode(state: State<'_, AppState>, mode: VoiceMode) -> SoulResult<()> {
    state.voice.set_voice_mode(mode).await
}

#[tauri::command]
pub async fn set_language_pref(
    state: State<'_, AppState>,
    pref: LanguagePref,
) -> SoulResult<()> {
    state.voice.set_language_pref(pref).await
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
pub async fn stop_speaking(state: State<'_, AppState>) -> SoulResult<()> {
    state.voice.stop_speaking().await
}

#[tauri::command]
pub async fn preview_speech_and_visemes(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    emotion: Option<EmotionTag>,
) -> SoulResult<()> {
    let app_handle = app.clone();
    let on_viseme = Box::new(move |frame| {
        let _ = app_handle.emit(events::AVATAR_VISEME, frame);
    });
    state
        .voice
        .speak(&text, emotion.unwrap_or_default(), on_viseme)
        .await
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
    }
    let mut guard = state
        .desktop_pet_mode
        .lock()
        .map_err(|_| SoulError::Internal("desktop_pet_mode mutex poisoned".into()))?;
    *guard = enabled;
    Ok(enabled)
}

#[tauri::command]
pub async fn list_live2d_models(state: State<'_, AppState>) -> SoulResult<Vec<ModelInfo>> {
    list_models(state, Some(ModelKind::Live2d)).await
}

#[tauri::command]
pub async fn load_live2d_folder(
    state: State<'_, AppState>,
    folder_path: String,
) -> SoulResult<ModelInfo> {
    register_custom_model(state, ModelKind::Live2d, folder_path, None).await
}
