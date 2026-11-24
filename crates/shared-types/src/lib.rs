use std::fmt;
use std::path::Path;
use std::str::FromStr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Canonical Tauri event names shared across backend and frontend.
pub mod events {
    pub const CHAT_TOKEN: &str = "chat://token";
    pub const AVATAR_EMOTION: &str = "avatar://emotion";
    pub const AVATAR_VISEME: &str = "avatar://viseme";
    pub const AVATAR_STATE: &str = "avatar://state";
    pub const VOICE_TRANSCRIPT: &str = "voice://transcript";
    pub const SYSTEM_RAM: &str = "system://ram";
    pub const PROACTIVE_MESSAGE: &str = "companion://proactive";
    pub const INNER_LOOP_STATUS: &str = "companion://inner-loop";
}

/// Unified error enum across Coral&Kai Companion crates and Tauri IPC boundaries.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "type", content = "detail", rename_all = "camelCase")]
pub enum SoulError {
    #[error("Memory error: {0}")]
    Memory(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Voice error: {0}")]
    Voice(String),

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Insufficient RAM: needed {needed_mb} MB, available {available_mb} MB")]
    #[serde(rename_all = "camelCase")]
    InsufficientRam { needed_mb: u64, available_mb: u64 },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<std::io::Error> for SoulError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<serde_json::Error> for SoulError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err.to_string())
    }
}

/// Standard Result alias for Coral&Kai Companion operations.
pub type SoulResult<T> = Result<T, SoulError>;

/// High-level companion emotional state driven by conversation and memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EmotionTag {
    Joy,
    Sadness,
    Shy,
    Surprised,
    Thinking,
    Empathetic,
    #[default]
    Neutral,
}

impl fmt::Display for EmotionTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Joy => "joy",
            Self::Sadness => "sadness",
            Self::Shy => "shy",
            Self::Surprised => "surprised",
            Self::Thinking => "thinking",
            Self::Empathetic => "empathetic",
            Self::Neutral => "neutral",
        };
        f.write_str(s)
    }
}

impl FromStr for EmotionTag {
    type Err = SoulError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "joy" => Ok(Self::Joy),
            "sadness" => Ok(Self::Sadness),
            "shy" => Ok(Self::Shy),
            "surprised" => Ok(Self::Surprised),
            "thinking" => Ok(Self::Thinking),
            "empathetic" => Ok(Self::Empathetic),
            "neutral" => Ok(Self::Neutral),
            other => Err(SoulError::InvalidInput(format!(
                "Unknown emotion tag: {other}"
            ))),
        }
    }
}

/// Instantaneous emotional state emitted to the Live2D avatar and UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmotionState {
    pub tag: EmotionTag,
    pub intensity: f32,
    pub reason: String,
    pub timestamp_ms: i64,
}

impl Default for EmotionState {
    fn default() -> Self {
        Self {
            tag: EmotionTag::Neutral,
            intensity: 0.0,
            reason: String::new(),
            timestamp_ms: 0,
        }
    }
}

/// Lip-sync frame streamed from TTS/audio energy analysis to the Live2D mouth parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VisemeFrame {
    pub mouth_open_y: f32,
    pub mouth_form: f32,
    pub energy: f32,
    pub timestamp_ms: i64,
}

/// Speaker role in a chat conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

impl fmt::Display for ChatRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Assistant => f.write_str("assistant"),
            Self::System => f.write_str("system"),
        }
    }
}

impl FromStr for ChatRole {
    type Err = SoulError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            other => Err(SoulError::InvalidInput(format!(
                "Unknown chat role: {other}"
            ))),
        }
    }
}

/// Single persisted or active conversation turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub id: String,
    pub role: ChatRole,
    pub content: String,
    pub emotion: Option<EmotionTag>,
    pub timestamp_ms: i64,
}

/// Combined brain generation output containing the assistant turn and full emotion state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainReply {
    pub turn: ChatTurn,
    pub emotion: EmotionState,
}

/// Structured atomic memory fact with decay heat and supersession tracking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFact {
    pub id: String,
    pub category: String,
    pub key: String,
    pub value: String,
    pub confidence: f32,
    pub importance: f32,
    pub memory_heat: f32,
    pub access_count: u32,
    pub created_at: i64,
    pub last_accessed_at: i64,
    pub superseded_by: Option<String>,
    pub superseded_at: Option<i64>,
}

/// Compressed episodic memory summarizing a window of raw conversation turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeRecord {
    pub id: String,
    pub summary: String,
    pub raw_turn_count: u32,
    pub emotional_valence: f32,
    pub memory_heat: f32,
    pub created_at: i64,
    pub last_accessed_at: i64,
}

/// Consolidated dream diary reflection spanning a day, week, or milestone period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DreamDiary {
    pub id: String,
    pub period_type: String,
    pub period_key: String,
    pub summary: String,
    pub emotional_tone: String,
    pub bond_delta: f32,
    pub created_at: i64,
}

/// Persistent companion relationship metrics updated over interactions and dream cycles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipState {
    pub trust: f32,
    pub warmth: f32,
    pub familiarity: f32,
    pub mood_baseline: f32,
    pub total_turns: u64,
    pub last_interaction_at: i64,
}

impl Default for RelationshipState {
    fn default() -> Self {
        Self {
            trust: 0.3,
            warmth: 0.4,
            familiarity: 0.1,
            mood_baseline: 0.5,
            total_turns: 0,
            last_interaction_at: 0,
        }
    }
}

/// Context bundle retrieved from SoulMemoryStore for prompt construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRetrievalContext {
    pub facts: Vec<MemoryFact>,
    pub episodes: Vec<EpisodeRecord>,
    pub recent_diaries: Vec<DreamDiary>,
    pub relationship: RelationshipState,
}

/// Category of local model or asset managed by Coral&Kai Companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    Llm,
    Asr,
    Embed,
    Live2d,
}

/// Origin of a discovered model or avatar bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelSource {
    Bundled,
    UserData,
    Custom,
}

/// Memory footprint safety classification relative to 8GB Unified Memory hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RamSafetyTier {
    Safe8Gb,
    Tight8Gb,
    Recommended16GbPlus,
}

/// Descriptor for a local LLM, ASR, embedding, or Live2D model bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub kind: ModelKind,
    pub path: String,
    pub size_bytes: u64,
    pub estimated_ram_mb: u64,
    pub source: ModelSource,
    pub safety_tier: RamSafetyTier,
    pub is_active: bool,
    pub valid: bool,
    pub notes: Option<String>,
}

/// Live system and process RAM telemetry used by the 8GB budget guard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SystemRamStatus {
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub process_ram_mb: u64,
    pub budget_ceiling_mb: u64,
    pub is_8gb_class: bool,
    pub active_llm_id: Option<String>,
    pub active_asr_id: Option<String>,
}

/// Voice input operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum VoiceMode {
    #[default]
    PushToTalk,
    HandsFree,
    Muted,
}

/// Preferred ASR / response language setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LanguagePref {
    #[default]
    Auto,
    Thai,
    English,
}

/// Streaming token delta emitted during LLM generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenChunk {
    pub text: String,
    pub is_final: bool,
    pub full_text: String,
}

/// Partial or final speech-to-text transcription event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEvent {
    pub text: String,
    pub language: String,
    pub is_final: bool,
    pub timestamp_ms: i64,
}

/// Active companion character identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CharacterId {
    #[default]
    Coral,
    Kai,
}

impl CharacterId {
    /// Returns the canonical lowercase identifier string for this character.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Coral => "coral",
            Self::Kai => "kai",
        }
    }
}

impl fmt::Display for CharacterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CharacterId {
    type Err = SoulError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "coral" => Ok(Self::Coral),
            "kai" => Ok(Self::Kai),
            other => Err(SoulError::InvalidInput(format!(
                "Unknown character id: {other}"
            ))),
        }
    }
}

/// Decaying intensity entry for an individual emotion tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmotionDecayEntry {
    pub tag: EmotionTag,
    pub intensity: f32,
    pub baseline: f32,
    pub half_life_minutes: f32,
    pub updated_at_ms: i64,
}

/// Full decaying emotion profile snapshot for a character.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EmotionProfileState {
    pub character_id: CharacterId,
    pub dominant: EmotionTag,
    pub dominant_intensity: f32,
    pub emotions: Vec<EmotionDecayEntry>,
    pub computed_at_ms: i64,
}

/// Persisted companion diary entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiaryEntry {
    pub id: String,
    pub character_id: CharacterId,
    pub entry_date: String,
    pub title: String,
    pub content: String,
    pub mood: EmotionTag,
    pub source: String,
    pub merged_count: u32,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Editable persona biography for a companion character.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PersonaBio {
    pub character_id: CharacterId,
    pub display_name: String,
    pub traits: String,
    pub self_reflection: String,
    pub favorite_topics: String,
    pub updated_at: i64,
}

/// Editable user profile remembered across conversations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub display_name: String,
    pub timezone: String,
    pub location: String,
    pub notes: String,
    pub updated_at: i64,
}

/// High-level logical animation state for the Live2D avatar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AvatarLogicalState {
    #[default]
    Idle,
    Think,
    Write,
    Speak,
    Sleep,
}

impl AvatarLogicalState {
    /// Returns the canonical lowercase name for this avatar logical state.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Think => "think",
            Self::Write => "write",
            Self::Speak => "speak",
            Self::Sleep => "sleep",
        }
    }
}

impl fmt::Display for AvatarLogicalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AvatarLogicalState {
    type Err = SoulError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => Ok(Self::Idle),
            "think" => Ok(Self::Think),
            "write" => Ok(Self::Write),
            "speak" => Ok(Self::Speak),
            "sleep" => Ok(Self::Sleep),
            other => Err(SoulError::InvalidInput(format!(
                "Unknown avatar logical state: {other}"
            ))),
        }
    }
}

/// Full synchronized snapshot of the Live2D avatar state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AvatarSnapshot {
    pub character_id: CharacterId,
    pub logical_state: AvatarLogicalState,
    pub emotion: EmotionState,
    pub emotion_profile: EmotionProfileState,
    pub eyes_closed: bool,
    pub auto_blink_enabled: bool,
    pub gaze_tracking_enabled: bool,
    pub viseme: VisemeFrame,
    pub updated_at_ms: i64,
}

/// Configuration for the autonomous background inner-loop heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InnerLoopConfig {
    pub enabled: bool,
    pub interval_minutes: u32,
    pub proactive_outreach_enabled: bool,
    pub quiet_hours_start: Option<u8>,
    pub quiet_hours_end: Option<u8>,
    pub min_idle_seconds: u64,
}

impl Default for InnerLoopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 20,
            proactive_outreach_enabled: true,
            quiet_hours_start: Some(23),
            quiet_hours_end: Some(7),
            min_idle_seconds: 120,
        }
    }
}

/// Outcome category for an inner-loop beat execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InnerLoopBeatKind {
    MemoryReflection,
    EmotionUpdate,
    ProactiveOutreach,
    SkippedNoModel,
    SkippedBusy,
    SkippedLowRam,
    SkippedDisabled,
}

/// Current runtime status and telemetry of the background inner loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InnerLoopStatus {
    pub config: InnerLoopConfig,
    pub last_beat_at_ms: Option<i64>,
    pub last_beat_kind: Option<InnerLoopBeatKind>,
    pub last_beat_summary: Option<String>,
    pub next_beat_at_ms: Option<i64>,
    pub total_beats: u64,
}

/// Proactive companion message emitted asynchronously from the inner loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProactiveMessageEvent {
    pub character_id: CharacterId,
    pub content: String,
    pub emotion: EmotionState,
    pub reason: String,
    pub timestamp_ms: i64,
}

/// Request payload to invoke a plugin or tool action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionInvocation {
    pub action: String,
    pub params: serde_json::Value,
}

/// Result payload returned by a plugin action execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    pub action: String,
    pub plugin_name: String,
    pub success: bool,
    pub summary: String,
    pub data: Option<serde_json::Value>,
}

/// Metadata describing an installed companion plugin and its actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub builtin: bool,
    pub supported_actions: Vec<String>,
}

/// Configuration for a single external MCP server connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub enabled: bool,
    pub timeout_ms: u64,
}

/// Global MCP integration configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    pub enabled: bool,
    pub config_path: Option<String>,
    pub servers: Vec<McpServerConfig>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            config_path: None,
            servers: Vec::new(),
        }
    }
}

/// Descriptor for a tool exposed by a configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub server_name: String,
    pub tool_name: String,
    pub description: String,
}

/// Streaming callback for generated LLM tokens.
pub type TokenCallback = Box<dyn FnMut(TokenChunk) + Send>;

/// Streaming callback for ASR transcript events.
pub type TranscriptCallback = Box<dyn FnMut(TranscriptEvent) + Send>;

/// Streaming callback for real-time lip-sync viseme frames.
pub type VisemeCallback = Box<dyn FnMut(VisemeFrame) + Send>;

/// Persistent memory, relationship, and dream-consolidation engine contract.
#[async_trait]
pub trait MemoryEngine: Send + Sync {
    async fn append_turn(&self, turn: ChatTurn) -> SoulResult<()>;
    async fn recent_turns(&self, limit: usize) -> SoulResult<Vec<ChatTurn>>;
    async fn retrieve_context(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        fact_limit: usize,
        episode_limit: usize,
    ) -> SoulResult<MemoryRetrievalContext>;
    async fn upsert_fact(&self, fact: MemoryFact, embedding: Option<&[f32]>) -> SoulResult<()>;
    async fn list_facts(&self, limit: usize, include_superseded: bool) -> SoulResult<Vec<MemoryFact>>;
    async fn store_episode(
        &self,
        episode: EpisodeRecord,
        embedding: Option<&[f32]>,
    ) -> SoulResult<()>;
    async fn list_dream_diaries(&self, limit: usize) -> SoulResult<Vec<DreamDiary>>;
    async fn store_dream_diary(&self, diary: DreamDiary) -> SoulResult<()>;
    async fn get_relationship_state(&self) -> SoulResult<RelationshipState>;
    async fn update_relationship_state(&self, state: RelationshipState) -> SoulResult<()>;
    async fn consolidate_now(&self) -> SoulResult<Option<DreamDiary>>;
    async fn export_snapshot(&self, target_path: &Path) -> SoulResult<String>;
    async fn import_snapshot(&self, source_path: &Path) -> SoulResult<usize>;
}

/// Local LLM inference, embedding, and model hot-swap engine contract.
#[async_trait]
pub trait BrainEngine: Send + Sync {
    async fn load_model(&self, model: &ModelInfo) -> SoulResult<()>;
    async fn unload_model(&self) -> SoulResult<()>;
    fn active_model(&self) -> Option<ModelInfo>;
    async fn generate_reply(
        &self,
        user_message: &str,
        history: &[ChatTurn],
        memory_ctx: &MemoryRetrievalContext,
        on_token: TokenCallback,
    ) -> SoulResult<BrainReply>;
    async fn embed_text(&self, text: &str) -> SoulResult<Vec<f32>>;
    fn system_ram_status(&self) -> SystemRamStatus;
}

/// Local ASR, VAD, TTS, and viseme-streaming engine contract.
#[async_trait]
pub trait VoiceEngine: Send + Sync {
    async fn load_asr_model(&self, model: &ModelInfo) -> SoulResult<()>;
    fn active_asr_model(&self) -> Option<ModelInfo>;
    async fn set_voice_mode(&self, mode: VoiceMode) -> SoulResult<()>;
    fn voice_mode(&self) -> VoiceMode;
    async fn set_language_pref(&self, pref: LanguagePref) -> SoulResult<()>;
    fn language_pref(&self) -> LanguagePref;
    async fn start_listening(&self, on_transcript: TranscriptCallback) -> SoulResult<()>;
    async fn stop_listening(&self) -> SoulResult<()>;
    async fn speak(
        &self,
        text: &str,
        emotion: EmotionTag,
        on_viseme: VisemeCallback,
    ) -> SoulResult<()>;
    async fn stop_speaking(&self) -> SoulResult<()>;
}

/// Current UNIX timestamp in milliseconds.
pub fn now_timestamp_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Generate a random UUID v4 string.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emotion_tag_round_trip() {
        let tags = [
            EmotionTag::Joy,
            EmotionTag::Sadness,
            EmotionTag::Shy,
            EmotionTag::Surprised,
            EmotionTag::Thinking,
            EmotionTag::Empathetic,
            EmotionTag::Neutral,
        ];
        for tag in tags {
            let text = tag.to_string();
            let parsed = EmotionTag::from_str(&text).expect("valid emotion tag");
            assert_eq!(tag, parsed);
            let json = serde_json::to_string(&tag).expect("serialize emotion tag");
            assert_eq!(json, format!("\"{text}\""));
        }
    }

    #[test]
    fn camel_case_serialization() {
        let tier = RamSafetyTier::Safe8Gb;
        assert_eq!(serde_json::to_string(&tier).unwrap(), "\"safe8Gb\"");
        let tier3 = RamSafetyTier::Recommended16GbPlus;
        assert_eq!(
            serde_json::to_string(&tier3).unwrap(),
            "\"recommended16GbPlus\""
        );
        let status = SystemRamStatus {
            total_ram_mb: 8192,
            available_ram_mb: 3200,
            process_ram_mb: 450,
            budget_ceiling_mb: 4800,
            is_8gb_class: true,
            active_llm_id: Some("qwen2.5-3b".into()),
            active_asr_id: None,
        };
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(v["totalRamMb"], 8192);
        assert_eq!(v["is8gbClass"], true);
        assert_eq!(v["activeLlmId"], "qwen2.5-3b");

        let err = SoulError::InsufficientRam {
            needed_mb: 5200,
            available_mb: 3100,
        };
        let ev = serde_json::to_value(&err).unwrap();
        assert_eq!(ev["type"], "insufficientRam");
        assert_eq!(ev["detail"]["neededMb"], 5200);
        assert_eq!(ev["detail"]["availableMb"], 3100);
    }

    #[test]
    fn character_id_and_avatar_logical_state_round_trip() {
        assert_eq!(CharacterId::default(), CharacterId::Coral);
        for (id, expected) in [(CharacterId::Coral, "coral"), (CharacterId::Kai, "kai")] {
            assert_eq!(id.as_str(), expected);
            assert_eq!(id.to_string(), expected);
            assert_eq!(
                CharacterId::from_str(&expected.to_uppercase()).unwrap(),
                id
            );
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let back: CharacterId = serde_json::from_str(&json).unwrap();
            assert_eq!(back, id);
        }

        assert_eq!(AvatarLogicalState::default(), AvatarLogicalState::Idle);
        for (state, expected) in [
            (AvatarLogicalState::Idle, "idle"),
            (AvatarLogicalState::Think, "think"),
            (AvatarLogicalState::Write, "write"),
            (AvatarLogicalState::Speak, "speak"),
            (AvatarLogicalState::Sleep, "sleep"),
        ] {
            assert_eq!(state.to_string(), expected);
            assert_eq!(
                AvatarLogicalState::from_str(&format!(" {expected} ")).unwrap(),
                state
            );
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let back: AvatarLogicalState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state);
        }
    }

    #[test]
    fn inner_loop_and_mcp_defaults_round_trip() {
        for (kind, expected) in [
            (InnerLoopBeatKind::MemoryReflection, "memoryReflection"),
            (InnerLoopBeatKind::EmotionUpdate, "emotionUpdate"),
            (InnerLoopBeatKind::ProactiveOutreach, "proactiveOutreach"),
            (InnerLoopBeatKind::SkippedNoModel, "skippedNoModel"),
            (InnerLoopBeatKind::SkippedBusy, "skippedBusy"),
            (InnerLoopBeatKind::SkippedLowRam, "skippedLowRam"),
            (InnerLoopBeatKind::SkippedDisabled, "skippedDisabled"),
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
            let back: InnerLoopBeatKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }

        let inner_cfg = InnerLoopConfig::default();
        assert!(!inner_cfg.enabled);
        assert_eq!(inner_cfg.interval_minutes, 20);
        assert!(inner_cfg.proactive_outreach_enabled);
        assert_eq!(inner_cfg.quiet_hours_start, Some(23));
        assert_eq!(inner_cfg.quiet_hours_end, Some(7));
        assert_eq!(inner_cfg.min_idle_seconds, 120);
        let inner_json = serde_json::to_string(&inner_cfg).unwrap();
        let inner_back: InnerLoopConfig = serde_json::from_str(&inner_json).unwrap();
        assert_eq!(inner_back, inner_cfg);

        let mcp_cfg = McpConfig::default();
        assert!(!mcp_cfg.enabled);
        assert!(mcp_cfg.config_path.is_none());
        assert!(mcp_cfg.servers.is_empty());
        let mcp_json = serde_json::to_string(&mcp_cfg).unwrap();
        let mcp_back: McpConfig = serde_json::from_str(&mcp_json).unwrap();
        assert_eq!(mcp_back, mcp_cfg);
    }

    #[test]
    fn snapshot_diary_and_action_result_round_trip() {
        let snapshot = AvatarSnapshot {
            character_id: CharacterId::Kai,
            logical_state: AvatarLogicalState::Speak,
            emotion: EmotionState {
                tag: EmotionTag::Joy,
                intensity: 0.85,
                reason: "happy chat".into(),
                timestamp_ms: 1_700_000_000_000,
            },
            emotion_profile: EmotionProfileState {
                character_id: CharacterId::Kai,
                dominant: EmotionTag::Joy,
                dominant_intensity: 0.85,
                emotions: vec![EmotionDecayEntry {
                    tag: EmotionTag::Joy,
                    intensity: 0.85,
                    baseline: 0.2,
                    half_life_minutes: 30.0,
                    updated_at_ms: 1_700_000_000_000,
                }],
                computed_at_ms: 1_700_000_000_000,
            },
            eyes_closed: false,
            auto_blink_enabled: true,
            gaze_tracking_enabled: true,
            viseme: VisemeFrame {
                mouth_open_y: 0.5,
                mouth_form: 0.25,
                energy: 0.7,
                timestamp_ms: 1_700_000_000_000,
            },
            updated_at_ms: 1_700_000_000_000,
        };
        let snap_val = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(snap_val["characterId"], "kai");
        assert_eq!(snap_val["logicalState"], "speak");
        assert_eq!(snap_val["autoBlinkEnabled"], true);
        let snap_back: AvatarSnapshot = serde_json::from_value(snap_val).unwrap();
        assert_eq!(snap_back, snapshot);

        let diary = DiaryEntry {
            id: "diary-1".into(),
            character_id: CharacterId::Coral,
            entry_date: "2026-11-24".into(),
            title: "Morning Tea".into(),
            content: "Talked about Rust traits.".into(),
            mood: EmotionTag::Empathetic,
            source: "inner-loop".into(),
            merged_count: 2,
            created_at: 1000,
            updated_at: 2000,
        };
        let diary_val = serde_json::to_value(&diary).unwrap();
        assert_eq!(diary_val["characterId"], "coral");
        assert_eq!(diary_val["entryDate"], "2026-11-24");
        assert_eq!(diary_val["mergedCount"], 2);
        let diary_back: DiaryEntry = serde_json::from_value(diary_val).unwrap();
        assert_eq!(diary_back, diary);

        let action_res = ActionResult {
            action: "weather.today".into(),
            plugin_name: "builtin-weather".into(),
            success: true,
            summary: "Sunny, 24C".into(),
            data: Some(serde_json::json!({ "tempC": 24 })),
        };
        let action_val = serde_json::to_value(&action_res).unwrap();
        assert_eq!(action_val["pluginName"], "builtin-weather");
        assert_eq!(action_val["data"]["tempC"], 24);
        let action_back: ActionResult = serde_json::from_value(action_val).unwrap();
        assert_eq!(action_back, action_res);
    }
}
