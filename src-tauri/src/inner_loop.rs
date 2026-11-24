use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use brain_llm::{InnerLifePromptContext, LlmBrainManager, current_time_summary};
use chrono::Timelike;
use shared_types::{
    AvatarLogicalState, BrainEngine, CharacterId, InnerLoopBeatKind, InnerLoopConfig,
    InnerLoopStatus, MemoryEngine, ProactiveMessageEvent, SystemRamStatus, VoiceEngine, events,
    now_timestamp_ms,
};
use soul_memory::SoulMemoryStore;
use tauri::{AppHandle, Emitter};
use tracing::{info, warn};
use voice_io::VoicePipeline;

use crate::avatar_state::AvatarStateServer;

const LOW_RAM_8GB_AVAILABLE_MIN_MB: u64 = 900;
const TICKER_POLL_INTERVAL_SECS: u64 = 30;

/// Inputs sampled for evaluating G.R.I.L.L.O. inner-loop safety guards.
#[derive(Debug, Clone)]
pub struct BeatGuardInput {
    pub has_active_model: bool,
    pub voice_busy: bool,
    pub ram: SystemRamStatus,
    pub now_ms: i64,
    pub local_hour: u8,
}

/// G.R.I.L.L.O. (Gentle Reflective Inner Loop Lifecycle Orchestrator) background task manager.
///
/// Strictly opt-in (`enabled: false` by default) and guarded against running without a loaded
/// LLM, during active chat/voice/recent user activity, during quiet hours, or under 8GB RAM pressure.
#[derive(Debug)]
pub struct InnerLoopManager {
    status: RwLock<InnerLoopStatus>,
    last_user_activity_ms: AtomicI64,
    chat_busy: Arc<AtomicBool>,
}

impl InnerLoopManager {
    /// Initialize `InnerLoopManager` from persisted [`InnerLoopConfig`].
    pub fn new(config: InnerLoopConfig) -> Self {
        let now = now_timestamp_ms();
        let next_beat_at_ms = if config.enabled {
            Some(now + interval_ms(&config))
        } else {
            None
        };
        let initial_kind = if config.enabled {
            None
        } else {
            Some(InnerLoopBeatKind::SkippedDisabled)
        };
        let initial_summary = if config.enabled {
            Some(format!("Armed (every {} min).", config.interval_minutes.max(1)))
        } else {
            Some("Inner Loop is disabled (opt-in).".to_string())
        };

        Self {
            status: RwLock::new(InnerLoopStatus {
                config,
                last_beat_at_ms: None,
                last_beat_kind: initial_kind,
                last_beat_summary: initial_summary,
                next_beat_at_ms,
                total_beats: 0,
            }),
            last_user_activity_ms: AtomicI64::new(now),
            chat_busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Return the shared `chat_busy` flag used by the Single Message Chain.
    pub fn chat_busy_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.chat_busy)
    }

    /// Record user activity timestamp to reset the idle cooldown window.
    pub fn record_user_activity(&self) {
        self.last_user_activity_ms
            .store(now_timestamp_ms(), Ordering::Release);
    }

    /// Set last user activity timestamp explicitly (useful for deterministic unit tests).
    pub fn set_last_user_activity_ms(&self, timestamp_ms: i64) {
        self.last_user_activity_ms
            .store(timestamp_ms, Ordering::Release);
    }

    /// Set `chat_busy` flag directly.
    pub fn set_chat_busy(&self, busy: bool) {
        self.chat_busy.store(busy, Ordering::Release);
    }

    /// Return a cloned snapshot of [`InnerLoopStatus`].
    pub fn status(&self) -> InnerLoopStatus {
        self.status
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Update [`InnerLoopConfig`] and recalculate `next_beat_at_ms`, returning the updated status.
    pub fn update_config(&self, mut config: InnerLoopConfig) -> InnerLoopStatus {
        config.interval_minutes = config.interval_minutes.clamp(1, 240);
        let now = now_timestamp_ms();
        let mut guard = self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.config = config.clone();
        if config.enabled {
            guard.next_beat_at_ms = Some(now + interval_ms(&config));
            if guard.last_beat_kind == Some(InnerLoopBeatKind::SkippedDisabled) {
                guard.last_beat_kind = None;
            }
            guard.last_beat_summary = Some(format!(
                "Armed (every {} min).",
                config.interval_minutes
            ));
        } else {
            guard.next_beat_at_ms = None;
            guard.last_beat_kind = Some(InnerLoopBeatKind::SkippedDisabled);
            guard.last_beat_summary = Some("Inner Loop is disabled (opt-in).".to_string());
        }
        guard.clone()
    }

    /// Evaluate safety guards deterministically without performing side effects.
    ///
    /// Returns `Some((InnerLoopBeatKind, reason))` when the beat must be a strict no-op,
    /// or `None` when all safety conditions pass.
    pub fn evaluate_guard(&self, input: &BeatGuardInput) -> Option<(InnerLoopBeatKind, String)> {
        let cfg = self.status().config;
        if !cfg.enabled {
            return Some((
                InnerLoopBeatKind::SkippedDisabled,
                "Inner Loop is disabled (opt-in).".to_string(),
            ));
        }

        if !input.has_active_model {
            return Some((
                InnerLoopBeatKind::SkippedNoModel,
                "Skipped: no local LLM (.gguf) is loaded (BYOM guard).".to_string(),
            ));
        }

        if self.chat_busy.load(Ordering::Acquire) {
            return Some((
                InnerLoopBeatKind::SkippedBusy,
                "Skipped: active chat generation in progress.".to_string(),
            ));
        }

        if input.voice_busy {
            return Some((
                InnerLoopBeatKind::SkippedBusy,
                "Skipped: voice TTS/ASR pipeline is active.".to_string(),
            ));
        }

        let last_activity = self.last_user_activity_ms.load(Ordering::Acquire);
        let idle_seconds = input.now_ms.saturating_sub(last_activity).max(0) as u64 / 1000;
        if idle_seconds < cfg.min_idle_seconds {
            return Some((
                InnerLoopBeatKind::SkippedBusy,
                format!(
                    "Skipped: user was active {idle_seconds}s ago (< {}s idle threshold).",
                    cfg.min_idle_seconds
                ),
            ));
        }

        if is_in_quiet_hours(cfg.quiet_hours_start, cfg.quiet_hours_end, input.local_hour) {
            return Some((
                InnerLoopBeatKind::SkippedBusy,
                format!("Skipped: currently inside quiet hours ({:02}:00).", input.local_hour),
            ));
        }

        if input.ram.is_8gb_class {
            let ceiling_90 = (input.ram.budget_ceiling_mb * 90) / 100;
            if input.ram.process_ram_mb >= ceiling_90
                || input.ram.available_ram_mb < LOW_RAM_8GB_AVAILABLE_MIN_MB
            {
                return Some((
                    InnerLoopBeatKind::SkippedLowRam,
                    format!(
                        "Skipped: 8GB RAM headroom guard (process={}MB / 90%-ceiling={}MB, available={}MB).",
                        input.ram.process_ram_mb, ceiling_90, input.ram.available_ram_mb
                    ),
                ));
            }
        }

        None
    }

    /// Execute one inner-loop beat check with live app dependencies, updating [`InnerLoopStatus`]
    /// and emitting `companion://inner-loop` (and optional `companion://proactive` / `avatar://state`).
    pub async fn run_beat(
        &self,
        app: Option<&AppHandle>,
        character_id: CharacterId,
        memory: &Arc<SoulMemoryStore>,
        brain: &Arc<LlmBrainManager>,
        voice: &Arc<VoicePipeline>,
        avatar: &Arc<AvatarStateServer>,
        enabled_actions: Vec<String>,
    ) -> InnerLoopStatus {
        let now_ms = now_timestamp_ms();
        let local_hour = chrono::Local::now().hour() as u8;
        let mut ram = brain.system_ram_status();
        ram.active_asr_id = voice.active_asr_model().map(|m| m.id);

        let guard_input = BeatGuardInput {
            has_active_model: brain.active_model().is_some(),
            voice_busy: voice.is_speaking() || voice.is_listening(),
            ram,
            now_ms,
            local_hour,
        };

        if let Some((kind, reason)) = self.evaluate_guard(&guard_input) {
            let status = self.record_beat_result(now_ms, kind, reason, false);
            if let Some(app_handle) = app {
                let _ = app_handle.emit(events::INNER_LOOP_STATUS, &status);
            }
            return status;
        }

        // Transition avatar to Write while consolidating/reflecting.
        let write_snap = avatar.set_logical_state(AvatarLogicalState::Write, None, None);
        if let Some(app_handle) = app {
            AvatarStateServer::emit_snapshot(app_handle, &write_snap);
        }

        let consolidated = match memory.consolidate_now().await {
            Ok(opt) => opt,
            Err(err) => {
                warn!("Inner loop consolidation failed: {err}");
                None
            }
        };

        let mem_for_profile = Arc::clone(memory);
        let decayed_profile = tokio::task::spawn_blocking(move || mem_for_profile.get_emotion_profile())
            .await
            .ok()
            .and_then(Result::ok);

        if let Some(ref profile) = decayed_profile {
            let idle_snap = avatar.set_logical_state(AvatarLogicalState::Idle, None, Some(profile.clone()));
            if let Some(app_handle) = app {
                AvatarStateServer::emit_snapshot(app_handle, &idle_snap);
            }
        } else {
            let idle_snap = avatar.set_logical_state(AvatarLogicalState::Idle, None, None);
            if let Some(app_handle) = app {
                AvatarStateServer::emit_snapshot(app_handle, &idle_snap);
            }
        }

        let mut beat_kind = if consolidated.is_some() {
            InnerLoopBeatKind::MemoryReflection
        } else {
            InnerLoopBeatKind::EmotionUpdate
        };

        let mut beat_summary = if let Some(ref diary) = consolidated {
            format!("Reflected and consolidated dream diary ({}).", diary.period_key)
        } else if let Some(ref profile) = decayed_profile {
            format!(
                "Updated emotion decay baseline (dominant: {} {:.2}).",
                profile.dominant, profile.dominant_intensity
            )
        } else {
            "Completed inner-loop reflection check.".to_string()
        };

        let cfg = self.status().config;
        if cfg.proactive_outreach_enabled && !self.chat_busy.load(Ordering::Acquire) {
            if let Ok(recent_turns) = memory.recent_turns(8).await
                && !recent_turns.is_empty()
            {
                let mem_ctx = memory
                    .retrieve_context("recent conversation mood reflection", None, 4, 2)
                    .await
                    .unwrap_or_default();
                let mem_bio = Arc::clone(memory);
                let (persona_bio, user_profile, recent_diary) = tokio::task::spawn_blocking(move || {
                    (
                        mem_bio.get_persona_bio().ok(),
                        mem_bio.get_user_profile().ok(),
                        mem_bio.list_diary_entries(1).ok().and_then(|v| v.into_iter().next()),
                    )
                })
                .await
                .unwrap_or((None, None, None));

                let inner_life = InnerLifePromptContext {
                    persona_bio,
                    user_profile,
                    emotion_profile: decayed_profile.clone(),
                    recent_diary,
                    current_time_summary: Some(current_time_summary()),
                    enabled_actions,
                };

                let prompt = "Share one gentle, warm, non-intrusive check-in sentence based on our mood and shared memories.";
                let lang_pref = voice.language_pref();
                if let Ok((reply, _)) = brain
                    .generate_reply_with_inner_life(
                        prompt,
                        &recent_turns,
                        &mem_ctx,
                        lang_pref,
                        &inner_life,
                        Box::new(|_| {}),
                        Box::new(|_| {}),
                    )
                    .await
                {
                    let content = reply.turn.content.trim().to_string();
                    if !content.is_empty() {
                        let mem_imp = Arc::clone(memory);
                        let emo_tag = reply.emotion.tag;
                        let emo_int = reply.emotion.intensity;
                        let updated_prof = tokio::task::spawn_blocking(move || {
                            mem_imp.record_emotion_impulse(emo_tag, emo_int)
                        })
                        .await
                        .ok()
                        .and_then(Result::ok);

                        let next_snap = avatar.set_emotion(reply.emotion.clone(), updated_prof);
                        if let Some(app_handle) = app {
                            AvatarStateServer::emit_snapshot(app_handle, &next_snap);
                            let proactive = ProactiveMessageEvent {
                                character_id,
                                content: content.clone(),
                                emotion: reply.emotion,
                                reason: "Gentle inner-loop reflection check-in".to_string(),
                                timestamp_ms: now_timestamp_ms(),
                            };
                            let _ = app_handle.emit(events::PROACTIVE_MESSAGE, &proactive);
                        }
                        beat_kind = InnerLoopBeatKind::ProactiveOutreach;
                        beat_summary = "Shared a gentle proactive check-in message.".to_string();
                    }
                }
            }
        }

        let status = self.record_beat_result(now_ms, beat_kind, beat_summary, true);
        if let Some(app_handle) = app {
            let _ = app_handle.emit(events::INNER_LOOP_STATUS, &status);
        }
        info!(?beat_kind, character = %character_id, "G.R.I.L.L.O. inner-loop beat completed");
        status
    }

    fn record_beat_result(
        &self,
        now_ms: i64,
        kind: InnerLoopBeatKind,
        summary: String,
        increment_total: bool,
    ) -> InnerLoopStatus {
        let mut guard = self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.last_beat_at_ms = Some(now_ms);
        guard.last_beat_kind = Some(kind);
        guard.last_beat_summary = Some(summary);
        guard.next_beat_at_ms = if guard.config.enabled {
            Some(now_ms + interval_ms(&guard.config))
        } else {
            None
        };
        if increment_total {
            guard.total_beats = guard.total_beats.saturating_add(1);
        }
        guard.clone()
    }

    /// Return whether an automatic beat is due based on `next_beat_at_ms` and `now_ms`.
    pub fn is_beat_due(&self, now_ms: i64) -> bool {
        let guard = self.status();
        guard.config.enabled
            && guard
                .next_beat_at_ms
                .is_some_and(|next_at| now_ms >= next_at)
    }

    /// Background ticker polling every 30s to refresh emotion decay and fire scheduled inner-loop beats.
    pub fn spawn_background_loops(
        app: AppHandle,
        shutdown_done: Arc<AtomicBool>,
        coral_memory: Arc<SoulMemoryStore>,
        kai_memory: Arc<SoulMemoryStore>,
        active_character: Arc<RwLock<CharacterId>>,
        brain: Arc<LlmBrainManager>,
        voice: Arc<VoicePipeline>,
        avatar: Arc<AvatarStateServer>,
        inner_loop: Arc<InnerLoopManager>,
        plugin_registry: Arc<brain_llm::PluginRegistry>,
    ) {
        tauri::async_runtime::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(TICKER_POLL_INTERVAL_SECS));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Consume initial immediate tick so startup stays completely idle.
            ticker.tick().await;

            loop {
                ticker.tick().await;
                if shutdown_done.load(Ordering::Acquire) {
                    break;
                }

                let char_id = active_character
                    .read()
                    .map(|g| *g)
                    .unwrap_or(CharacterId::Coral);
                let mem = match char_id {
                    CharacterId::Coral => Arc::clone(&coral_memory),
                    CharacterId::Kai => Arc::clone(&kai_memory),
                };

                // 30s lightweight SQLite emotion-decay refresh ticker -> avatar://state.
                let mem_decay = Arc::clone(&mem);
                if let Ok(Ok(profile)) =
                    tokio::task::spawn_blocking(move || mem_decay.get_emotion_profile()).await
                {
                    let snap = avatar.update_emotion_profile(profile);
                    AvatarStateServer::emit_snapshot(&app, &snap);
                }

                // Trigger scheduled G.R.I.L.L.O. inner-loop beat when enabled & due.
                if inner_loop.is_beat_due(now_timestamp_ms()) {
                    let actions = plugin_registry.enabled_actions();
                    let _ = inner_loop
                        .run_beat(
                            Some(&app),
                            char_id,
                            &mem,
                            &brain,
                            &voice,
                            &avatar,
                            actions,
                        )
                        .await;
                }
            }
        });
    }
}

fn interval_ms(config: &InnerLoopConfig) -> i64 {
    i64::from(config.interval_minutes.max(1)) * 60_000
}

/// Returns `true` when `local_hour` falls inside `[start, end)` (handling midnight wrap-around).
pub fn is_in_quiet_hours(start: Option<u8>, end: Option<u8>, local_hour: u8) -> bool {
    let (Some(s), Some(e)) = (start, end) else {
        return false;
    };
    let s = s % 24;
    let e = e % 24;
    let h = local_hour % 24;
    if s == e {
        return false;
    }
    if s < e {
        h >= s && h < e
    } else {
        h >= s || h < e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_16gb_ram() -> SystemRamStatus {
        SystemRamStatus {
            total_ram_mb: 16_384,
            available_ram_mb: 8_000,
            process_ram_mb: 1_200,
            budget_ceiling_mb: 11_468,
            is_8gb_class: false,
            active_llm_id: Some("qwen3-4b".into()),
            active_asr_id: None,
        }
    }

    fn base_input(now_ms: i64) -> BeatGuardInput {
        BeatGuardInput {
            has_active_model: true,
            voice_busy: false,
            ram: safe_16gb_ram(),
            now_ms,
            local_hour: 14,
        }
    }

    #[test]
    fn inner_loop_safety_guards_enforce_all_four_skips() {
        let now = 1_000_000_i64;
        // 1. SkippedDisabled when config.enabled == false
        let disabled_mgr = InnerLoopManager::new(InnerLoopConfig::default());
        disabled_mgr.set_last_user_activity_ms(now - 300_000);
        let res = disabled_mgr.evaluate_guard(&base_input(now));
        assert_eq!(res.map(|(k, _)| k), Some(InnerLoopBeatKind::SkippedDisabled));

        // Enable for remaining checks
        let enabled_cfg = InnerLoopConfig {
            enabled: true,
            interval_minutes: 20,
            proactive_outreach_enabled: true,
            quiet_hours_start: Some(23),
            quiet_hours_end: Some(7),
            min_idle_seconds: 120,
        };
        let mgr = InnerLoopManager::new(enabled_cfg);
        mgr.set_last_user_activity_ms(now - 300_000);

        // 2. SkippedNoModel when brain.active_model().is_none()
        let mut no_model = base_input(now);
        no_model.has_active_model = false;
        assert_eq!(
            mgr.evaluate_guard(&no_model).map(|(k, _)| k),
            Some(InnerLoopBeatKind::SkippedNoModel)
        );

        // 3. SkippedBusy: chat_busy, voice_busy, recent idle < min_idle_seconds, or quiet_hours
        mgr.set_chat_busy(true);
        assert_eq!(
            mgr.evaluate_guard(&base_input(now)).map(|(k, _)| k),
            Some(InnerLoopBeatKind::SkippedBusy)
        );
        mgr.set_chat_busy(false);

        let mut voice_busy = base_input(now);
        voice_busy.voice_busy = true;
        assert_eq!(
            mgr.evaluate_guard(&voice_busy).map(|(k, _)| k),
            Some(InnerLoopBeatKind::SkippedBusy)
        );

        mgr.set_last_user_activity_ms(now - 30_000); // 30s < 120s
        assert_eq!(
            mgr.evaluate_guard(&base_input(now)).map(|(k, _)| k),
            Some(InnerLoopBeatKind::SkippedBusy)
        );
        mgr.set_last_user_activity_ms(now - 300_000);

        let mut quiet = base_input(now);
        quiet.local_hour = 2; // inside 23..7
        assert_eq!(
            mgr.evaluate_guard(&quiet).map(|(k, _)| k),
            Some(InnerLoopBeatKind::SkippedBusy)
        );

        // 4. SkippedLowRam on 8GB-class host (>= 90% ceiling or < 900 MB available)
        let mut low_ram_pct = base_input(now);
        low_ram_pct.ram = SystemRamStatus {
            total_ram_mb: 8_192,
            available_ram_mb: 1_800,
            process_ram_mb: 4_320, // 90% of 4800
            budget_ceiling_mb: 4_800,
            is_8gb_class: true,
            active_llm_id: Some("qwen3-4b".into()),
            active_asr_id: None,
        };
        assert_eq!(
            mgr.evaluate_guard(&low_ram_pct).map(|(k, _)| k),
            Some(InnerLoopBeatKind::SkippedLowRam)
        );

        let mut low_ram_avail = base_input(now);
        low_ram_avail.ram = SystemRamStatus {
            total_ram_mb: 8_192,
            available_ram_mb: 850, // < 900 MB
            process_ram_mb: 2_500,
            budget_ceiling_mb: 4_800,
            is_8gb_class: true,
            active_llm_id: Some("qwen3-4b".into()),
            active_asr_id: None,
        };
        assert_eq!(
            mgr.evaluate_guard(&low_ram_avail).map(|(k, _)| k),
            Some(InnerLoopBeatKind::SkippedLowRam)
        );

        // 5. Passes all guards when safe!
        assert!(mgr.evaluate_guard(&base_input(now)).is_none());
    }
}
