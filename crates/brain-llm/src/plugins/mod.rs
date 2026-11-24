pub mod builtins;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::{Map, Value};
use shared_types::{
    ActionInvocation, ActionResult, CharacterId, DiaryEntry, EmotionTag, PersonaBio, PluginInfo,
    SoulResult, UserProfile,
};

use self::builtins::{BioUpdaterPlugin, DiaryWriterPlugin, TimeAwarenessPlugin};

/// Host-side persistence sink injected by the application layer so plugins can mutate
/// companion diaries, self-bio, and user profile without coupling `brain-llm` to SQLite.
#[async_trait]
pub trait PluginActionSink: Send + Sync {
    async fn write_diary(
        &self,
        title: &str,
        content: &str,
        mood: EmotionTag,
    ) -> SoulResult<DiaryEntry>;

    async fn update_persona_bio(
        &self,
        traits: Option<String>,
        self_reflection: Option<String>,
        favorite_topics: Option<String>,
    ) -> SoulResult<PersonaBio>;

    async fn update_user_profile(
        &self,
        display_name: Option<String>,
        timezone: Option<String>,
        location: Option<String>,
        notes: Option<String>,
    ) -> SoulResult<UserProfile>;
}

/// Execution context passed to plugins when dispatching an action.
#[derive(Clone, Default)]
pub struct PluginContext {
    pub character_id: CharacterId,
    pub user_profile: Option<UserProfile>,
    pub persona_bio: Option<PersonaBio>,
    pub sink: Option<Arc<dyn PluginActionSink>>,
}

impl std::fmt::Debug for PluginContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginContext")
            .field("character_id", &self.character_id)
            .field("has_user_profile", &self.user_profile.is_some())
            .field("has_persona_bio", &self.persona_bio.is_some())
            .field("has_sink", &self.sink.is_some())
            .finish()
    }
}

/// Modular companion capability plugin contract.
#[async_trait]
pub trait CompanionPlugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn builtin(&self) -> bool;
    fn supported_actions(&self) -> Vec<String>;
    async fn execute(
        &self,
        action: &str,
        params: &Value,
        ctx: &PluginContext,
    ) -> SoulResult<ActionResult>;
}

struct RegisteredPlugin {
    plugin: Arc<dyn CompanionPlugin>,
    enabled: bool,
}

/// Thread-safe registry for built-in and optional external companion plugins.
///
/// Fault-isolated: dispatching missing, disabled, or failing plugins never panics or
/// aborts chat; it always returns a structured [`ActionResult`].
#[derive(Default)]
pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, RegisteredPlugin>>,
    order: RwLock<Vec<String>>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("plugins", &self.list_plugins())
            .finish()
    }
}

impl PluginRegistry {
    /// Creates an empty plugin registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a registry pre-populated with all built-in offline companion plugins.
    pub fn new_with_builtins() -> Self {
        let registry = Self::new();
        registry.register(Arc::new(TimeAwarenessPlugin));
        registry.register(Arc::new(DiaryWriterPlugin));
        registry.register(Arc::new(BioUpdaterPlugin));
        registry
    }

    /// Registers (or replaces) a plugin, enabling it by default.
    pub fn register(&self, plugin: Arc<dyn CompanionPlugin>) {
        let key = plugin.name().to_string();
        if let (Ok(mut map), Ok(mut order)) = (self.plugins.write(), self.order.write()) {
            if !map.contains_key(&key) {
                order.push(key.clone());
            }
            map.insert(
                key,
                RegisteredPlugin {
                    plugin,
                    enabled: true,
                },
            );
        }
    }

    /// Removes a plugin by name, returning `true` if it was present.
    pub fn unregister(&self, name: &str) -> bool {
        let removed = self
            .plugins
            .write()
            .map(|mut map| map.remove(name).is_some())
            .unwrap_or(false);
        if removed && let Ok(mut order) = self.order.write() {
            order.retain(|item| item != name);
        }
        removed
    }

    /// Enables or disables a registered plugin by name, returning `true` if found.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> bool {
        self.plugins
            .write()
            .map(|mut map| {
                if let Some(entry) = map.get_mut(name) {
                    entry.enabled = enabled;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }

    /// Lists all registered plugins and their supported actions in registration order.
    pub fn list_plugins(&self) -> Vec<PluginInfo> {
        let Ok(map) = self.plugins.read() else {
            return Vec::new();
        };
        let Ok(order) = self.order.read() else {
            return Vec::new();
        };
        order
            .iter()
            .filter_map(|name| map.get(name))
            .map(|entry| PluginInfo {
                name: entry.plugin.name().to_string(),
                description: entry.plugin.description().to_string(),
                enabled: entry.enabled,
                builtin: entry.plugin.builtin(),
                supported_actions: entry.plugin.supported_actions(),
            })
            .collect()
    }

    /// Returns the list of action names exposed by currently enabled plugins.
    pub fn enabled_actions(&self) -> Vec<String> {
        let Ok(map) = self.plugins.read() else {
            return Vec::new();
        };
        let Ok(order) = self.order.read() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        for name in order.iter() {
            if let Some(entry) = map.get(name)
                && entry.enabled
            {
                actions.extend(entry.plugin.supported_actions());
            }
        }
        actions
    }

    /// Dispatches a single action cleanly, returning `ActionResult { success: false, ... }`
    /// if the action is unknown, its plugin is disabled, or execution fails.
    pub async fn dispatch(
        &self,
        action: &str,
        params: &Value,
        ctx: &PluginContext,
    ) -> ActionResult {
        let target_action = action.trim();
        if target_action.is_empty() {
            return ActionResult {
                action: String::new(),
                plugin_name: String::new(),
                success: false,
                summary: "Empty action name".to_string(),
                data: None,
            };
        }

        let lookup = {
            let Ok(map) = self.plugins.read() else {
                return ActionResult {
                    action: target_action.to_string(),
                    plugin_name: String::new(),
                    success: false,
                    summary: "Plugin registry lock poisoned".to_string(),
                    data: None,
                };
            };
            let Ok(order) = self.order.read() else {
                return ActionResult {
                    action: target_action.to_string(),
                    plugin_name: String::new(),
                    success: false,
                    summary: "Plugin order lock poisoned".to_string(),
                    data: None,
                };
            };

            let mut matched: Option<(String, bool, Arc<dyn CompanionPlugin>)> = None;
            for name in order.iter() {
                if let Some(entry) = map.get(name)
                    && entry
                        .plugin
                        .supported_actions()
                        .iter()
                        .any(|a| a == target_action)
                {
                    matched = Some((name.clone(), entry.enabled, Arc::clone(&entry.plugin)));
                    if entry.enabled {
                        break;
                    }
                }
            }
            matched
        };

        let Some((plugin_name, enabled, plugin)) = lookup else {
            return ActionResult {
                action: target_action.to_string(),
                plugin_name: String::new(),
                success: false,
                summary: format!("No plugin registered for action '{target_action}'"),
                data: None,
            };
        };

        if !enabled {
            return ActionResult {
                action: target_action.to_string(),
                plugin_name: plugin_name.clone(),
                success: false,
                summary: format!("Plugin '{plugin_name}' is disabled"),
                data: None,
            };
        }

        match plugin.execute(target_action, params, ctx).await {
            Ok(mut result) => {
                if result.action.is_empty() {
                    result.action = target_action.to_string();
                }
                if result.plugin_name.is_empty() {
                    result.plugin_name = plugin_name;
                }
                result
            }
            Err(err) => {
                tracing::warn!(
                    plugin = %plugin_name,
                    action = %target_action,
                    error = %err,
                    "Plugin action execution failed"
                );
                ActionResult {
                    action: target_action.to_string(),
                    plugin_name,
                    success: false,
                    summary: err.to_string(),
                    data: None,
                }
            }
        }
    }

    /// Dispatches a sequence of parsed [`ActionInvocation`]s in order, isolating any per-action failures.
    pub async fn dispatch_invocations(
        &self,
        invocations: &[ActionInvocation],
        ctx: &PluginContext,
    ) -> Vec<ActionResult> {
        let mut results = Vec::with_capacity(invocations.len());
        for inv in invocations {
            results.push(self.dispatch(&inv.action, &inv.params, ctx).await);
        }
        results
    }
}

/// Extracts optional `[action: <name>, <json or key=value params>]` directives from text
/// without leaking directive tags into the spoken reply.
pub fn parse_action_directives(text: &str) -> (String, Vec<ActionInvocation>) {
    let mut cleaned = String::with_capacity(text.len());
    let mut invocations = Vec::new();
    let mut cursor = 0usize;

    while cursor < text.len() {
        let rest = &text[cursor..];
        let Some(rel_open) = find_next_action_open(rest) else {
            cleaned.push_str(rest);
            break;
        };

        let abs_open = cursor + rel_open;
        let before = &text[cursor..abs_open];
        cleaned.push_str(before);

        let after_bracket = abs_open + 1;
        let Some(colon_rel) = text[after_bracket..].find(':') else {
            // Unreachable because `find_next_action_open` verified `action:`.
            break;
        };
        let body_start = after_bracket + colon_rel + 1;

        let (inner, next_cursor, is_closed) = scan_directive_body(&text[body_start..], body_start);
        if is_closed && let Some(inv) = parse_directive_inner(inner) {
            invocations.push(inv);
        }

        // Prevent double spaces when removing an inline directive like `"Hi [action: ...] there"`.
        if cleaned.ends_with(' ') && text[next_cursor..].starts_with(' ') {
            let trimmed_spaces = text[next_cursor..].trim_start_matches(' ');
            let space_count = text[next_cursor..].len() - trimmed_spaces.len();
            cursor = next_cursor + space_count;
        } else {
            cursor = next_cursor;
        }
    }

    let normalized = normalize_cleaned_text(&cleaned);
    (normalized, invocations)
}

/// Finds the byte offset of the next `[action:` (allowing optional ASCII spaces between `[` and `action:`).
fn find_next_action_open(haystack: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == b'[' {
            let after = haystack[idx + 1..].trim_start_matches([' ', '\t']);
            if after.len() >= 7 && after[..7].eq_ignore_ascii_case("action:") {
                return Some(idx);
            }
        }
        idx += 1;
        while idx < bytes.len() && !haystack.is_char_boundary(idx) {
            idx += 1;
        }
    }
    None
}

/// Scans from immediately after `[action:` to the matching outer `]` (respecting quotes and nested `{}`/`[]`).
/// Returns `(inner_slice, next_cursor, is_closed)`. Unclosed directives are still stripped cleanly
/// without leaking `[action:` tags, while `is_closed == false` prevents bogus invocation parsing.
fn scan_directive_body(slice: &str, base_offset: usize) -> (&str, usize, bool) {
    let mut in_quote: Option<char> = None;
    let mut escaped = false;
    let mut brace_depth: usize = 0;
    let mut bracket_depth: usize = 0;
    let mut first_same_line_close: Option<usize> = None;

    for (idx, ch) in slice.char_indices() {
        if let Some(q) = in_quote {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == q {
                in_quote = None;
            }
            continue;
        }

        match ch {
            '"' | '\'' => in_quote = Some(ch),
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            '[' => bracket_depth += 1,
            ']' => {
                if first_same_line_close.is_none() {
                    first_same_line_close = Some(idx);
                }
                if brace_depth == 0 && bracket_depth == 0 {
                    return (&slice[..idx], base_offset + idx + 1, true);
                }
                bracket_depth = bracket_depth.saturating_sub(1);
            }
            '\n' if brace_depth == 0 && bracket_depth == 0 && first_same_line_close.is_none() => {
                // Unclosed single-line directive; strip up to newline so tag does not leak.
                return (&slice[..idx], base_offset + idx, false);
            }
            _ => {}
        }
    }

    if let Some(close_idx) = first_same_line_close {
        (&slice[..close_idx], base_offset + close_idx + 1, true)
    } else {
        (slice, base_offset + slice.len(), false)
    }
}

fn parse_directive_inner(inner: &str) -> Option<ActionInvocation> {
    let trimmed = inner.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (raw_name, raw_params) = match trimmed.split_once(',') {
        Some((name, rest)) => (name.trim(), rest.trim()),
        None => (trimmed, ""),
    };

    let action_name = raw_name.trim_matches(['"', '\'']).trim();
    if action_name.is_empty() || action_name.contains(char::is_whitespace) {
        return None;
    }

    if raw_params.is_empty() {
        return Some(ActionInvocation {
            action: action_name.to_string(),
            params: Value::Object(Map::new()),
        });
    }

    if raw_params.starts_with('{') {
        return match serde_json::from_str::<Value>(raw_params) {
            Ok(Value::Object(map)) => Some(ActionInvocation {
                action: action_name.to_string(),
                params: Value::Object(map),
            }),
            Ok(_) | Err(_) => None,
        };
    }

    let map = parse_key_value_params(raw_params)?;
    Some(ActionInvocation {
        action: action_name.to_string(),
        params: Value::Object(map),
    })
}

fn parse_key_value_params(input: &str) -> Option<Map<String, Value>> {
    let mut map = Map::new();
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut i = 0usize;

    while i < chars.len() {
        while i < chars.len() && (chars[i].1.is_whitespace() || chars[i].1 == ',') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        let key_start = chars[i].0;
        let mut eq_idx = None;
        while i < chars.len() {
            if chars[i].1 == '=' {
                eq_idx = Some(chars[i].0);
                i += 1;
                break;
            }
            if chars[i].1 == ',' {
                // Malformed entry without `=`.
                return None;
            }
            i += 1;
        }

        let Some(eq_byte) = eq_idx else {
            return None;
        };
        let key = input[key_start..eq_byte].trim();
        if key.is_empty() {
            return None;
        }

        while i < chars.len() && chars[i].1.is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            map.insert(key.to_string(), Value::String(String::new()));
            break;
        }

        let val_str = if chars[i].1 == '"' || chars[i].1 == '\'' {
            let quote = chars[i].1;
            i += 1;
            let mut out = String::new();
            let mut escaped = false;
            let mut closed = false;
            while i < chars.len() {
                let ch = chars[i].1;
                i += 1;
                if escaped {
                    match ch {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        '\\' => out.push('\\'),
                        other => out.push(other),
                    }
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == quote {
                    closed = true;
                    break;
                }
                out.push(ch);
            }
            if !closed {
                return None;
            }
            out
        } else {
            let val_start = chars[i].0;
            let mut val_end = input.len();
            while i < chars.len() {
                if chars[i].1 == ',' {
                    val_end = chars[i].0;
                    i += 1;
                    break;
                }
                i += 1;
            }
            input[val_start..val_end].trim().to_string()
        };

        map.insert(key.to_string(), Value::String(val_str));
    }

    Some(map)
}

fn normalize_cleaned_text(raw: &str) -> String {
    let lines: Vec<String> = raw
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    lines.join("\n")
}
