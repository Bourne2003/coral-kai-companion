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
    pub const VOICE_TRANSCRIPT: &str = "voice://transcript";
    pub const SYSTEM_RAM: &str = "system://ram";
}

/// Unified error enum across SoulMate2D crates and Tauri IPC boundaries.
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

/// Standard Result alias for SoulMate2D operations.
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

/// Category of local model or asset managed by SoulMate2D.
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
}
