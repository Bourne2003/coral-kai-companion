use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shared_types::{InnerLoopConfig, McpConfig, ModelKind, SoulResult};
use tracing::warn;

fn default_active_character() -> String {
    "coral".to_string()
}

fn normalize_character_id(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "kai" | "kai_companion" => "kai".to_string(),
        _ => "coral".to_string(),
    }
}

/// Persisted custom BYOM model or Live2D bundle reference saved in `models_config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedModelEntry {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// Persistent user BYOM, Inner Loop, and optional MCP configuration stored at `app_data_dir/models_config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsConfig {
    #[serde(default = "default_active_character")]
    pub active_character: String,
    #[serde(default)]
    pub llm: Vec<SavedModelEntry>,
    #[serde(default)]
    pub asr: Vec<SavedModelEntry>,
    #[serde(default)]
    pub embed: Vec<SavedModelEntry>,
    #[serde(default)]
    pub live2d: Vec<SavedModelEntry>,
    #[serde(default)]
    pub inner_loop: InnerLoopConfig,
    #[serde(default)]
    pub mcp: McpConfig,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            active_character: default_active_character(),
            llm: Vec::new(),
            asr: Vec::new(),
            embed: Vec::new(),
            live2d: Vec::new(),
            inner_loop: InnerLoopConfig::default(),
            mcp: McpConfig::default(),
        }
    }
}

impl ModelsConfig {
    /// Load `models_config.json` from disk, returning defaults cleanly if missing or malformed.
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(mut cfg) => {
                cfg.active_character = normalize_character_id(&cfg.active_character);
                cfg
            }
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "Ignoring malformed models_config.json and using defaults"
                );
                Self::default()
            }
        }
    }

    /// Persist `models_config.json` to disk, creating parent directories if necessary.
    pub fn save(&self, path: &Path) -> SoulResult<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Convert saved entries into `(PathBuf, Option<String>)` pairs for crate restoration helpers.
    pub fn as_restore_tuples(entries: &[SavedModelEntry]) -> Vec<(PathBuf, Option<String>)> {
        entries
            .iter()
            .map(|e| (PathBuf::from(&e.path), Some(e.name.clone())))
            .collect()
    }

    /// Insert or update a custom model entry for `kind`, deduplicating by `path` or `id`.
    pub fn upsert_entry(&mut self, kind: ModelKind, entry: SavedModelEntry) {
        let list = self.entries_mut(kind);
        let entry_canon = fs::canonicalize(&entry.path).ok();
        if let Some(existing) = list.iter_mut().find(|item| {
            item.id == entry.id
                || item.path == entry.path
                || (entry_canon.is_some() && fs::canonicalize(&item.path).ok() == entry_canon)
        }) {
            *existing = entry;
        } else {
            list.push(entry);
        }
    }

    /// Remove a custom model entry matching `id_or_path`, returning `true` if removed.
    pub fn remove_entry(&mut self, kind: ModelKind, id_or_path: &str) -> bool {
        let list = self.entries_mut(kind);
        let target_canon = fs::canonicalize(id_or_path).ok();
        let before = list.len();
        list.retain(|item| {
            if item.id == id_or_path || item.path == id_or_path {
                return false;
            }
            if let Some(ref tc) = target_canon
                && fs::canonicalize(&item.path).ok().as_ref() == Some(tc)
            {
                return false;
            }
            true
        });
        list.len() < before
    }

    fn entries_mut(&mut self, kind: ModelKind) -> &mut Vec<SavedModelEntry> {
        match kind {
            ModelKind::Llm => &mut self.llm,
            ModelKind::Asr => &mut self.asr,
            ModelKind::Embed => &mut self.embed,
            ModelKind::Live2d => &mut self.live2d,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_malformed_config_fall_back_safely() {
        let dir = std::env::temp_dir().join(format!("soulmate2d_cfg_{}", shared_types::new_id()));
        let missing = dir.join("nested/models_config.json");
        let cfg = ModelsConfig::load(&missing);
        assert_eq!(cfg, ModelsConfig::default());
        assert_eq!(cfg.active_character, "coral");

        fs::create_dir_all(missing.parent().unwrap()).unwrap();
        fs::write(&missing, "{not valid json!!!").unwrap();
        let malformed = ModelsConfig::load(&missing);
        assert_eq!(malformed, ModelsConfig::default());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_roundtrip_upsert_and_remove() {
        let dir = std::env::temp_dir().join(format!("soulmate2d_cfg_rt_{}", shared_types::new_id()));
        let cfg_path = dir.join("sub/models_config.json");

        let mut cfg = ModelsConfig::default();
        cfg.active_character = "kai".to_string();
        cfg.upsert_entry(
            ModelKind::Llm,
            SavedModelEntry {
                id: "qwen".into(),
                name: "Qwen 3B".into(),
                path: "/tmp/qwen.gguf".into(),
            },
        );
        cfg.upsert_entry(
            ModelKind::Llm,
            SavedModelEntry {
                id: "qwen".into(),
                name: "Qwen 3B Updated".into(),
                path: "/tmp/qwen.gguf".into(),
            },
        );
        cfg.upsert_entry(
            ModelKind::Embed,
            SavedModelEntry {
                id: "embed-1".into(),
                name: "MiniLM".into(),
                path: "/tmp/minilm.onnx".into(),
            },
        );
        cfg.save(&cfg_path).unwrap();

        let loaded = ModelsConfig::load(&cfg_path);
        assert_eq!(loaded.active_character, "kai");
        assert_eq!(loaded.llm.len(), 1);
        assert_eq!(loaded.llm[0].name, "Qwen 3B Updated");
        assert_eq!(loaded.embed.len(), 1);

        let mut mutated = loaded;
        assert!(mutated.remove_entry(ModelKind::Llm, "qwen"));
        assert!(!mutated.remove_entry(ModelKind::Llm, "missing"));
        assert!(mutated.llm.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inner_loop_and_mcp_default_to_disabled_and_roundtrip() {
        let dir = std::env::temp_dir().join(format!("soulmate2d_cfg_il_{}", shared_types::new_id()));
        let cfg_path = dir.join("models_config.json");

        let mut cfg = ModelsConfig::default();
        assert!(!cfg.inner_loop.enabled);
        assert!(!cfg.mcp.enabled);

        cfg.inner_loop.enabled = true;
        cfg.inner_loop.interval_minutes = 30;
        cfg.mcp.enabled = true;
        cfg.mcp.config_path = Some("/tmp/mcp.json".into());
        cfg.save(&cfg_path).unwrap();

        let reloaded = ModelsConfig::load(&cfg_path);
        assert!(reloaded.inner_loop.enabled);
        assert_eq!(reloaded.inner_loop.interval_minutes, 30);
        assert!(reloaded.mcp.enabled);
        assert_eq!(reloaded.mcp.config_path.as_deref(), Some("/tmp/mcp.json"));

        let _ = fs::remove_dir_all(&dir);
    }
}
