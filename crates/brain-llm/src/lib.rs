//! Local offline LLM manager, Metal hot-swapper, RAM guard, and streaming emotion parser.

pub mod backend;
pub mod embed;
pub mod manager;
pub mod parser;
pub mod prompt;
pub mod ram_guard;
pub mod scanner;

pub use backend::{LlamaCppMetalBackend, LoadedModelInstance, ModelBackend};
pub use embed::{LOCAL_EMBEDDING_DIM, compute_local_embedding};
pub use manager::{LlmBrainManager, LoadedLlm};
pub use parser::{EmotionCallback, EmotionStreamParser, parse_emotion_tag_inner, split_sentences};
pub use prompt::{
    ResolvedLanguage, build_chat_messages, build_soul_system_prompt, format_chatml_prompt,
    resolve_reply_language,
};
pub use ram_guard::{
    AdaptiveInferenceParams, DEFAULT_8GB_CEILING_MB, EIGHT_GB_CLASS_MAX_MB, LLM_8GB_CEILING_MB,
    RamProbe, RamSnapshot, SIXTEEN_GB_CLASS_MIN_MB, SysinfoRamProbe, adaptive_params_for_ram,
    classify_ram_safety_tier, estimate_gguf_ram_mb,
};
pub use scanner::{GGUF_MAGIC, register_custom_gguf, scan_llm_dirs, validate_gguf_magic};

#[cfg(test)]
mod tests;
