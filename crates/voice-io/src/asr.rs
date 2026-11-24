use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once, RwLock};

use serde::Deserialize;
use shared_types::{
    LanguagePref, ModelInfo, ModelKind, ModelSource, RamSafetyTier, SoulError, SoulResult,
    TranscriptEvent, now_timestamp_ms,
};
use tracing::{debug, info};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

static WHISPER_LOGGING_INIT: Once = Once::new();

/// Minimum size (in bytes) for a valid Whisper GGML binary model header.
const MIN_GGML_HEADER_BYTES: usize = 4;

/// Supported GGML / Whisper binary 4-byte magic headers.
///
/// Standard `ggml` files from `ggerganov/whisper.cpp` start with `0x67676d6c` in little-endian
/// (`b"lmgg"`). Newer variants (`ggmf`, `ggjt`, `ggla`, or `GGUF`) are also accepted.
const VALID_GGML_MAGICS: [[u8; 4]; 6] = [
    *b"lmgg", // 0x67676d6c LE (ggml-base.bin)
    *b"fmgg", // 0x67676d66 LE (ggmf)
    *b"tjgg", // 0x67676a74 LE (ggjt)
    *b"algg", // 0x67676c61 LE (ggla)
    *b"ggml", // big-endian / raw ASCII tag
    *b"GGUF", // GGUF container if used
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AsrManifest {
    id: Option<String>,
    name: Option<String>,
    filename: Option<String>,
    size_bytes: Option<u64>,
    estimated_ram_mb: Option<u64>,
    safety_tier: Option<RamSafetyTier>,
}

struct LoadedWhisper {
    info: ModelInfo,
    ctx: Arc<WhisperContext>,
}

/// Manages local Metal Whisper ASR model discovery, hot-swapping, and transcription.
pub struct AsrManager {
    bundled_dir: Mutex<Option<PathBuf>>,
    user_dir: Mutex<Option<PathBuf>>,
    custom_models: Mutex<Vec<ModelInfo>>,
    loaded: Arc<RwLock<Option<LoadedWhisper>>>,
    swap_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Default for AsrManager {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl AsrManager {
    pub fn new(bundled_dir: Option<PathBuf>, user_dir: Option<PathBuf>) -> Self {
        WHISPER_LOGGING_INIT.call_once(|| {
            whisper_rs::install_logging_hooks();
        });
        Self {
            bundled_dir: Mutex::new(bundled_dir.or_else(discover_default_bundled_asr_dir)),
            user_dir: Mutex::new(user_dir.or_else(discover_default_user_asr_dir)),
            custom_models: Mutex::new(Vec::new()),
            loaded: Arc::new(RwLock::new(None)),
            swap_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn set_directories(&self, bundled_dir: Option<PathBuf>, user_dir: Option<PathBuf>) {
        if let Ok(mut g) = self.bundled_dir.lock() {
            *g = bundled_dir;
        }
        if let Ok(mut g) = self.user_dir.lock() {
            *g = user_dir;
        }
    }

    pub fn active_model(&self) -> Option<ModelInfo> {
        self.loaded
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(|loaded| loaded.info.clone()))
    }

    /// Scan bundled, user, and custom ASR `.bin` models and validate their GGML magic headers.
    pub fn scan_asr_models(&self) -> SoulResult<Vec<ModelInfo>> {
        let active = self.active_model();
        let active_id = active.as_ref().map(|m| m.id.as_str());
        let active_path = active
            .as_ref()
            .and_then(|m| std::fs::canonicalize(&m.path).ok());

        let mut results = Vec::new();
        let mut seen_paths = HashSet::<PathBuf>::new();

        let bundled = self.bundled_dir.lock().ok().and_then(|g| g.clone());
        let user = self.user_dir.lock().ok().and_then(|g| g.clone());

        if let Some(dir) = bundled {
            scan_dir_into(&dir, ModelSource::Bundled, &mut seen_paths, &mut results);
        }
        if let Some(dir) = user {
            scan_dir_into(&dir, ModelSource::UserData, &mut seen_paths, &mut results);
        }

        if let Ok(custom) = self.custom_models.lock() {
            for item in custom.iter() {
                let path = PathBuf::from(&item.path);
                let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if seen_paths.insert(canon) {
                    let mut cloned = item.clone();
                    cloned.valid = has_valid_ggml_magic(&path);
                    results.push(cloned);
                }
            }
        }

        for model in &mut results {
            let canon = std::fs::canonicalize(&model.path).ok();
            model.is_active = active_id == Some(model.id.as_str())
                || (active_path.is_some() && canon == active_path);
        }

        Ok(results)
    }

    /// Register a custom `.bin` Whisper GGML model file after validating existence and magic bytes.
    pub fn register_custom_model(
        &self,
        path: &Path,
        name: Option<&str>,
    ) -> SoulResult<ModelInfo> {
        if !path.exists() || !path.is_file() {
            return Err(SoulError::ModelNotFound(format!(
                "ASR model file not found: {}",
                path.display()
            )));
        }
        if !has_valid_ggml_magic(path) {
            return Err(SoulError::InvalidInput(format!(
                "File does not have a valid Whisper GGML header: {}",
                path.display()
            )));
        }

        let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let estimated_ram_mb = estimate_asr_ram_mb(size_bytes);
        let safety_tier = classify_ram_safety(estimated_ram_mb);
        let display_name = name
            .map(ToString::to_string)
            .unwrap_or_else(|| default_display_name(path));
        let id = sanitize_model_id(
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("custom-asr"),
        );

        let info = ModelInfo {
            id,
            name: display_name,
            kind: ModelKind::Asr,
            path: path.to_string_lossy().into_owned(),
            size_bytes,
            estimated_ram_mb,
            source: ModelSource::Custom,
            safety_tier,
            is_active: false,
            valid: true,
            notes: Some("Custom local Whisper GGML model".into()),
        };

        let mut custom = self
            .custom_models
            .lock()
            .map_err(|_| SoulError::Internal("custom_models mutex poisoned".into()))?;
        if let Some(existing) = custom.iter_mut().find(|m| m.path == info.path) {
            *existing = info.clone();
        } else {
            custom.push(info.clone());
        }
        Ok(info)
    }

    /// Hot-swap the active Metal Whisper ASR model, strictly dropping any prior context first so
    /// two Whisper Metal models are never resident in Unified Memory simultaneously.
    pub async fn load_asr_model(&self, model: &ModelInfo) -> SoulResult<()> {
        if model.kind != ModelKind::Asr {
            return Err(SoulError::InvalidInput(format!(
                "Expected ModelKind::Asr, got {:?}",
                model.kind
            )));
        }

        let path = PathBuf::from(&model.path);
        if !path.exists() || !path.is_file() {
            return Err(SoulError::ModelNotFound(format!(
                "ASR model file not found: {}",
                path.display()
            )));
        }
        if !has_valid_ggml_magic(&path) {
            return Err(SoulError::Voice(format!(
                "Invalid GGML Whisper model header in {}",
                path.display()
            )));
        }

        let _swap_guard = self.swap_lock.lock().await;

        // Drop previous Metal Whisper context before loading the replacement.
        {
            let mut guard = self
                .loaded
                .write()
                .map_err(|_| SoulError::Internal("ASR context lock poisoned".into()))?;
            let old = guard.take();
            drop(guard);
            drop(old);
        }

        let load_path = path.clone();
        let ctx = tokio::task::spawn_blocking(move || {
            let mut params = WhisperContextParameters::default();
            params.use_gpu(true).flash_attn(false);
            WhisperContext::new_with_params(&load_path, params)
                .map(Arc::new)
                .map_err(|e| SoulError::Voice(format!("Failed to initialize Whisper Metal: {e}")))
        })
        .await
        .map_err(|e| SoulError::Internal(format!("Whisper model loader task failed: {e}")))??;

        let mut active_info = model.clone();
        active_info.is_active = true;
        active_info.valid = true;

        let mut guard = self
            .loaded
            .write()
            .map_err(|_| SoulError::Internal("ASR context lock poisoned".into()))?;
        *guard = Some(LoadedWhisper {
            info: active_info.clone(),
            ctx,
        });

        info!(
            model_id = %active_info.id,
            path = %active_info.path,
            "Loaded Metal Whisper ASR model"
        );
        Ok(())
    }

    /// Unload the currently active Whisper ASR context to release Metal memory.
    pub async fn unload_asr_model(&self) -> SoulResult<()> {
        let _swap_guard = self.swap_lock.lock().await;
        let mut guard = self
            .loaded
            .write()
            .map_err(|_| SoulError::Internal("ASR context lock poisoned".into()))?;
        let old = guard.take();
        drop(guard);
        drop(old);
        Ok(())
    }

    /// Synchronously transcribe a 16,000 Hz mono `f32` speech utterance using the loaded Whisper
    /// Metal context. Intended for invocation from a blocking worker thread or `spawn_blocking`.
    pub fn transcribe_pcm_blocking(
        &self,
        pcm_16k: &[f32],
        pref: LanguagePref,
    ) -> SoulResult<Option<TranscriptEvent>> {
        if pcm_16k.is_empty() {
            return Ok(None);
        }
        let ctx = self.active_context()?;
        transcribe_with_context(&ctx, pcm_16k, pref)
    }

    /// Async wrapper for transcribing an utterance on Tokio's blocking pool.
    pub async fn transcribe_pcm(
        &self,
        pcm_16k: Vec<f32>,
        pref: LanguagePref,
    ) -> SoulResult<Option<TranscriptEvent>> {
        let ctx = self.active_context()?;
        tokio::task::spawn_blocking(move || transcribe_with_context(&ctx, &pcm_16k, pref))
            .await
            .map_err(|e| SoulError::Internal(format!("Whisper task join failed: {e}")))?
    }

    fn active_context(&self) -> SoulResult<Arc<WhisperContext>> {
        let guard = self
            .loaded
            .read()
            .map_err(|_| SoulError::Internal("ASR context lock poisoned".into()))?;
        let Some(loaded) = guard.as_ref() else {
            return Err(SoulError::Voice(
                "No Whisper ASR model loaded; call load_asr_model first".into(),
            ));
        };
        Ok(Arc::clone(&loaded.ctx))
    }
}

fn transcribe_with_context(
    ctx: &WhisperContext,
    pcm_16k: &[f32],
    pref: LanguagePref,
) -> SoulResult<Option<TranscriptEvent>> {
    if pcm_16k.is_empty() {
        return Ok(None);
    }

    let mut state = ctx
        .create_state()
        .map_err(|e| SoulError::Voice(format!("Failed to create Whisper state: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 4))
        .unwrap_or(4) as std::ffi::c_int;
    params.set_n_threads(threads);
    params.set_translate(false);
    params.set_no_context(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_detect_language(false);

    match pref {
        LanguagePref::Thai => params.set_language(Some("th")),
        LanguagePref::English => params.set_language(Some("en")),
        LanguagePref::Auto => params.set_language(None),
    }

    state
        .full(params, pcm_16k)
        .map_err(|e| SoulError::Voice(format!("Whisper inference failed: {e}")))?;

    let mut text = String::new();
    for segment in state.as_iter() {
        let seg = segment
            .to_str_lossy()
            .map_err(|e| SoulError::Voice(format!("Whisper segment decode failed: {e}")))?;
        let cleaned = clean_whisper_segment(&seg);
        if !cleaned.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(cleaned);
        }
    }

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        debug!("Whisper produced no speech text for segment");
        return Ok(None);
    }

    let language = resolve_transcript_language(pref, &state, &trimmed);
    Ok(Some(TranscriptEvent {
        text: trimmed,
        language,
        is_final: true,
        timestamp_ms: now_timestamp_ms(),
    }))
}

fn resolve_transcript_language(
    pref: LanguagePref,
    state: &whisper_rs::WhisperState,
    text: &str,
) -> String {
    match pref {
        LanguagePref::Thai => "th".to_string(),
        LanguagePref::English => "en".to_string(),
        LanguagePref::Auto => {
            if text.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c)) {
                return "th".to_string();
            }
            whisper_rs::get_lang_str(state.full_lang_id_from_state())
                .unwrap_or("en")
                .to_string()
        }
    }
}

fn clean_whisper_segment(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "";
    }
    let upper = trimmed.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "[BLANK_AUDIO]" | "(SILENCE)" | "[SILENCE]" | "[MUSIC]" | "(MUSIC)" | "[NOISE]"
    ) {
        return "";
    }
    trimmed
}

/// Check whether `path` exists and begins with a recognized GGML / Whisper binary magic header.
pub fn has_valid_ggml_magic(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; MIN_GGML_HEADER_BYTES];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    VALID_GGML_MAGICS.iter().any(|expected| *expected == magic)
}

/// Estimate runtime RAM usage (MB) for a Whisper GGML model file.
pub fn estimate_asr_ram_mb(size_bytes: u64) -> u64 {
    let mb = size_bytes as f64 / (1024.0 * 1024.0);
    (mb * 1.6 + 140.0).round().max(140.0) as u64
}

/// Classify estimated RAM footprint relative to 8GB Unified Memory Macs.
pub fn classify_ram_safety(estimated_ram_mb: u64) -> RamSafetyTier {
    if estimated_ram_mb <= 3200 {
        RamSafetyTier::Safe8Gb
    } else if estimated_ram_mb <= 4500 {
        RamSafetyTier::Tight8Gb
    } else {
        RamSafetyTier::Recommended16GbPlus
    }
}

fn scan_dir_into(
    dir: &Path,
    source: ModelSource,
    seen_paths: &mut HashSet<PathBuf>,
    out: &mut Vec<ModelInfo>,
) {
    if !dir.is_dir() {
        return;
    }

    // Read optional manifest.json first for curated metadata.
    let manifest_path = dir.join("manifest.json");
    if let Ok(raw) = std::fs::read_to_string(&manifest_path) {
        if let Ok(manifest) = serde_json::from_str::<AsrManifest>(&raw) {
            if let Some(filename) = manifest.filename.as_deref() {
                let model_path = dir.join(filename);
                if model_path.is_file() {
                    let canon =
                        std::fs::canonicalize(&model_path).unwrap_or_else(|_| model_path.clone());
                    if seen_paths.insert(canon) {
                        let size_bytes = std::fs::metadata(&model_path)
                            .map(|m| m.len())
                            .or(manifest.size_bytes.ok_or(()))
                            .unwrap_or(0);
                        let estimated_ram_mb = manifest
                            .estimated_ram_mb
                            .unwrap_or_else(|| estimate_asr_ram_mb(size_bytes));
                        let valid = has_valid_ggml_magic(&model_path);
                        out.push(ModelInfo {
                            id: manifest
                                .id
                                .unwrap_or_else(|| sanitize_model_id(filename)),
                            name: manifest
                                .name
                                .unwrap_or_else(|| default_display_name(&model_path)),
                            kind: ModelKind::Asr,
                            path: model_path.to_string_lossy().into_owned(),
                            size_bytes,
                            estimated_ram_mb,
                            source,
                            safety_tier: manifest
                                .safety_tier
                                .unwrap_or_else(|| classify_ram_safety(estimated_ram_mb)),
                            is_active: false,
                            valid,
                            notes: None,
                        });
                    }
                }
            }
        }
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut bin_files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|ent| ent.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("bin"))
        })
        .collect();
    bin_files.sort();

    for model_path in bin_files {
        let canon = std::fs::canonicalize(&model_path).unwrap_or_else(|_| model_path.clone());
        if !seen_paths.insert(canon) {
            continue;
        }
        let size_bytes = std::fs::metadata(&model_path).map(|m| m.len()).unwrap_or(0);
        let estimated_ram_mb = estimate_asr_ram_mb(size_bytes);
        let valid = has_valid_ggml_magic(&model_path);
        let stem = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("whisper-asr");
        out.push(ModelInfo {
            id: sanitize_model_id(stem),
            name: default_display_name(&model_path),
            kind: ModelKind::Asr,
            path: model_path.to_string_lossy().into_owned(),
            size_bytes,
            estimated_ram_mb,
            source,
            safety_tier: classify_ram_safety(estimated_ram_mb),
            is_active: false,
            valid,
            notes: None,
        });
    }
}

fn default_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("whisper-model.bin")
        .to_string()
}

fn sanitize_model_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "whisper-asr".to_string()
    } else {
        trimmed
    }
}

fn discover_default_bundled_asr_dir() -> Option<PathBuf> {
    if let Ok(env_dir) = std::env::var("SOULMATE2D_ASR_DIR") {
        let p = PathBuf::from(env_dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    let mut candidates = vec![
        PathBuf::from("src-tauri/resources/models/asr"),
        PathBuf::from("resources/models/asr"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("resources/models/asr"));
            candidates.push(dir.join("../Resources/resources/models/asr"));
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest_dir.join("../../src-tauri/resources/models/asr"));

    candidates.into_iter().find(|p| p.is_dir())
}

fn discover_default_user_asr_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/Application Support/com.soulmate2d.app/models/asr")
    })
}
