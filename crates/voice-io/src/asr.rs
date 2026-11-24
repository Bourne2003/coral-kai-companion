use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_void};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use serde::Deserialize;
use shared_types::{
    LanguagePref, ModelInfo, ModelKind, ModelSource, RamSafetyTier, SoulError, SoulResult,
    TranscriptEvent, now_timestamp_ms,
};
use tracing::{debug, info};

#[repr(C)]
struct WhisperLoadResult {
    ctx: *mut c_void,
    err_ptr: *mut c_char,
}

#[repr(C)]
struct WhisperTranscribeResult {
    status: i32,
    text_ptr: *mut c_char,
    lang_ptr: *mut c_char,
    err_ptr: *mut c_char,
}

type FnAbiVersion = unsafe extern "C" fn() -> u32;
type FnLoad = unsafe extern "C" fn(*const c_char, bool) -> WhisperLoadResult;
type FnFreeLoadError = unsafe extern "C" fn(*mut c_char);
type FnFree = unsafe extern "C" fn(*mut c_void);
type FnTranscribe =
    unsafe extern "C" fn(*const c_void, *const f32, usize, u8) -> WhisperTranscribeResult;
type FnFreeResult = unsafe extern "C" fn(WhisperTranscribeResult);

struct WhisperBridgeApi {
    _lib: Box<dyn std::any::Any + Send + Sync>,
    load: FnLoad,
    free_load_error: FnFreeLoadError,
    free: FnFree,
    transcribe: FnTranscribe,
    free_result: FnFreeResult,
}

static WHISPER_BRIDGE: OnceLock<Result<WhisperBridgeApi, String>> = OnceLock::new();

fn load_bridge_api(bundled_asr_dir: Option<&Path>) -> SoulResult<&'static WhisperBridgeApi> {
    let res = WHISPER_BRIDGE.get_or_init(|| init_bridge_api(bundled_asr_dir));
    match res {
        Ok(api) => Ok(api),
        Err(msg) => Err(SoulError::Voice(msg.clone())),
    }
}

fn init_bridge_api(bundled_asr_dir: Option<&Path>) -> Result<WhisperBridgeApi, String> {
    let path = find_whisper_bridge_dylib(bundled_asr_dir).ok_or_else(|| {
        "libwhisper_bridge.dylib not found (build whisper-bridge or place it in bundled ASR dir)"
            .to_string()
    })?;

    #[cfg(unix)]
    let lib = unsafe {
        libloading::os::unix::Library::open(
            Some(&path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_LOCAL,
        )
        .map_err(|e| format!("Failed to dlopen {}: {e}", path.display()))?
    };

    #[cfg(not(unix))]
    let lib = unsafe {
        libloading::Library::new(&path)
            .map_err(|e| format!("Failed to load {}: {e}", path.display()))?
    };

    unsafe {
        let abi_ver = lib
            .get::<FnAbiVersion>(b"soulmate_whisper_abi_version\0")
            .map_err(|e| format!("Missing soulmate_whisper_abi_version: {e}"))?;
        let ver = abi_ver();
        if ver != 1 {
            return Err(format!("Unsupported whisper bridge ABI version: {ver}"));
        }

        let load: FnLoad = *lib
            .get::<FnLoad>(b"soulmate_whisper_load\0")
            .map_err(|e| format!("Missing soulmate_whisper_load: {e}"))?;
        let free_load_error: FnFreeLoadError = *lib
            .get::<FnFreeLoadError>(b"soulmate_whisper_free_load_error\0")
            .map_err(|e| format!("Missing soulmate_whisper_free_load_error: {e}"))?;
        let free: FnFree = *lib
            .get::<FnFree>(b"soulmate_whisper_free\0")
            .map_err(|e| format!("Missing soulmate_whisper_free: {e}"))?;
        let transcribe: FnTranscribe = *lib
            .get::<FnTranscribe>(b"soulmate_whisper_transcribe\0")
            .map_err(|e| format!("Missing soulmate_whisper_transcribe: {e}"))?;
        let free_result: FnFreeResult = *lib
            .get::<FnFreeResult>(b"soulmate_whisper_free_result\0")
            .map_err(|e| format!("Missing soulmate_whisper_free_result: {e}"))?;

        Ok(WhisperBridgeApi {
            _lib: Box::new(lib),
            load,
            free_load_error,
            free,
            transcribe,
            free_result,
        })
    }
}

fn find_whisper_bridge_dylib(bundled_asr_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(env_path) = std::env::var_os("SOULMATE2D_WHISPER_DYLIB") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }

    let dylib_name = if cfg!(target_os = "macos") {
        "libwhisper_bridge.dylib"
    } else if cfg!(target_os = "windows") {
        "whisper_bridge.dll"
    } else {
        "libwhisper_bridge.so"
    };

    // Check bundled runtime locations first (`src-tauri/resources/runtime/`, `resources/runtime/`,
    // and `../Resources/resources/runtime/` relative to the executable).
    for candidate in [
        PathBuf::from("src-tauri/resources/runtime").join(dylib_name),
        PathBuf::from("resources/runtime").join(dylib_name),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../src-tauri/resources/runtime")
            .join(dylib_name),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        let runtime_rel = exe_dir
            .join("../Resources/resources/runtime")
            .join(dylib_name);
        if runtime_rel.is_file() {
            return Some(runtime_rel);
        }
    }

    if let Some(dir) = bundled_asr_dir {
        let candidate = dir.join(dylib_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        for dir in [
            exe_dir.join("../Resources/resources/models/asr"),
            exe_dir.join("../Resources"),
            exe_dir.join("../Frameworks"),
            exe_dir.to_path_buf(),
            exe_dir.join(".."),
        ] {
            let candidate = dir.join(dylib_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for candidate in [
        PathBuf::from("src-tauri/resources/models/asr").join(dylib_name),
        PathBuf::from("resources/models/asr").join(dylib_name),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../src-tauri/resources/models/asr")
            .join(dylib_name),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

struct WhisperCtxHandle {
    ptr: *mut c_void,
    api: &'static WhisperBridgeApi,
}

unsafe impl Send for WhisperCtxHandle {}
unsafe impl Sync for WhisperCtxHandle {}

impl Drop for WhisperCtxHandle {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.api.free)(self.ptr) };
        }
    }
}

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
    ctx: Arc<WhisperCtxHandle>,
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

    /// Restore persisted custom `.bin` Whisper ASR entries by validating existence and GGML
    /// magic headers without loading them into Metal memory. Missing or invalid entries are skipped.
    pub fn restore_custom_models(&self, entries: &[(PathBuf, Option<String>)]) -> Vec<ModelInfo> {
        let mut restored = Vec::new();
        for (path, name) in entries {
            if let Ok(info) = self.register_custom_model(path, name.as_deref()) {
                restored.push(info);
            }
        }
        restored
    }

    /// Remove a custom ASR model entry by `id` or `path`.
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

        let bundled_dir_snapshot = self.bundled_dir.lock().ok().and_then(|g| g.clone());
        let api = load_bridge_api(bundled_dir_snapshot.as_deref())?;

        let load_path = path.clone();
        let ctx = tokio::task::spawn_blocking(move || {
            let path_str = load_path.to_str().ok_or_else(|| {
                SoulError::InvalidInput(format!(
                    "ASR model path is not valid UTF-8: {}",
                    load_path.display()
                ))
            })?;
            let c_path = CString::new(path_str).map_err(|_| {
                SoulError::InvalidInput("ASR model path contains interior NUL byte".into())
            })?;
            let load_res = unsafe { (api.load)(c_path.as_ptr(), true) };
            if load_res.ctx.is_null() {
                let err_msg = if load_res.err_ptr.is_null() {
                    "Unknown error initializing Whisper Metal".to_string()
                } else {
                    let s = unsafe { CStr::from_ptr(load_res.err_ptr) }
                        .to_string_lossy()
                        .into_owned();
                    unsafe { (api.free_load_error)(load_res.err_ptr) };
                    s
                };
                return Err(SoulError::Voice(err_msg));
            }
            Ok(Arc::new(WhisperCtxHandle {
                ptr: load_res.ctx,
                api,
            }))
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

    fn active_context(&self) -> SoulResult<Arc<WhisperCtxHandle>> {
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
    ctx: &WhisperCtxHandle,
    pcm_16k: &[f32],
    pref: LanguagePref,
) -> SoulResult<Option<TranscriptEvent>> {
    if pcm_16k.is_empty() {
        return Ok(None);
    }

    let lang_code: u8 = match pref {
        LanguagePref::Auto => 0,
        LanguagePref::Thai => 1,
        LanguagePref::English => 2,
    };

    let res = unsafe { (ctx.api.transcribe)(ctx.ptr, pcm_16k.as_ptr(), pcm_16k.len(), lang_code) };

    let out = match res.status {
        1 => {
            let text = if res.text_ptr.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(res.text_ptr) }
                    .to_string_lossy()
                    .trim()
                    .to_string()
            };
            let language = if res.lang_ptr.is_null() {
                "en".to_string()
            } else {
                unsafe { CStr::from_ptr(res.lang_ptr) }
                    .to_string_lossy()
                    .into_owned()
            };
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(TranscriptEvent {
                    text,
                    language,
                    is_final: true,
                    timestamp_ms: now_timestamp_ms(),
                }))
            }
        }
        0 => {
            debug!("Whisper produced no speech text for segment");
            Ok(None)
        }
        _ => {
            let err_msg = if res.err_ptr.is_null() {
                "Whisper inference failed".to_string()
            } else {
                unsafe { CStr::from_ptr(res.err_ptr) }
                    .to_string_lossy()
                    .into_owned()
            };
            Err(SoulError::Voice(err_msg))
        }
    };

    unsafe { (ctx.api.free_result)(res) };
    out
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
            .join("Library/Application Support/com.coralkai.companion/models/asr")
    })
}
