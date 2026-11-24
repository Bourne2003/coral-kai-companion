use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;
use shared_types::{ModelInfo, ModelKind, ModelSource, SoulError, SoulResult};

use crate::ram_guard::{classify_ram_safety_tier, estimate_gguf_ram_mb};

/// Canonical 4-byte magic header at offset 0 of every valid GGUF model file.
pub const GGUF_MAGIC: [u8; 4] = *b"GGUF";

#[derive(Debug, Clone, Default)]
struct ManifestMeta {
    id: Option<String>,
    name: Option<String>,
    notes: Option<String>,
}

/// Validates that a file starts with the 4-byte `GGUF` magic header.
pub fn validate_gguf_magic(path: &Path) -> SoulResult<bool> {
    let mut file = File::open(path)?;
    let mut header = [0u8; 4];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(header == GGUF_MAGIC),
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(SoulError::Io(err.to_string())),
    }
}

/// Scans bundled, user, and registered custom LLM entries for `.gguf` models.
pub fn scan_llm_dirs(
    bundled_dir: &Path,
    user_dir: &Path,
    custom_models: &[ModelInfo],
    active_path: Option<&Path>,
    n_ctx: u32,
) -> SoulResult<Vec<ModelInfo>> {
    let mut results = Vec::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let active_canonical = active_path.map(canonical_or_raw);

    for (dir, source) in [
        (bundled_dir, ModelSource::Bundled),
        (user_dir, ModelSource::UserData),
    ] {
        if !dir.exists() || !dir.is_dir() {
            continue;
        }
        let manifest = load_manifest_metadata(dir);
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| is_gguf_file(path))
            .collect();
        entries.sort();

        for path in entries {
            let key = canonical_or_raw(&path);
            if !seen_paths.insert(key.clone()) {
                continue;
            }
            let info = inspect_gguf_model(
                &path,
                source,
                &manifest,
                active_canonical.as_deref(),
                n_ctx,
                None,
            );
            results.push(info);
        }
    }

    for custom in custom_models {
        let path = PathBuf::from(&custom.path);
        let key = canonical_or_raw(&path);
        if !seen_paths.insert(key.clone()) {
            continue;
        }
        let empty_manifest = HashMap::new();
        let mut info = inspect_gguf_model(
            &path,
            ModelSource::Custom,
            &empty_manifest,
            active_canonical.as_deref(),
            n_ctx,
            Some(custom),
        );
        info.id = custom.id.clone();
        info.name = custom.name.clone();
        results.push(info);
    }

    Ok(results)
}

/// Validates and registers a custom `.gguf` model file on disk.
pub fn register_custom_gguf(
    path: &Path,
    name: Option<&str>,
    n_ctx: u32,
) -> SoulResult<ModelInfo> {
    if !path.exists() || !path.is_file() {
        return Err(SoulError::ModelNotFound(format!(
            "GGUF model file not found: {}",
            path.display()
        )));
    }
    if !is_gguf_file(path) {
        return Err(SoulError::InvalidInput(format!(
            "Model file must have .gguf extension: {}",
            path.display()
        )));
    }
    if !validate_gguf_magic(path)? {
        return Err(SoulError::InvalidInput(format!(
            "Invalid GGUF magic header in {}",
            path.display()
        )));
    }

    let size_bytes = fs::metadata(path)?.len();
    let estimated_ram_mb = estimate_gguf_ram_mb(size_bytes, n_ctx);
    let safety_tier = classify_ram_safety_tier(estimated_ram_mb);
    let canonical = canonical_or_raw(path);

    let file_stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("custom-llm");
    let display_name = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| file_stem.to_string());

    Ok(ModelInfo {
        id: build_model_id(ModelSource::Custom, path),
        name: display_name,
        kind: ModelKind::Llm,
        path: canonical.to_string_lossy().into_owned(),
        size_bytes,
        estimated_ram_mb,
        source: ModelSource::Custom,
        safety_tier,
        is_active: false,
        valid: true,
        notes: None,
    })
}

fn inspect_gguf_model(
    path: &Path,
    source: ModelSource,
    manifest: &HashMap<String, ManifestMeta>,
    active_canonical: Option<&Path>,
    n_ctx: u32,
    fallback: Option<&ModelInfo>,
) -> ModelInfo {
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let meta = manifest.get(&file_name);

    let canonical = canonical_or_raw(path);
    let is_active = active_canonical.is_some_and(|active| active == canonical.as_path());

    let (size_bytes, valid, mut notes) = match fs::metadata(path) {
        Ok(md) if md.is_file() => {
            let size = md.len();
            match validate_gguf_magic(path) {
                Ok(true) => (size, true, None),
                Ok(false) => (
                    size,
                    false,
                    Some("Invalid GGUF magic header (expected 'GGUF')".to_string()),
                ),
                Err(err) => (size, false, Some(format!("Failed reading GGUF header: {err}"))),
            }
        }
        Ok(_) => (0, false, Some("Path is not a regular file".to_string())),
        Err(err) => (
            fallback.map_or(0, |f| f.size_bytes),
            false,
            Some(format!("Model file missing or unreadable: {err}")),
        ),
    };

    if notes.is_none() {
        notes = meta.and_then(|m| m.notes.clone());
    }

    let estimated_ram_mb = estimate_gguf_ram_mb(size_bytes, n_ctx);
    let safety_tier = classify_ram_safety_tier(estimated_ram_mb);
    let default_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown-gguf")
        .to_string();

    ModelInfo {
        id: meta
            .and_then(|m| m.id.clone())
            .unwrap_or_else(|| build_model_id(source, path)),
        name: meta
            .and_then(|m| m.name.clone())
            .unwrap_or(default_name),
        kind: ModelKind::Llm,
        path: canonical.to_string_lossy().into_owned(),
        size_bytes,
        estimated_ram_mb,
        source,
        safety_tier,
        is_active,
        valid,
        notes,
    }
}

fn is_gguf_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn build_model_id(source: ModelSource, path: &Path) -> String {
    let prefix = match source {
        ModelSource::Bundled => "bundled",
        ModelSource::UserData => "user",
        ModelSource::Custom => "custom",
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model");
    format!("{prefix}:{stem}")
}

fn load_manifest_metadata(dir: &Path) -> HashMap<String, ManifestMeta> {
    let manifest_path = dir.join("manifest.json");
    let Ok(raw) = fs::read_to_string(&manifest_path) else {
        return HashMap::new();
    };
    let Ok(json) = serde_json::from_str::<Value>(&raw) else {
        return HashMap::new();
    };

    let mut out = HashMap::new();
    match json {
        Value::Object(ref map) => {
            if let Some(Value::Array(items)) = map.get("models") {
                for item in items {
                    insert_manifest_entry(item, &mut out);
                }
            } else {
                insert_manifest_entry(&json, &mut out);
            }
        }
        Value::Array(ref items) => {
            for item in items {
                insert_manifest_entry(item, &mut out);
            }
        }
        _ => {}
    }
    out
}

fn insert_manifest_entry(entry: &Value, out: &mut HashMap<String, ManifestMeta>) {
    let Some(obj) = entry.as_object() else {
        return;
    };
    let file_key = obj
        .get("filename")
        .or_else(|| obj.get("file"))
        .or_else(|| obj.get("path"))
        .and_then(Value::as_str)
        .and_then(|s| Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .map(ToString::to_string);
    let Some(key) = file_key else {
        return;
    };

    let meta = ManifestMeta {
        id: obj.get("id").and_then(Value::as_str).map(ToString::to_string),
        name: obj
            .get("name")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        notes: obj
            .get("notes")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    };
    out.insert(key, meta);
}
