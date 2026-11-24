use std::collections::HashMap;
use std::ffi::CString;
use std::fmt::Display;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::RANGE;
use shared_types::{
    DownloadProgress, DownloadStatus, ModelInfo, ModelKind, RamSafetyTier, SoulError, SoulResult,
    StarterAssetInfo,
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::commands::{AppState, emit_ram_status};

const DISK_HEADROOM_BYTES: u64 = 512 * 1024 * 1024;
const PROGRESS_THROTTLE_MS: u64 = 100;
const MIN_LLM_BYTES: u64 = 500 * 1024 * 1024;
const MIN_ASR_BYTES: u64 = 50 * 1024 * 1024;
const GGUF_MAGIC: [u8; 4] = *b"GGUF";
const GGML_MAGIC_LE: u32 = 0x6767_6D6C;

#[derive(Debug, Clone)]
struct StarterCatalogEntry {
    id: &'static str,
    name: &'static str,
    description_en: &'static str,
    description_th: &'static str,
    kind: ModelKind,
    repo: &'static str,
    filename: &'static str,
    approx_size_bytes: u64,
    estimated_ram_mb: u64,
    safety_tier: RamSafetyTier,
    is_primary: bool,
    license: &'static str,
}

const STARTER_CATALOG: &[StarterCatalogEntry] = &[
    StarterCatalogEntry {
        id: "typhoon25-qwen3-4b-q4km",
        name: "Typhoon 2.5 Qwen3 4B Instruct (Q4_K_M)",
        description_en: "Bilingual Thai/English flagship 4B instruct model tuned for natural conversation on 8GB Apple Silicon.",
        description_th: "โมเดลหลัก 4B รองรับภาษาไทย/อังกฤษเป็นธรรมชาติ เหมาะสำหรับ Mac M1 8GB",
        kind: ModelKind::Llm,
        repo: "typhoon-ai/typhoon2.5-qwen3-4b-gguf",
        filename: "typhoon2.5-qwen3-4b-q4_k_m.gguf",
        approx_size_bytes: 2_680_000_000,
        estimated_ram_mb: 3150,
        safety_tier: RamSafetyTier::Safe8Gb,
        is_primary: true,
        license: "Apache-2.0",
    },
    StarterCatalogEntry {
        id: "qwen25-3b-instruct-q4km",
        name: "Qwen 2.5 3B Instruct (Q4_K_M)",
        description_en: "Lightweight 3B multilingual instruct fallback with fast inference and low memory footprint.",
        description_th: "โมเดลทางเลือก 3B ขนาดกะทัดรัด ตอบสนองไวและประหยัดหน่วยความจำ",
        kind: ModelKind::Llm,
        repo: "Qwen/Qwen2.5-3B-Instruct-GGUF",
        filename: "qwen2.5-3b-instruct-q4_k_m.gguf",
        approx_size_bytes: 2_100_000_000,
        estimated_ram_mb: 2650,
        safety_tier: RamSafetyTier::Safe8Gb,
        is_primary: false,
        license: "Qwen License",
    },
    StarterCatalogEntry {
        id: "whisper-ggml-base",
        name: "Whisper Base (Multilingual Thai/English)",
        description_en: "Local speech-to-text model for Thai and English voice input via Metal Whisper.",
        description_th: "โมเดลถอดเสียงภาษาไทย/อังกฤษแบบออฟไลน์สำหรับการใช้งานไมโครโฟน",
        kind: ModelKind::Asr,
        repo: "ggerganov/whisper.cpp",
        filename: "ggml-base.bin",
        approx_size_bytes: 147_951_465,
        estimated_ram_mb: 384,
        safety_tier: RamSafetyTier::Safe8Gb,
        is_primary: true,
        license: "MIT",
    },
];

/// Opt-in manager for downloading curated starter models on explicit user action.
///
/// Never constructs a `reqwest::Client` at construction or startup; an HTTP client is
/// created lazily inside [`StarterDownloadManager::download_starter_asset`] only.
pub struct StarterDownloadManager {
    active_tokens: Mutex<HashMap<String, CancellationToken>>,
    client_initialized_count: AtomicUsize,
}

impl Default for StarterDownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StarterDownloadManager {
    /// Create a zero-network `StarterDownloadManager` ready for offline catalog scans.
    pub fn new() -> Self {
        Self {
            active_tokens: Mutex::new(HashMap::new()),
            client_initialized_count: AtomicUsize::new(0),
        }
    }

    /// Number of times a lazy `reqwest::Client` has been initialized (0 while purely offline).
    pub fn client_initialized_count(&self) -> usize {
        self.client_initialized_count.load(Ordering::Acquire)
    }

    /// Cancel an active starter asset download by ID, returning `true` if a token was cancelled.
    pub fn cancel_download(&self, asset_id: &str) -> bool {
        let Ok(guard) = self.active_tokens.lock() else {
            return false;
        };
        if let Some(token) = guard.get(asset_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Download, verify, and atomically install a curated starter asset into user model storage.
    pub async fn download_starter_asset<F>(
        &self,
        app: &AppHandleProxy<'_>,
        state: &AppState,
        asset_id: &str,
        mut on_progress: F,
    ) -> SoulResult<ModelInfo>
    where
        F: FnMut(DownloadProgress) + Send,
    {
        let entry = STARTER_CATALOG
            .iter()
            .find(|item| item.id == asset_id)
            .ok_or_else(|| {
                SoulError::InvalidInput(format!(
                    "ไม่พบโมเดลเริ่มต้น '{asset_id}' / Unknown starter asset id: {asset_id}"
                ))
            })?;

        let cancel_token = CancellationToken::new();
        let _active_guard =
            ActiveDownloadGuard::acquire(&self.active_tokens, asset_id, cancel_token.clone())?;

        let target_dir = match entry.kind {
            ModelKind::Llm => &state.paths.user_llm_dir,
            ModelKind::Asr => &state.paths.user_asr_dir,
            other => {
                return Err(SoulError::InvalidInput(format!(
                    "Unsupported starter asset kind: {other:?}"
                )));
            }
        };

        if let Err(err) = check_available_disk_space(target_dir, entry.approx_size_bytes) {
            emit_error(&mut on_progress, asset_id, 0, entry.approx_size_bytes, &err);
            return Err(err);
        }

        on_progress(DownloadProgress {
            asset_id: asset_id.to_string(),
            downloaded_bytes: 0,
            total_bytes: entry.approx_size_bytes,
            speed_mbps: 0.0,
            status: DownloadStatus::Resolving,
            message: Some(format!(
                "กำลังตรวจสอบไฟล์จาก Hugging Face ({}) / Resolving {} from Hugging Face...",
                entry.repo, entry.repo
            )),
        });

        let client = reqwest::Client::builder()
            .user_agent("CoralKaiCompanion/0.2.0")
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                let err = SoulError::Internal(format!("Failed to initialize HTTP client: {e}"));
                emit_error(&mut on_progress, asset_id, 0, entry.approx_size_bytes, &err);
                err
            })?;
        self.client_initialized_count.fetch_add(1, Ordering::AcqRel);

        let resolved = tokio::select! {
            _ = cancel_token.cancelled() => {
                emit_cancelled(&mut on_progress, asset_id, 0, entry.approx_size_bytes);
                return Err(cancelled_error());
            }
            res = resolve_huggingface_asset(&client, entry) => res,
        };

        let (resolved_rfilename, exact_size, download_url) = match resolved {
            Ok(v) => v,
            Err(err) => {
                emit_error(&mut on_progress, asset_id, 0, entry.approx_size_bytes, &err);
                return Err(err);
            }
        };

        let total_expected = exact_size.unwrap_or(entry.approx_size_bytes);
        if let Err(err) = check_available_disk_space(target_dir, total_expected) {
            emit_error(&mut on_progress, asset_id, 0, total_expected, &err);
            return Err(err);
        }

        let base_filename = Path::new(&resolved_rfilename)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(entry.filename);
        let final_path = target_dir.join(base_filename);
        let part_path = PathBuf::from(format!("{}.part", final_path.display()));

        let mut existing_bytes = tokio::fs::metadata(&part_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if let Some(expected) = exact_size
            && expected > 0
            && existing_bytes > expected
        {
            let _ = tokio::fs::remove_file(&part_path).await;
            existing_bytes = 0;
        }

        let skip_transfer = exact_size.is_some_and(|exp| exp > 0 && existing_bytes == exp);
        let mut final_total = total_expected;

        if !skip_transfer {
            let mut req = client.get(&download_url);
            if existing_bytes > 0 {
                req = req.header(RANGE, format!("bytes={existing_bytes}-"));
            }

            let resp_res = tokio::select! {
                _ = cancel_token.cancelled() => {
                    let _ = tokio::fs::remove_file(&part_path).await;
                    emit_cancelled(&mut on_progress, asset_id, existing_bytes, total_expected);
                    return Err(cancelled_error());
                }
                res = req.send() => res,
            };

            let resp = match resp_res {
                Ok(r) => r,
                Err(e) => {
                    let err = SoulError::Io(format!(
                        "เชื่อมต่อดาวน์โหลดไม่สำเร็จ / Failed to connect to download URL: {e}"
                    ));
                    emit_error(&mut on_progress, asset_id, existing_bytes, total_expected, &err);
                    return Err(err);
                }
            };

            match resp.status() {
                StatusCode::PARTIAL_CONTENT => {
                    let append_total = exact_size.unwrap_or_else(|| {
                        existing_bytes.saturating_add(resp.content_length().unwrap_or(0))
                    });
                    final_total = append_total.max(existing_bytes);
                    on_progress(DownloadProgress {
                        asset_id: asset_id.to_string(),
                        downloaded_bytes: existing_bytes,
                        total_bytes: final_total,
                        speed_mbps: 0.0,
                        status: DownloadStatus::Downloading,
                        message: Some(format!(
                            "กำลังดาวน์โหลดต่อจากจุดเดิม ({:.1} MB) / Resuming download...",
                            existing_bytes as f64 / (1024.0 * 1024.0)
                        )),
                    });

                    let stream = resp.bytes_stream();
                    if let Err(err) = write_chunk_stream_with_cancel(
                        stream,
                        &part_path,
                        true,
                        existing_bytes,
                        final_total,
                        asset_id,
                        &cancel_token,
                        &mut on_progress,
                    )
                    .await
                    {
                        if !cancel_token.is_cancelled() {
                            emit_error(&mut on_progress, asset_id, existing_bytes, final_total, &err);
                        }
                        return Err(err);
                    }
                }
                StatusCode::OK => {
                    final_total = exact_size
                        .or_else(|| resp.content_length())
                        .unwrap_or(entry.approx_size_bytes);
                    on_progress(DownloadProgress {
                        asset_id: asset_id.to_string(),
                        downloaded_bytes: 0,
                        total_bytes: final_total,
                        speed_mbps: 0.0,
                        status: DownloadStatus::Downloading,
                        message: Some(
                            "กำลังดาวน์โหลดไฟล์โมเดล / Downloading model weights...".to_string(),
                        ),
                    });

                    let stream = resp.bytes_stream();
                    if let Err(err) = write_chunk_stream_with_cancel(
                        stream,
                        &part_path,
                        false,
                        0,
                        final_total,
                        asset_id,
                        &cancel_token,
                        &mut on_progress,
                    )
                    .await
                    {
                        if !cancel_token.is_cancelled() {
                            emit_error(&mut on_progress, asset_id, 0, final_total, &err);
                        }
                        return Err(err);
                    }
                }
                StatusCode::RANGE_NOT_SATISFIABLE => {
                    final_total = exact_size.unwrap_or(existing_bytes);
                }
                status => {
                    let err = SoulError::Io(format!(
                        "เซิร์ฟเวอร์ตอบกลับผิดพลาด (HTTP {status}) / Download failed with HTTP status {status}"
                    ));
                    emit_error(&mut on_progress, asset_id, existing_bytes, total_expected, &err);
                    return Err(err);
                }
            }
        }

        let part_size = tokio::fs::metadata(&part_path)
            .await
            .map(|m| m.len())
            .unwrap_or(final_total);

        on_progress(DownloadProgress {
            asset_id: asset_id.to_string(),
            downloaded_bytes: part_size,
            total_bytes: final_total.max(part_size),
            speed_mbps: 0.0,
            status: DownloadStatus::Verifying,
            message: Some(
                "กำลังตรวจสอบขนาดและหัวไฟล์โมเดล / Verifying file size and magic bytes..."
                    .to_string(),
            ),
        });

        let min_size = match entry.kind {
            ModelKind::Llm => MIN_LLM_BYTES,
            ModelKind::Asr => MIN_ASR_BYTES,
            _ => 1,
        };
        let verify_path = part_path.clone();
        let verify_kind = entry.kind;
        let verify_res = tokio::task::spawn_blocking(move || {
            verify_downloaded_file(&verify_path, verify_kind, exact_size, min_size)
        })
        .await
        .map_err(|e| SoulError::Internal(format!("Verification task failed: {e}")))?;

        if let Err(err) = verify_res {
            let _ = tokio::fs::remove_file(&part_path).await;
            emit_error(&mut on_progress, asset_id, 0, final_total, &err);
            return Err(err);
        }

        if let Err(e) = tokio::fs::rename(&part_path, &final_path).await {
            let err = SoulError::Io(format!(
                "ย้ายไฟล์โมเดลไม่สำเร็จ / Failed to finalize model file {}: {e}",
                final_path.display()
            ));
            emit_error(&mut on_progress, asset_id, part_size, final_total, &err);
            return Err(err);
        }

        let canon_final = fs::canonicalize(&final_path).unwrap_or_else(|_| final_path.clone());
        let discovered = match entry.kind {
            ModelKind::Llm => state.brain.scan_models()?,
            ModelKind::Asr => state.voice.scan_asr_models()?,
            _ => Vec::new(),
        };

        let model_info = discovered
            .into_iter()
            .find(|m| {
                fs::canonicalize(&m.path).unwrap_or_else(|_| PathBuf::from(&m.path)) == canon_final
                    || Path::new(&m.path).file_name() == final_path.file_name()
            })
            .ok_or_else(|| {
                SoulError::ModelNotFound(format!(
                    "ดาวน์โหลดสำเร็จแต่ไม่พบโมเดลหลังสแกน / Downloaded file missing after scan: {}",
                    final_path.display()
                ))
            })?;

        on_progress(DownloadProgress {
            asset_id: asset_id.to_string(),
            downloaded_bytes: model_info.size_bytes,
            total_bytes: model_info.size_bytes,
            speed_mbps: 0.0,
            status: DownloadStatus::Completed,
            message: Some(
                "ดาวน์โหลดและตรวจสอบโมเดลเสร็จสมบูรณ์ / Model downloaded and verified".to_string(),
            ),
        });

        app.emit_ram(state);
        info!(
            asset_id = %asset_id,
            model_id = %model_info.id,
            path = %model_info.path,
            "Installed curated starter model"
        );
        Ok(model_info)
    }
}

/// Thin wrapper so [`StarterDownloadManager::download_starter_asset`] emits RAM updates cleanly.
pub struct AppHandleProxy<'a>(pub &'a tauri::AppHandle);

impl AppHandleProxy<'_> {
    fn emit_ram(&self, state: &AppState) {
        emit_ram_status(self.0, state);
    }
}

struct ActiveDownloadGuard<'a> {
    map: &'a Mutex<HashMap<String, CancellationToken>>,
    asset_id: String,
}

impl<'a> ActiveDownloadGuard<'a> {
    fn acquire(
        map: &'a Mutex<HashMap<String, CancellationToken>>,
        asset_id: &str,
        token: CancellationToken,
    ) -> SoulResult<Self> {
        let mut guard = map
            .lock()
            .map_err(|_| SoulError::Internal("active_tokens mutex poisoned".into()))?;
        if guard.contains_key(asset_id) {
            return Err(SoulError::InvalidInput(format!(
                "กำลังดาวน์โหลดโมเดล '{asset_id}' อยู่แล้ว / Download already active for '{asset_id}'"
            )));
        }
        guard.insert(asset_id.to_string(), token);
        Ok(Self {
            map,
            asset_id: asset_id.to_string(),
        })
    }
}

impl Drop for ActiveDownloadGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.map.lock() {
            guard.remove(&self.asset_id);
        }
    }
}

/// Build the curated starter catalog offline using local directories and discovered models.
pub fn curated_starter_catalog(
    user_llm_dir: &Path,
    user_asr_dir: &Path,
    discovered_llm: &[ModelInfo],
    discovered_asr: &[ModelInfo],
) -> Vec<StarterAssetInfo> {
    STARTER_CATALOG
        .iter()
        .map(|entry| {
            let (user_dir, discovered) = match entry.kind {
                ModelKind::Llm => (user_llm_dir, discovered_llm),
                ModelKind::Asr => (user_asr_dir, discovered_asr),
                _ => (user_llm_dir, discovered_llm),
            };
            let expected_path = user_dir.join(entry.filename);
            let matched = find_matching_discovered(entry, &expected_path, discovered);

            let (installed, installed_model_id, installed_path) = if let Some(m) = matched {
                (true, Some(m.id.clone()), Some(m.path.clone()))
            } else if expected_path.is_file() {
                let canon = fs::canonicalize(&expected_path)
                    .unwrap_or_else(|_| expected_path.clone())
                    .to_string_lossy()
                    .into_owned();
                let fallback_id = fallback_model_id(entry.kind, &expected_path);
                (true, Some(fallback_id), Some(canon))
            } else {
                (false, None, None)
            };

            StarterAssetInfo {
                id: entry.id.to_string(),
                name: entry.name.to_string(),
                description_en: entry.description_en.to_string(),
                description_th: entry.description_th.to_string(),
                kind: entry.kind,
                repo: entry.repo.to_string(),
                filename: entry.filename.to_string(),
                approx_size_bytes: entry.approx_size_bytes,
                estimated_ram_mb: entry.estimated_ram_mb,
                safety_tier: entry.safety_tier,
                is_primary: entry.is_primary,
                installed,
                installed_model_id,
                installed_path,
                license: entry.license.to_string(),
            }
        })
        .collect()
}

fn find_matching_discovered<'a>(
    entry: &StarterCatalogEntry,
    expected_path: &Path,
    discovered: &'a [ModelInfo],
) -> Option<&'a ModelInfo> {
    let expected_canon = fs::canonicalize(expected_path).ok();
    let expected_filename_lower = entry.filename.to_ascii_lowercase();

    let mut fallback: Option<&ModelInfo> = None;
    for model in discovered {
        let model_path = Path::new(&model.path);
        let path_matches = expected_canon.as_ref().is_some_and(|exp| {
            fs::canonicalize(model_path).ok().as_ref() == Some(exp)
        });
        let file_name_lower = model_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let stem_lower = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let name_matches = file_name_lower == expected_filename_lower
            || match entry.id {
                "typhoon25-qwen3-4b-q4km" => {
                    stem_lower.contains("typhoon2.5-qwen3-4b") && stem_lower.contains("q4_k_m")
                }
                "qwen25-3b-instruct-q4km" => {
                    stem_lower.contains("qwen2.5-3b-instruct") && stem_lower.contains("q4_k_m")
                }
                "whisper-ggml-base" => {
                    file_name_lower == "ggml-base.bin" || stem_lower == "ggml-base"
                }
                _ => false,
            };

        if path_matches || name_matches {
            if model.valid {
                return Some(model);
            }
            if fallback.is_none() {
                fallback = Some(model);
            }
        }
    }
    fallback
}

fn fallback_model_id(kind: ModelKind, path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model");
    match kind {
        ModelKind::Llm => format!("user:{stem}"),
        ModelKind::Asr => stem.to_ascii_lowercase(),
        _ => stem.to_string(),
    }
}

/// Check available filesystem space at `dir` (creating it if needed) requiring `required_bytes + 512 MiB`.
pub fn check_available_disk_space(dir: &Path, required_bytes: u64) -> SoulResult<u64> {
    fs::create_dir_all(dir)?;

    let c_path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| SoulError::InvalidInput("Directory path contains interior NUL byte".into()))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(SoulError::Io(format!(
            "ไม่สามารถตรวจสอบพื้นที่ดิสก์ได้ / Failed to check disk space at {}: {}",
            dir.display(),
            std::io::Error::last_os_error()
        )));
    }

    let avail_bytes = u64::from(stat.f_bavail).saturating_mul(stat.f_frsize);
    let required_with_headroom = required_bytes.saturating_add(DISK_HEADROOM_BYTES);
    if avail_bytes < required_with_headroom {
        let needed_gb = required_with_headroom as f64 / (1024.0 * 1024.0 * 1024.0);
        let avail_gb = avail_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        return Err(SoulError::Io(format!(
            "พื้นที่ดิสก์ไม่เพียงพอ (ต้องการอย่างน้อย {needed_gb:.2} GB รวมพื้นที่เผื่อ 512 MB แต่มีว่าง {avail_gb:.2} GB) / Insufficient disk space (requires {needed_gb:.2} GB including 512 MB headroom, only {avail_gb:.2} GB available)"
        )));
    }

    Ok(avail_bytes)
}

/// Verify downloaded file size bounds and 4-byte model magic header (`GGUF` or `0x67676D6C` LE).
pub fn verify_downloaded_file(
    path: &Path,
    kind: ModelKind,
    expected_size: Option<u64>,
    min_size: u64,
) -> SoulResult<()> {
    let metadata = fs::metadata(path).map_err(|e| {
        SoulError::Io(format!(
            "ไม่พบไฟล์ที่ดาวน์โหลด / Missing downloaded file {}: {e}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(SoulError::Io(format!(
            "ไฟล์ที่ดาวน์โหลดไม่ถูกต้อง / Downloaded path is not a regular file: {}",
            path.display()
        )));
    }

    let actual_size = metadata.len();
    if actual_size < min_size {
        return Err(SoulError::Io(format!(
            "ขนาดไฟล์เล็กเกินไป ({actual_size} bytes < {min_size} bytes) / Downloaded file too small ({actual_size} bytes < {min_size} bytes)"
        )));
    }
    if let Some(expected) = expected_size
        && actual_size != expected
    {
        return Err(SoulError::Io(format!(
            "ขนาดไฟล์ไม่ตรงกับต้นทาง ({actual_size} != {expected} bytes) / Downloaded size mismatch ({actual_size} != {expected} bytes)"
        )));
    }

    let mut file = File::open(path)?;
    let mut header = [0u8; 4];
    file.read_exact(&mut header).map_err(|e| {
        SoulError::Io(format!(
            "ไม่สามารถอ่านหัวไฟล์โมเดล / Failed reading model header: {e}"
        ))
    })?;

    match kind {
        ModelKind::Llm => {
            if header != GGUF_MAGIC {
                return Err(SoulError::Io(format!(
                    "หัวไฟล์ GGUF ไม่ถูกต้อง ({header:02x?}) / Invalid GGUF magic header (expected 'GGUF')"
                )));
            }
        }
        ModelKind::Asr => {
            let magic_le = u32::from_le_bytes(header);
            if magic_le != GGML_MAGIC_LE {
                return Err(SoulError::Io(format!(
                    "หัวไฟล์ Whisper GGML ไม่ถูกต้อง (0x{magic_le:08x}) / Invalid Whisper GGML magic header (expected 0x67676d6c)"
                )));
            }
        }
        other => {
            return Err(SoulError::InvalidInput(format!(
                "Unsupported model kind for verification: {other:?}"
            )));
        }
    }

    Ok(())
}

/// Write an async chunk stream to `part_path` with cancellation, `.part` cleanup, and ~10 Hz progress updates.
#[allow(clippy::too_many_arguments)]
pub async fn write_chunk_stream_with_cancel<S, B, E>(
    mut stream: S,
    part_path: &Path,
    append: bool,
    initial_bytes: u64,
    total_bytes: u64,
    asset_id: &str,
    cancel_token: &CancellationToken,
    on_progress: &mut impl FnMut(DownloadProgress),
) -> SoulResult<u64>
where
    S: futures_util::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: Display,
{
    if let Some(parent) = part_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let opened = if append {
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(part_path)
            .await?
    } else {
        tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(part_path)
            .await?
    };

    let mut file = Some(opened);
    let mut downloaded = initial_bytes;
    let mut last_emit = Instant::now() - Duration::from_millis(PROGRESS_THROTTLE_MS);
    let mut window_start = Instant::now();
    let mut window_bytes = 0u64;

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                drop(file.take());
                let _ = tokio::fs::remove_file(part_path).await;
                emit_cancelled(on_progress, asset_id, downloaded, total_bytes);
                return Err(cancelled_error());
            }
            next = stream.next() => {
                match next {
                    Some(Ok(chunk)) => {
                        let bytes = chunk.as_ref();
                        if let Some(f) = file.as_mut()
                            && let Err(e) = f.write_all(bytes).await
                        {
                            drop(file.take());
                            return Err(SoulError::Io(format!(
                                "เขียนไฟล์บางส่วนไม่สำเร็จ / Failed writing partial download: {e}"
                            )));
                        }

                        let len = bytes.len() as u64;
                        downloaded = downloaded.saturating_add(len);
                        window_bytes = window_bytes.saturating_add(len);

                        if last_emit.elapsed() >= Duration::from_millis(PROGRESS_THROTTLE_MS) {
                            let elapsed_secs = window_start.elapsed().as_secs_f64().max(0.001);
                            let speed_mbps = (window_bytes as f64 / (1024.0 * 1024.0)) / elapsed_secs;
                            on_progress(DownloadProgress {
                                asset_id: asset_id.to_string(),
                                downloaded_bytes: downloaded,
                                total_bytes: total_bytes.max(downloaded),
                                speed_mbps,
                                status: DownloadStatus::Downloading,
                                message: None,
                            });
                            last_emit = Instant::now();
                            if window_start.elapsed() >= Duration::from_secs(1) {
                                window_start = Instant::now();
                                window_bytes = 0;
                            }
                        }
                    }
                    Some(Err(err)) => {
                        drop(file.take());
                        return Err(SoulError::Io(format!(
                            "การดาวน์โหลดขัดข้องระหว่างรับข้อมูล / Download stream interrupted: {err}"
                        )));
                    }
                    None => break,
                }
            }
        }
    }

    if let Some(mut f) = file.take() {
        f.flush().await?;
    }

    Ok(downloaded)
}

async fn resolve_huggingface_asset(
    client: &reqwest::Client,
    entry: &StarterCatalogEntry,
) -> SoulResult<(String, Option<u64>, String)> {
    let api_url = format!("https://huggingface.co/api/models/{}?blobs=true", entry.repo);
    let resp = client.get(&api_url).send().await.map_err(|e| {
        SoulError::Io(format!(
            "ไม่สามารถเชื่อมต่อ Hugging Face API ได้ / Failed querying Hugging Face API ({}): {e}",
            entry.repo
        ))
    })?;

    if !resp.status().is_success() {
        return Err(SoulError::Io(format!(
            "Hugging Face API ตอบกลับ HTTP {} สำหรับ {} / Hugging Face API returned HTTP {} for {}",
            resp.status(),
            entry.repo,
            resp.status(),
            entry.repo
        )));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        SoulError::Serialization(format!(
            "อ่านข้อมูลจาก Hugging Face API ไม่สำเร็จ / Failed parsing Hugging Face API response: {e}"
        ))
    })?;

    let siblings = json
        .get("siblings")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            SoulError::ModelNotFound(format!(
                "ไม่พบรายการไฟล์ใน {} / No siblings list in Hugging Face metadata for {}",
                entry.repo, entry.repo
            ))
        })?;

    let selected = match entry.kind {
        ModelKind::Llm => siblings.iter().find(|s| {
            let name = s
                .get("rfilename")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            name.ends_with(".gguf") && name.contains("q4_k_m")
        }),
        ModelKind::Asr => siblings.iter().find(|s| {
            s.get("rfilename")
                .and_then(|v| v.as_str())
                .is_some_and(|name| name == "ggml-base.bin")
        }),
        _ => None,
    }
    .ok_or_else(|| {
        SoulError::ModelNotFound(format!(
            "ไม่พบไฟล์โมเดลที่รองรับใน {} / Could not resolve compatible file in {}",
            entry.repo, entry.repo
        ))
    })?;

    let rfilename = selected
        .get("rfilename")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SoulError::Serialization("Missing rfilename in Hugging Face sibling".into()))?
        .to_string();

    let exact_size = selected
        .get("size")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            selected
                .get("lfs")
                .and_then(|l| l.get("size"))
                .and_then(|v| v.as_u64())
        })
        .filter(|&sz| sz > 0);

    let download_url = format!("https://huggingface.co/{}/resolve/main/{}", entry.repo, rfilename);
    Ok((rfilename, exact_size, download_url))
}

fn emit_cancelled(
    on_progress: &mut impl FnMut(DownloadProgress),
    asset_id: &str,
    downloaded_bytes: u64,
    total_bytes: u64,
) {
    on_progress(DownloadProgress {
        asset_id: asset_id.to_string(),
        downloaded_bytes,
        total_bytes,
        speed_mbps: 0.0,
        status: DownloadStatus::Cancelled,
        message: Some("ยกเลิกการดาวน์โหลดแล้ว / Download cancelled".to_string()),
    });
}

fn emit_error(
    on_progress: &mut impl FnMut(DownloadProgress),
    asset_id: &str,
    downloaded_bytes: u64,
    total_bytes: u64,
    err: &SoulError,
) {
    on_progress(DownloadProgress {
        asset_id: asset_id.to_string(),
        downloaded_bytes,
        total_bytes,
        speed_mbps: 0.0,
        status: DownloadStatus::Error,
        message: Some(err.to_string()),
    });
}

fn cancelled_error() -> SoulError {
    SoulError::Io("ยกเลิกการดาวน์โหลดแล้ว / Download cancelled by user".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::Write;

    use shared_types::ModelSource;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;

    #[test]
    fn test_starter_catalog_validation_and_offline_init() {
        let manager = StarterDownloadManager::new();
        assert_eq!(manager.client_initialized_count(), 0);

        let tmp = tempdir().unwrap();
        let llm_dir = tmp.path().join("models/llm");
        let asr_dir = tmp.path().join("models/asr");
        fs::create_dir_all(&llm_dir).unwrap();
        fs::create_dir_all(&asr_dir).unwrap();

        let empty_catalog = curated_starter_catalog(&llm_dir, &asr_dir, &[], &[]);
        assert_eq!(empty_catalog.len(), 3);
        assert_eq!(manager.client_initialized_count(), 0);

        let mut unique_ids = HashSet::new();
        for asset in &empty_catalog {
            assert!(unique_ids.insert(asset.id.clone()), "Duplicate asset id: {}", asset.id);
            assert_eq!(asset.safety_tier, RamSafetyTier::Safe8Gb);
            assert!(!asset.installed);
            assert!(asset.installed_model_id.is_none());
            assert!(asset.installed_path.is_none());
        }

        // Simulate a matching LLM discovered via scan_models and an expected ASR file in user_asr_dir.
        let discovered_llm = vec![ModelInfo {
            id: "user:Typhoon2.5-Qwen3-4B-Q4_K_M".into(),
            name: "Typhoon 2.5 Qwen3 4B".into(),
            kind: ModelKind::Llm,
            path: llm_dir
                .join("Typhoon2.5-Qwen3-4B-Q4_K_M.gguf")
                .to_string_lossy()
                .into_owned(),
            size_bytes: 2_680_000_000,
            estimated_ram_mb: 3150,
            source: ModelSource::UserData,
            safety_tier: RamSafetyTier::Safe8Gb,
            is_active: false,
            valid: true,
            notes: None,
        }];
        fs::write(asr_dir.join("ggml-base.bin"), b"lmgg").unwrap();

        let populated_catalog = curated_starter_catalog(&llm_dir, &asr_dir, &discovered_llm, &[]);
        let typhoon = populated_catalog
            .iter()
            .find(|a| a.id == "typhoon25-qwen3-4b-q4km")
            .unwrap();
        assert!(typhoon.installed);
        assert_eq!(
            typhoon.installed_model_id.as_deref(),
            Some("user:Typhoon2.5-Qwen3-4B-Q4_K_M")
        );
        assert!(typhoon.installed_path.is_some());

        let qwen = populated_catalog
            .iter()
            .find(|a| a.id == "qwen25-3b-instruct-q4km")
            .unwrap();
        assert!(!qwen.installed);

        let whisper = populated_catalog
            .iter()
            .find(|a| a.id == "whisper-ggml-base")
            .unwrap();
        assert!(whisper.installed);
        assert!(whisper.installed_path.as_deref().unwrap().ends_with("ggml-base.bin"));
        assert_eq!(manager.client_initialized_count(), 0);
    }

    #[test]
    fn test_magic_byte_and_size_verification() {
        let tmp = tempdir().unwrap();

        // Valid GGUF
        let gguf_ok = tmp.path().join("ok.gguf");
        {
            let mut f = File::create(&gguf_ok).unwrap();
            f.write_all(b"GGUF").unwrap();
            f.write_all(&[0u8; 60]).unwrap();
        }
        verify_downloaded_file(&gguf_ok, ModelKind::Llm, Some(64), 32).unwrap();

        // Wrong magic for GGUF
        let gguf_bad_magic = tmp.path().join("bad_magic.gguf");
        fs::write(&gguf_bad_magic, vec![b'X'; 64]).unwrap();
        assert!(verify_downloaded_file(&gguf_bad_magic, ModelKind::Llm, Some(64), 32).is_err());

        // Expected size mismatch
        assert!(verify_downloaded_file(&gguf_ok, ModelKind::Llm, Some(128), 32).is_err());

        // Below min size
        assert!(verify_downloaded_file(&gguf_ok, ModelKind::Llm, None, 128).is_err());

        // Valid Whisper GGML (0x67676d6c LE == b"lmgg")
        let ggml_ok = tmp.path().join("ggml-base.bin");
        {
            let mut f = File::create(&ggml_ok).unwrap();
            f.write_all(&0x6767_6D6Cu32.to_le_bytes()).unwrap();
            f.write_all(&[1u8; 60]).unwrap();
        }
        verify_downloaded_file(&ggml_ok, ModelKind::Asr, Some(64), 32).unwrap();

        // Wrong magic for ASR
        let ggml_bad = tmp.path().join("bad.bin");
        fs::write(&ggml_bad, vec![0u8; 64]).unwrap();
        assert!(verify_downloaded_file(&ggml_bad, ModelKind::Asr, Some(64), 32).is_err());
    }

    #[test]
    fn test_disk_space_check_rejects_unreasonable_size() {
        let tmp = tempdir().unwrap();
        let target = tmp.path().join("nested/models");
        let err = check_available_disk_space(&target, u64::MAX / 2)
            .expect_err("Expected unreasonable size to fail disk check");
        let msg = err.to_string();
        assert!(
            msg.contains("พื้นที่ดิสก์ไม่เพียงพอ") && msg.contains("Insufficient disk space"),
            "Expected bilingual disk space error, got: {msg}"
        );
        assert!(target.is_dir(), "Expected check_available_disk_space to create dir");
    }

    #[tokio::test]
    async fn test_cancellation_cleans_up_part_file() {
        let tmp = tempdir().unwrap();
        let final_path = tmp.path().join("typhoon2.5-qwen3-4b-q4_k_m.gguf");
        let part_path = tmp.path().join("typhoon2.5-qwen3-4b-q4_k_m.gguf.part");

        let (tx, mut rx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
        let stream = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));

        let cancel_token = CancellationToken::new();
        let cancel_clone = cancel_token.clone();

        tx.send(Ok(vec![b'G'; 1024])).await.unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            cancel_clone.cancel();
            // Keep sender alive until cancel triggers so stream doesn't end prematurely.
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(tx);
        });

        let mut events = Vec::new();
        let res = write_chunk_stream_with_cancel(
            stream,
            &part_path,
            false,
            0,
            4096,
            "typhoon25-qwen3-4b-q4km",
            &cancel_token,
            &mut |p| events.push(p),
        )
        .await;

        assert!(res.is_err(), "Expected cancellation to return Err");
        assert!(!part_path.exists(), ".part file must be deleted on cancel");
        assert!(!final_path.exists(), "Final file must not exist on cancel");
        assert!(
            events.iter().any(|e| e.status == DownloadStatus::Cancelled),
            "Expected DownloadStatus::Cancelled progress event, got {events:?}"
        );
    }
}
