use std::str::FromStr;

use async_trait::async_trait;
use chrono::{Local, Timelike};
use serde_json::{Value, json};
use shared_types::{ActionResult, EmotionTag, SoulError, SoulResult};

use super::{CompanionPlugin, PluginContext};

/// Returns a human-readable current local date/time summary for prompt enrichment.
pub fn current_time_summary() -> String {
    let now = Local::now();
    let day_part = classify_day_part(now.hour());
    format!(
        "{} ({}, {})",
        now.format("%Y-%m-%d %H:%M %Z"),
        now.format("%A"),
        day_part
    )
}

fn classify_day_part(hour: u32) -> &'static str {
    match hour {
        5..=11 => "morning",
        12..=16 => "afternoon",
        17..=20 => "evening",
        _ => "late_night",
    }
}

/// Built-in offline plugin providing current local time and part-of-day awareness.
#[derive(Debug, Default)]
pub struct TimeAwarenessPlugin;

#[async_trait]
impl CompanionPlugin for TimeAwarenessPlugin {
    fn name(&self) -> &'static str {
        "time_awareness"
    }

    fn description(&self) -> &'static str {
        "Provides local wall-clock time, date, weekday, and part-of-day awareness offline."
    }

    fn builtin(&self) -> bool {
        true
    }

    fn supported_actions(&self) -> Vec<String> {
        vec!["get_current_time".to_string(), "get_day_part".to_string()]
    }

    async fn execute(
        &self,
        action: &str,
        _params: &Value,
        _ctx: &PluginContext,
    ) -> SoulResult<ActionResult> {
        let now = Local::now();
        let day_part = classify_day_part(now.hour());

        match action {
            "get_current_time" => {
                let date = now.format("%Y-%m-%d").to_string();
                let time = now.format("%H:%M:%S").to_string();
                let weekday = now.format("%A").to_string();
                let iso = now.to_rfc3339();
                Ok(ActionResult {
                    action: action.to_string(),
                    plugin_name: self.name().to_string(),
                    success: true,
                    summary: format!("{date} {time} ({weekday}, {day_part})"),
                    data: Some(json!({
                        "iso": iso,
                        "date": date,
                        "time": time,
                        "weekday": weekday,
                        "hour": now.hour(),
                        "dayPart": day_part,
                    })),
                })
            }
            "get_day_part" => Ok(ActionResult {
                action: action.to_string(),
                plugin_name: self.name().to_string(),
                success: true,
                summary: format!("Current day part is {day_part}"),
                data: Some(json!({
                    "dayPart": day_part,
                    "hour": now.hour(),
                })),
            }),
            other => Err(SoulError::InvalidInput(format!(
                "Unsupported time_awareness action: {other}"
            ))),
        }
    }
}

/// Built-in offline plugin writing personal companion diary entries via [`PluginActionSink`].
#[derive(Debug, Default)]
pub struct DiaryWriterPlugin;

#[async_trait]
impl CompanionPlugin for DiaryWriterPlugin {
    fn name(&self) -> &'static str {
        "diary_writer"
    }

    fn description(&self) -> &'static str {
        "Writes or merges companion personal diary entries via the host memory sink."
    }

    fn builtin(&self) -> bool {
        true
    }

    fn supported_actions(&self) -> Vec<String> {
        vec!["write_diary".to_string()]
    }

    async fn execute(
        &self,
        action: &str,
        params: &Value,
        ctx: &PluginContext,
    ) -> SoulResult<ActionResult> {
        if action != "write_diary" {
            return Err(SoulError::InvalidInput(format!(
                "Unsupported diary_writer action: {action}"
            )));
        }

        let sink = ctx.sink.as_ref().ok_or_else(|| {
            SoulError::NotImplemented("No PluginActionSink configured for diary_writer".into())
        })?;

        let content = extract_non_empty_string(params, &["content", "body", "text", "reflection"])
            .ok_or_else(|| {
                SoulError::InvalidInput("write_diary requires a non-empty 'content' field".into())
            })?;
        let title = extract_non_empty_string(params, &["title", "subject"])
            .unwrap_or_else(|| "Daily Reflection".to_string());
        let mood = extract_non_empty_string(params, &["mood", "emotion"])
            .and_then(|raw| EmotionTag::from_str(&raw).ok())
            .unwrap_or(EmotionTag::Neutral);

        let entry = sink.write_diary(&title, &content, mood).await?;
        Ok(ActionResult {
            action: action.to_string(),
            plugin_name: self.name().to_string(),
            success: true,
            summary: format!("Saved diary entry '{}' ({})", entry.title, entry.mood),
            data: Some(serde_json::to_value(&entry)?),
        })
    }
}

/// Built-in offline plugin updating companion persona bio and user profile via [`PluginActionSink`].
#[derive(Debug, Default)]
pub struct BioUpdaterPlugin;

#[async_trait]
impl CompanionPlugin for BioUpdaterPlugin {
    fn name(&self) -> &'static str {
        "bio_updater"
    }

    fn description(&self) -> &'static str {
        "Updates the companion self-bio and remembered user profile fields."
    }

    fn builtin(&self) -> bool {
        true
    }

    fn supported_actions(&self) -> Vec<String> {
        vec![
            "update_bio".to_string(),
            "update_user_profile".to_string(),
        ]
    }

    async fn execute(
        &self,
        action: &str,
        params: &Value,
        ctx: &PluginContext,
    ) -> SoulResult<ActionResult> {
        let sink = ctx.sink.as_ref().ok_or_else(|| {
            SoulError::NotImplemented("No PluginActionSink configured for bio_updater".into())
        })?;

        match action {
            "update_bio" => {
                let traits = extract_non_empty_string(params, &["traits"]);
                let self_reflection =
                    extract_non_empty_string(params, &["self_reflection", "selfReflection"]);
                let favorite_topics =
                    extract_non_empty_string(params, &["favorite_topics", "favoriteTopics"]);

                if traits.is_none() && self_reflection.is_none() && favorite_topics.is_none() {
                    return Err(SoulError::InvalidInput(
                        "update_bio requires at least one of traits, self_reflection, favorite_topics"
                            .into(),
                    ));
                }

                let bio = sink
                    .update_persona_bio(traits, self_reflection, favorite_topics)
                    .await?;
                Ok(ActionResult {
                    action: action.to_string(),
                    plugin_name: self.name().to_string(),
                    success: true,
                    summary: format!("Updated persona bio for {}", bio.character_id),
                    data: Some(serde_json::to_value(&bio)?),
                })
            }
            "update_user_profile" => {
                let display_name =
                    extract_non_empty_string(params, &["display_name", "displayName", "name"]);
                let timezone = extract_non_empty_string(params, &["timezone", "tz"]);
                let location = extract_non_empty_string(params, &["location", "city"]);
                let notes = extract_non_empty_string(params, &["notes", "note"]);

                if display_name.is_none()
                    && timezone.is_none()
                    && location.is_none()
                    && notes.is_none()
                {
                    return Err(SoulError::InvalidInput(
                        "update_user_profile requires at least one profile field".into(),
                    ));
                }

                let profile = sink
                    .update_user_profile(display_name, timezone, location, notes)
                    .await?;
                Ok(ActionResult {
                    action: action.to_string(),
                    plugin_name: self.name().to_string(),
                    success: true,
                    summary: "Updated remembered user profile".to_string(),
                    data: Some(serde_json::to_value(&profile)?),
                })
            }
            other => Err(SoulError::InvalidInput(format!(
                "Unsupported bio_updater action: {other}"
            ))),
        }
    }
}

fn extract_non_empty_string(params: &Value, keys: &[&str]) -> Option<String> {
    let obj = params.as_object()?;
    for key in keys {
        if let Some(val) = obj.get(*key) {
            let text = match val {
                Value::String(s) => s.trim().to_string(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}
