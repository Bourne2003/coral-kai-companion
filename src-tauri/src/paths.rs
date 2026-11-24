use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use shared_types::{ModelInfo, ModelKind, ModelSource, RamSafetyTier, SoulError, SoulResult};
use tauri::{AppHandle, Manager};

/// Resolved filesystem paths for bundled resources and persistent user data.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub db_path: PathBuf,
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

        let user_llm_dir = app_data_dir.join("models/llm");
        let user_asr_dir = app_data_dir.join("models/asr");
        let user_embed_dir = app_data_dir.join("models/embed");
        let user_live2d_dir = app_data_dir.join("live2d");

        for dir in [
            &user_llm_dir,
            &user_asr_dir,
            &user_embed_dir,
            &user_live2d_dir,
        ] {
            fs::create_dir_all(dir)?;
        }

        Ok(Self {
            db_path: app_data_dir.join("soul_memory.sqlite"),
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
        return PathBuf::from(home).join("Library/Application Support/com.soulmate2d.app");
    }
    std::env::temp_dir().join("soulmate2d")
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

/// Scan bundled, user, and custom embedding models.
pub fn scan_embed_models(
    bundled_dir: &Path,
    user_dir: &Path,
    custom: &[ModelInfo],
    onnx_active: bool,
) -> Vec<ModelInfo> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

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

        let filename = manifest
            .as_ref()
            .and_then(|m| m.filename.clone())
            .unwrap_or_else(|| "model_quantized.onnx".to_string());
        let model_path = dir.join(&filename);
        let tokenizer_path = dir.join("tokenizer.json");

        if !model_path.is_file() && manifest.is_none() {
            continue;
        }

        let canon = fs::canonicalize(&model_path).unwrap_or_else(|_| model_path.clone());
        if !seen.insert(canon) {
            continue;
        }

        let size_bytes = fs::metadata(&model_path).map(|m| m.len()).unwrap_or(0);
        let valid = model_path.is_file() && tokenizer_path.is_file();
        let estimated_ram_mb = manifest
            .as_ref()
            .and_then(|m| m.estimated_ram_mb)
            .unwrap_or(256);
        let safety_tier = manifest
            .as_ref()
            .and_then(|m| m.safety_tier)
            .unwrap_or(RamSafetyTier::Safe8Gb);

        let is_active = valid && onnx_active && source == ModelSource::Bundled;
        let notes = if !valid {
            Some("Missing model_quantized.onnx or tokenizer.json".to_string())
        } else if is_active {
            Some("ONNX Runtime active (384-dim multilingual Thai/English)".to_string())
        } else {
            Some("ONNX weights ready (char-ngram fallback active if dylib missing)".to_string())
        };

        out.push(ModelInfo {
            id: manifest
                .as_ref()
                .and_then(|m| m.id.clone())
                .unwrap_or_else(|| format!("embed-{}", dir_label(dir))),
            name: manifest
                .as_ref()
                .and_then(|m| m.name.clone())
                .unwrap_or_else(|| filename.clone()),
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

    for item in custom {
        let p = PathBuf::from(&item.path);
        let canon = fs::canonicalize(&p).unwrap_or(p);
        if seen.insert(canon) {
            out.push(item.clone());
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

    let model3_path = find_model3_file(&folder, manifest.as_ref().and_then(|m| m.model_file.as_deref()))
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
                if let Ok((canon_folder, info)) = inspect_live2d_folder(&subdir, source, active_id) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_resources_discover_embed_and_live2d() {
        let res_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let embed_dir = res_root.join("models/embed");
        let live2d_dir = res_root.join("live2d");
        let empty_user = std::env::temp_dir().join("soulmate2d_test_empty_user_models");

        let embed_models = scan_embed_models(&embed_dir, &empty_user, &[], true);
        assert!(
            !embed_models.is_empty(),
            "Expected bundled embed model in {}",
            embed_dir.display()
        );
        assert_eq!(embed_models[0].id, "paraphrase-multilingual-minilm-l12-v2");
        assert!(embed_models[0].valid);
        assert!(embed_models[0].is_active);

        let (allowed_dirs, live2d_models) = scan_live2d_models(
            &live2d_dir,
            &empty_user,
            &[],
            Some("default_companion"),
        );
        assert!(!allowed_dirs.is_empty());
        assert_eq!(live2d_models.len(), 1);
        assert_eq!(live2d_models[0].id, "default_companion");
        assert!(live2d_models[0].valid);
        assert!(live2d_models[0].is_active);
        assert!(live2d_models[0].path.ends_with("soulmate.model3.json"));
    }

    #[test]
    fn bundled_onnx_dylib_loads_in_soul_memory_store() {
        let embed_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/models/embed");
        let db_path = std::env::temp_dir().join(format!(
            "soulmate2d_tauri_onnx_test_{}.sqlite",
            shared_types::new_id()
        ));
        let store = soul_memory::SoulMemoryStore::open(&db_path, Some(&embed_dir))
            .expect("open SoulMemoryStore with bundled embed_dir");
        assert_eq!(store.embedder_kind(), "onnx");
        let _ = std::fs::remove_file(&db_path);
    }
}
