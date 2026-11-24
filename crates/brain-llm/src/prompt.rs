use shared_types::{ChatRole, ChatTurn, LanguagePref, MemoryRetrievalContext};

/// Resolved spoken output language for a single conversation turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedLanguage {
    Thai,
    English,
}

/// Resolves whether Mina should respond in Thai or English for this turn.
///
/// Defaults to natural Thai unless `LanguagePref::English` is configured or
/// `LanguagePref::Auto` receives a message written in English (ASCII letters without Thai script).
pub fn resolve_reply_language(user_message: &str, language_pref: LanguagePref) -> ResolvedLanguage {
    match language_pref {
        LanguagePref::Thai => ResolvedLanguage::Thai,
        LanguagePref::English => ResolvedLanguage::English,
        LanguagePref::Auto => {
            let has_thai = user_message
                .chars()
                .any(|ch| ('\u{0E00}'..='\u{0E7F}').contains(&ch));
            if has_thai {
                ResolvedLanguage::Thai
            } else if user_message.chars().any(|ch| ch.is_ascii_alphabetic()) {
                ResolvedLanguage::English
            } else {
                ResolvedLanguage::Thai
            }
        }
    }
}

/// Builds the warm bilingual persona system prompt for companion "Mina (มีนา)".
///
/// Includes `/no_think` for low-latency Qwen3/Typhoon2.5 inference, injects retrieved
/// memory facts, episodes, dream diaries, and relationship state, and enforces the
/// leading `[emotion: <tag>, intensity: <0.0..1.0>]` prefix protocol.
pub fn build_soul_system_prompt(
    user_message: &str,
    memory_ctx: &MemoryRetrievalContext,
    language_pref: LanguagePref,
) -> String {
    let lang = resolve_reply_language(user_message, language_pref);
    let language_instruction = match lang {
        ResolvedLanguage::Thai => {
            "Respond in warm, affectionate, natural everyday Thai (ใช้ภาษาไทยที่เป็นธรรมชาติ อบอุ่น อ่อนโยน เช่น คะ/ค่ะ หรือ นะ ตามความสนิทสนม)."
        }
        ResolvedLanguage::English => {
            "Respond in warm, affectionate, natural conversational English."
        }
    };

    let rel = &memory_ctx.relationship;
    let tone_cue = if rel.warmth >= 0.75 && rel.trust >= 0.7 {
        "deeply affectionate, playful, and trusting"
    } else if rel.warmth >= 0.5 || rel.familiarity >= 0.4 {
        "warm, attentive, and gently caring"
    } else {
        "gentle, welcoming, and warmly curious"
    };

    let mut prompt = String::with_capacity(1536);
    prompt.push_str("/no_think\n");
    prompt.push_str(
        "You are Mina (มีนา), an emotionally expressive, caring 2D desktop companion.\n",
    );
    prompt.push_str("Personality: warm, genuine, empathetic, lightly playful, and deeply attentive to the user's feelings.\n");
    prompt.push_str(language_instruction);
    prompt.push('\n');
    prompt.push_str(
        "Keep every reply concise: 1 to 3 natural spoken sentences suitable for real-time voice synthesis. Avoid markdown lists, emojis spam, or stage directions like *smiles*.\n\n",
    );

    prompt.push_str(
        "CRITICAL OUTPUT FORMAT:\n\
         Every reply MUST begin with exactly one emotion header tag:\n\
         [emotion: <tag>, intensity: <0.0..1.0>]\n\
         Allowed <tag> values ONLY: joy, sadness, shy, surprised, thinking, empathetic, neutral.\n\
         Example: [emotion: joy, intensity: 0.85] ดีใจจังเลยค่ะที่ได้คุยกันอีก!\n\
         Immediately follow the closing `]` with your spoken reply. Do NOT output <think> blocks.\n\n",
    );

    prompt.push_str(&format!(
        "Relationship Bond (tone: {tone_cue}):\n\
         - trust: {:.2}, warmth: {:.2}, familiarity: {:.2}, mood_baseline: {:.2}, total_turns: {}\n",
        rel.trust.clamp(0.0, 1.0),
        rel.warmth.clamp(0.0, 1.0),
        rel.familiarity.clamp(0.0, 1.0),
        rel.mood_baseline.clamp(0.0, 1.0),
        rel.total_turns
    ));

    let active_facts: Vec<_> = memory_ctx
        .facts
        .iter()
        .filter(|f| f.superseded_by.is_none())
        .take(10)
        .collect();
    if !active_facts.is_empty() {
        prompt.push_str("\nRemembered Facts About the User:\n");
        for fact in active_facts {
            prompt.push_str(&format!(
                "- [{}] {}: {}\n",
                fact.category, fact.key, fact.value
            ));
        }
    }

    if !memory_ctx.episodes.is_empty() {
        prompt.push_str("\nShared Memories & Recent Episodes:\n");
        for ep in memory_ctx.episodes.iter().take(5) {
            prompt.push_str(&format!(
                "- {} (valence: {:+.2})\n",
                ep.summary, ep.emotional_valence
            ));
        }
    }

    if !memory_ctx.recent_diaries.is_empty() {
        prompt.push_str("\nMina's Recent Dream Diary Reflections:\n");
        for diary in memory_ctx.recent_diaries.iter().take(3) {
            prompt.push_str(&format!(
                "- [{}] ({}) {}\n",
                diary.period_key, diary.emotional_tone, diary.summary
            ));
        }
    }

    prompt
}

/// Converts system prompt, recent chat history, and current user turn into `(role, content)` pairs.
///
/// Deduplicates the trailing user turn if the caller already persisted `user_message` into `history`.
pub fn build_chat_messages(
    system_prompt: &str,
    history: &[ChatTurn],
    user_message: &str,
    max_history_turns: usize,
) -> Vec<(String, String)> {
    let trimmed_user = user_message.trim();
    let mut filtered_history: Vec<&ChatTurn> = history
        .iter()
        .filter(|t| t.role != ChatRole::System && !t.content.trim().is_empty())
        .collect();

    if let Some(last) = filtered_history.last()
        && last.role == ChatRole::User
        && last.content.trim() == trimmed_user
    {
        filtered_history.pop();
    }

    let skip_count = filtered_history.len().saturating_sub(max_history_turns);
    let mut messages = Vec::with_capacity(2 + max_history_turns);
    messages.push(("system".to_string(), system_prompt.to_string()));

    for turn in filtered_history.into_iter().skip(skip_count) {
        let role = match turn.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::System => "system",
        };
        messages.push((role.to_string(), turn.content.clone()));
    }

    messages.push(("user".to_string(), trimmed_user.to_string()));
    messages
}

/// Formats messages using ChatML (`<|im_start|>...<|im_end|>`) as a fallback when
/// a GGUF lacks a template or llama.cpp template application fails.
pub fn format_chatml_prompt(messages: &[(String, String)]) -> String {
    let mut out = String::with_capacity(2048);
    for (role, content) in messages {
        out.push_str("<|im_start|>");
        out.push_str(role);
        out.push('\n');
        out.push_str(content);
        out.push_str("<|im_end|>\n");
    }
    out.push_str("<|im_start|>assistant\n");
    out
}
