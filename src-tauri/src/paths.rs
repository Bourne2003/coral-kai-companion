use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use shared_types::{
    CharacterId, ModelInfo, ModelKind, ModelSource, RamSafetyTier, SoulError, SoulResult,
};
use tauri::{AppHandle, Manager};

/// Resolved filesystem paths for bundled resources and persistent user data.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub db_path: PathBuf,
    pub coral_db_path: PathBuf,
    pub kai_db_path: PathBuf,
    pub config_path: PathBuf,
    pub bundled_llm_dir: PathBuf,
    pub bundled_asr_dir: PathBuf,
    pub bundled_embed_dir: PathBuf,
    pub bundled_live2d_dir: PathBuf,
    pub user_llm_dir: PathBuf,
    pub user_asr_dir: PathBuf,
    pub user_embed_dir: PathBuf,
    pub user_live2d_dir: PathBuf,
}

impl AppPaths {
    /// Resolve bundled resource directories and writable macOS Application Support paths.
    pub fn resolve(app: &AppHandle) -> SoulResult<Self> {
        let resource_dir = app.path().resource_dir().ok();
        let app_data_dir = app
            .path()
            .app_data_dir()
            .ok()
            .unwrap_or_else(fallback_app_data_dir);

        fs::create_dir_all(&app_data_dir)?;

        let coral_profile_dir = app_data_dir.join("profiles/coral");
        let kai_profile_dir = app_data_dir.join("profiles/kai");
        let user_llm_dir = app_data_dir.join("models/llm");
        let user_asr_dir = app_data_dir.join("models/asr");
        let user_embed_dir = app_data_dir.join("models/embed");
        let user_live2d_dir = app_data_dir.join("live2d");

        for dir in [
            &coral_profile_dir,
            &kai_profile_dir,
            &user_llm_dir,
            &user_asr_dir,
            &user_embed_dir,
            &user_live2d_dir,
        ] {
            fs::create_dir_all(dir)?;
        }

        let coral_db_path = coral_profile_dir.join("soul_memory.sqlite");
        let kai_db_path = kai_profile_dir.join("soul_memory.sqlite");

        Ok(Self {
            db_path: coral_db_path.clone(),
            coral_db_path,
            kai_db_path,
            config_path: app_data_dir.join("models_config.json"),
            bundled_llm_dir: resolve_bundled_subdir(resource_dir.as_deref(), "models/llm"),
            bundled_asr_dir: resolve_bundled_subdir(resource_dir.as_deref(), "models/asr"),
            bundled_embed_dir: resolve_bundled_subdir(resource_dir.as_deref(), "models/embed"),
            bundled_live2d_dir: resolve_bundled_subdir(resource_dir.as_deref(), "live2d"),
            user_llm_dir,
            user_asr_dir,
            user_embed_dir,
            user_live2d_dir,
        })
    }

    /// Return the isolated SQLite database path for the given character profile.
    pub fn db_path_for(&self, character: CharacterId) -> &Path {
        match character {
            CharacterId::Coral => &self.coral_db_path,
            CharacterId::Kai => &self.kai_db_path,
        }
    }
}

fn resolve_bundled_subdir(resource_dir: Option<&Path>, subpath: &str) -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(res) = resource_dir {
        candidates.push(res.join("resources").join(subpath));
        candidates.push(res.join(subpath));
    }
    candidates.push(PathBuf::from("src-tauri/resources").join(subpath));
    candidates.push(PathBuf::from("resources").join(subpath));
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(subpath),
    );

    for candidate in &candidates {
        if candidate.is_dir() {
            return candidate.clone();
        }
    }

    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("src-tauri/resources").join(subpath))
}

fn fallback_app_data_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Application Support/com.coralkai.companion");
    }
    std::env::temp_dir().join("coralkai-companion")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmbedManifest {
    id: Option<String>,
    name: Option<String>,
    filename: Option<String>,
    estimated_ram_mb: Option<u64>,
    safety_tier: Option<RamSafetyTier>,
}

fn is_same_canonical_path(a: &Path, active_canon: Option<&Path>) -> bool {
    let Some(active) = active_canon else {
        return false;
    };
    let canon_a = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    canon_a == active
}

/// Scan bundled, user, and custom embedding models without failing when directories are empty.
pub fn scan_embed_models(
    bundled_dir: &Path,
    user_dir: &Path,
    custom: &[ModelInfo],
    active_embed_path: Option<&Path>,
) -> Vec<ModelInfo> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let active_canon = active_embed_path.map(|p| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));

    for (dir, source) in [
        (bundled_dir, ModelSource::Bundled),
        (user_dir, ModelSource::UserData),
    ] {
        if !dir.is_dir() {
            continue;
        }

        let manifest = fs::read_to_string(dir.join("manifest.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<EmbedManifest>(&s).ok());

        if let Some(ref m) = manifest {
            let filename = m
                .filename
                .clone()
                .unwrap_or_else(|| "model_quantized.onnx".to_string());
            let model_path = dir.join(&filename);
            let canon = fs::canonicalize(&model_path).unwrap_or_else(|_| model_path.clone());
            if seen.insert(canon) {
                let size_bytes = fs::metadata(&model_path).map(|md| md.len()).unwrap_or(0);
                let valid = soul_memory::validate_onnx_embed_file(&model_path).is_ok();
                let estimated_ram_mb = m.estimated_ram_mb.unwrap_or(256);
                let safety_tier = m.safety_tier.unwrap_or(RamSafetyTier::Safe8Gb);
                let is_active = valid && is_same_canonical_path(&model_path, active_canon.as_deref());
                let notes = if !valid {
                    Some("Missing or invalid .onnx file or sibling tokenizer.json".to_string())
                } else if is_active {
                    Some("ONNX Runtime active (384-dim multilingual Thai/English)".to_string())
                } else {
                    Some("ONNX embedding weights ready (activate in Models)".to_string())
                };

                out.push(ModelInfo {
                    id: m
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("embed-{}", dir_label(dir))),
                    name: m.name.clone().unwrap_or(filename),
                    kind: ModelKind::Embed,
                    path: model_path.to_string_lossy().into_owned(),
                    size_bytes,
                    estimated_ram_mb,
                    source,
                    safety_tier,
                    is_active,
                    valid,
                    notes,
                });
            }
        }

        if let Ok(entries) = fs::read_dir(dir) {
            let mut onnx_files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|entry| entry.path()))
                .filter(|p| {
                    p.is_file()
                        && p.extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
                })
                .collect();
            onnx_files.sort();

            for model_path in onnx_files {
                let canon = fs::canonicalize(&model_path).unwrap_or_else(|_| model_path.clone());
                if !seen.insert(canon) {
                    continue;
                }
                let size_bytes = fs::metadata(&model_path).map(|md| md.len()).unwrap_or(0);
                let valid = soul_memory::validate_onnx_embed_file(&model_path).is_ok();
                let estimated_ram_mb = ((size_bytes / (1024 * 1024)).saturating_add(128)).max(128);
                let is_active =
                    valid && is_same_canonical_path(&model_path, active_canon.as_deref());
                let stem = model_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("embed-onnx");
                let filename = model_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("model.onnx")
                    .to_string();

                out.push(ModelInfo {
                    id: format!("embed-{}-{}", dir_label(dir), sanitize_slug(stem)),
                    name: filename,
                    kind: ModelKind::Embed,
                    path: model_path.to_string_lossy().into_owned(),
                    size_bytes,
                    estimated_ram_mb,
                    source,
                    safety_tier: RamSafetyTier::Safe8Gb,
                    is_active,
                    valid,
                    notes: if !valid {
                        Some("Missing sibling tokenizer.json or invalid ONNX header".to_string())
                    } else if is_active {
                        Some("ONNX Runtime active (384-dim)".to_string())
                    } else {
                        Some("ONNX embedding model ready".to_string())
                    },
                });
            }
        }
    }

    for item in custom {
        let p = PathBuf::from(&item.path);
        let canon = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if seen.insert(canon) {
            let mut cloned = item.clone();
            cloned.valid = soul_memory::validate_onnx_embed_file(&p).is_ok();
            cloned.is_active =
                cloned.valid && is_same_canonical_path(&p, active_canon.as_deref());
            if !cloned.valid {
                cloned.notes =
                    Some("ONNX file or sibling tokenizer.json missing on disk".to_string());
            } else if cloned.is_active {
                cloned.notes = Some("ONNX Runtime active (384-dim)".to_string());
            }
            out.push(cloned);
        }
    }

    out
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Live2dManifest {
    id: Option<String>,
    name: Option<String>,
    display_name: Option<String>,
    model_file: Option<String>,
    recommended_ram_mb: Option<u64>,
    notes: Option<String>,
}

/// Inspect a single Live2D folder containing a `.model3.json` file.
pub fn inspect_live2d_folder(
    input_path: &Path,
    source: ModelSource,
    active_id: Option<&str>,
) -> SoulResult<(PathBuf, ModelInfo)> {
    let folder = if input_path.is_file()
        && input_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_ascii_lowercase().ends_with(".model3.json"))
    {
        input_path
            .parent()
            .ok_or_else(|| SoulError::InvalidInput("Invalid .model3.json parent path".into()))?
            .to_path_buf()
    } else {
        input_path.to_path_buf()
    };

    if !folder.exists() || !folder.is_dir() {
        return Err(SoulError::ModelNotFound(format!(
            "Live2D folder does not exist: {}",
            folder.display()
        )));
    }

    let manifest = fs::read_to_string(folder.join("manifest.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Live2dManifest>(&s).ok());

    let model3_path = find_model3_file(
        &folder,
        manifest.as_ref().and_then(|m| m.model_file.as_deref()),
    )
    .ok_or_else(|| {
        SoulError::InvalidInput(format!(
            "No .model3.json file found inside Live2D folder: {}",
            folder.display()
        ))
    })?;

    // Validate JSON structure of .model3.json
    let raw_model3 = fs::read_to_string(&model3_path)?;
    let model3_json: serde_json::Value = serde_json::from_str(&raw_model3).map_err(|e| {
        SoulError::InvalidInput(format!(
            "Malformed .model3.json at {}: {e}",
            model3_path.display()
        ))
    })?;

    let canon_folder = fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone());
    let canon_model3 = fs::canonicalize(&model3_path).unwrap_or_else(|_| model3_path.clone());

    let folder_name = canon_folder
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("live2d-avatar")
        .to_string();

    let id = manifest
        .as_ref()
        .and_then(|m| m.id.clone())
        .unwrap_or_else(|| format!("live2d:{folder_name}"));
    let name = manifest
        .as_ref()
        .and_then(|m| m.display_name.clone().or_else(|| m.name.clone()))
        .or_else(|| {
            model3_json
                .get("Name")
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| folder_name.clone());

    let estimated_ram_mb = manifest
        .as_ref()
        .and_then(|m| m.recommended_ram_mb)
        .unwrap_or(48);
    let size_bytes = compute_dir_size(&canon_folder);
    let notes = manifest
        .as_ref()
        .and_then(|m| m.notes.clone())
        .or_else(|| Some("Live2D Cubism .model3.json avatar bundle".to_string()));

    let info = ModelInfo {
        id: id.clone(),
        name,
        kind: ModelKind::Live2d,
        path: canon_model3.to_string_lossy().into_owned(),
        size_bytes,
        estimated_ram_mb,
        source,
        safety_tier: RamSafetyTier::Safe8Gb,
        is_active: active_id == Some(id.as_str()),
        valid: true,
        notes,
    };

    Ok((canon_folder, info))
}

/// Scan bundled, user, and custom Live2D avatar directories.
pub fn scan_live2d_models(
    bundled_dir: &Path,
    user_dir: &Path,
    custom: &[ModelInfo],
    active_id: Option<&str>,
) -> (Vec<PathBuf>, Vec<ModelInfo>) {
    let mut allowed_dirs = Vec::new();
    let mut models = Vec::new();
    let mut seen_folders = HashSet::new();

    for (root, source) in [
        (bundled_dir, ModelSource::Bundled),
        (user_dir, ModelSource::UserData),
    ] {
        if !root.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(root) {
            let mut subdirs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|entry| entry.path()))
                .filter(|p| p.is_dir())
                .collect();
            subdirs.sort();

            for subdir in subdirs {
                if let Ok((canon_folder, info)) = inspect_live2d_folder(&subdir, source, active_id)
                {
                    if seen_folders.insert(canon_folder.clone()) {
                        allowed_dirs.push(canon_folder);
                        models.push(info);
                    }
                }
            }
        }
    }

    for item in custom {
        let p = PathBuf::from(&item.path);
        let folder = if p.is_file() {
            p.parent().unwrap_or(&p).to_path_buf()
        } else {
            p
        };
        let canon = fs::canonicalize(&folder).unwrap_or(folder);
        if seen_folders.insert(canon.clone()) {
            let mut cloned = item.clone();
            cloned.valid = Path::new(&cloned.path).exists();
            cloned.is_active = active_id == Some(cloned.id.as_str());
            allowed_dirs.push(canon);
            models.push(cloned);
        }
    }

    (allowed_dirs, models)
}

fn find_model3_file(dir: &Path, preferred: Option<&str>) -> Option<PathBuf> {
    if let Some(name) = preferred {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let mut matches: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_ascii_lowercase().ends_with(".model3.json"))
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

fn compute_dir_size(dir: &Path) -> u64 {
    let mut total = 0_u64;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total = total.saturating_add(compute_dir_size(&path));
        } else if let Ok(md) = entry.metadata() {
            total = total.saturating_add(md.len());
        }
    }
    total
}

fn dir_label(dir: &Path) -> String {
    dir.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("local")
        .to_string()
}

pub(crate) fn sanitize_slug(raw: &str) -> String {
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
        "custom".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byom_resources_handle_empty_embed_and_discover_coral_and_kai() {
        let res_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let embed_dir = res_root.join("models/embed");
        let live2d_dir = res_root.join("live2d");
        let empty_user = std::env::temp_dir().join(format!(
            "soulmate2d_test_empty_user_{}",
            shared_types::new_id()
        ));

        // Zero bundled embed weights (only .gitkeep) must return empty cleanly with no error.
        let embed_models = scan_embed_models(&embed_dir, &empty_user, &[], None);
        assert!(
            embed_models.is_empty(),
            "Expected zero bundled embed models in BYOM mode, got {embed_models:?}"
        );

        // If a manifest exists without its .onnx file, it should be flagged valid = false.
        fs::create_dir_all(&empty_user).unwrap();
        fs::write(
            empty_user.join("manifest.json"),
            r#"{"id":"missing-embed","name":"Missing","filename":"missing.onnx"}"#,
        )
        .unwrap();
        let invalid_manifest_models = scan_embed_models(&embed_dir, &empty_user, &[], None);
        assert_eq!(invalid_manifest_models.len(), 1);
        assert!(!invalid_manifest_models[0].valid);
        let _ = fs::remove_dir_all(&empty_user);

        // Bundled Live2D should discover both coral_companion and kai_companion.
        let (allowed_dirs, live2d_models) = scan_live2d_models(
            &live2d_dir,
            &empty_user,
            &[],
            Some("coral_companion"),
        );
        assert_eq!(allowed_dirs.len(), 2);
        assert_eq!(live2d_models.len(), 2);

        let coral = live2d_models
            .iter()
            .find(|m| m.id == "coral_companion")
            .expect("coral_companion bundle present");
        assert!(coral.valid);
        assert!(coral.is_active);
        assert!(coral.path.ends_with("soulmate.model3.json"));

        let kai = live2d_models
            .iter()
            .find(|m| m.id == "kai_companion")
            .expect("kai_companion bundle present");
        assert!(kai.valid);
        assert!(!kai.is_active);
        assert!(kai.path.ends_with("soulmate.model3.json"));
    }

    #[test]
    fn soul_memory_store_boots_in_zero_model_fallback_mode() {
        let db_path = std::env::temp_dir().join(format!(
            "soulmate2d_tauri_byom_mem_{}.sqlite",
            shared_types::new_id()
        ));
        let store = soul_memory::SoulMemoryStore::open(&db_path, None)
            .expect("open SoulMemoryStore without bundled ONNX weights");
        assert_eq!(store.embedder_kind(), "ngram-fallback");
        assert!(store.active_embed_model_path().is_none());
        let _ = fs::remove_file(&db_path);
    }
}
