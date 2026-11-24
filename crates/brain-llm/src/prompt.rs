use shared_types::{
    ChatRole, ChatTurn, DiaryEntry, EmotionProfileState, LanguagePref, MemoryRetrievalContext,
    PersonaBio, UserProfile,
};

/// Dynamic inner-life state injected into the companion's enriched system prompt.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InnerLifePromptContext {
    pub persona_bio: Option<PersonaBio>,
    pub user_profile: Option<UserProfile>,
    pub emotion_profile: Option<EmotionProfileState>,
    pub recent_diary: Option<DiaryEntry>,
    pub current_time_summary: Option<String>,
    pub enabled_actions: Vec<String>,
}

/// Active companion character persona controlling name, tone, and Thai polite particles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharacterPersona {
    #[default]
    Coral,
    Kai,
}

impl CharacterPersona {
    /// Parse a character persona identifier (`"coral"` or `"kai"`, case-insensitive).
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "coral" | "coral_companion" => Some(Self::Coral),
            "kai" | "kai_companion" => Some(Self::Kai),
            _ => None,
        }
    }

    /// Canonical lowercase identifier for this character persona.
    pub fn as_id(&self) -> &'static str {
        match self {
            Self::Coral => "coral",
            Self::Kai => "kai",
        }
    }
}

/// Resolved spoken output language for a single conversation turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedLanguage {
    Thai,
    English,
}

/// Resolves whether the companion should respond in Thai or English for this turn.
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

/// Builds the warm bilingual persona system prompt for the default companion (`Coral`).
///
/// Kept for backward compatibility; delegates to [`build_soul_system_prompt_for_persona`].
pub fn build_soul_system_prompt(
    user_message: &str,
    memory_ctx: &MemoryRetrievalContext,
    language_pref: LanguagePref,
) -> String {
    build_soul_system_prompt_for_persona(
        user_message,
        memory_ctx,
        language_pref,
        CharacterPersona::default(),
    )
}

/// Builds the warm bilingual persona system prompt for a specific [`CharacterPersona`].
///
/// Includes `/no_think` for low-latency Qwen3/Typhoon2.5 inference, injects retrieved
/// memory facts, episodes, dream diaries, and relationship state, and enforces the
/// leading `[emotion: <tag>, intensity: <0.0..1.0>]` prefix protocol.
pub fn build_soul_system_prompt_for_persona(
    user_message: &str,
    memory_ctx: &MemoryRetrievalContext,
    language_pref: LanguagePref,
    persona: CharacterPersona,
) -> String {
    let lang = resolve_reply_language(user_message, language_pref);
    let (identity_line, personality_line, thai_instruction, example_line, diary_heading) =
        match persona {
            CharacterPersona::Coral => (
                "You are Coral (คอรัล), an emotionally expressive, caring 2D desktop companion.\n",
                "Personality: warm, gentle, genuine, empathetic, lightly playful, and deeply attentive to the user's feelings.\n",
                "Respond in warm, affectionate, natural everyday Thai using soft feminine polite particles (ใช้ภาษาไทยที่เป็นธรรมชาติ อบอุ่น อ่อนโยน เช่น ค่ะ / คะ / นะคะ ตามความสนิทสนม).",
                "Example: [emotion: joy, intensity: 0.85] ดีใจจังเลยค่ะที่ได้คุยกันอีกนะคะ!\n",
                "\nCoral's Recent Dream Diary Reflections:\n",
            ),
            CharacterPersona::Kai => (
                "You are Kai (ไค), an emotionally expressive, caring 2D desktop companion.\n",
                "Personality: warm, friendly, steady, empathetic, lightly playful, and deeply attentive to the user's feelings.\n",
                "Respond in warm, friendly, natural everyday Thai using warm masculine polite particles (ใช้ภาษาไทยที่เป็นธรรมชาติ อบอุ่น เป็นกันเอง เช่น ครับ / นะครับ ตามความสนิทสนม).",
                "Example: [emotion: joy, intensity: 0.85] ดีใจจังเลยครับที่ได้คุยกันอีกนะครับ!\n",
                "\nKai's Recent Dream Diary Reflections:\n",
            ),
        };

    let language_instruction = match lang {
        ResolvedLanguage::Thai => thai_instruction,
        ResolvedLanguage::English => "Respond in warm, affectionate, natural conversational English.",
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
    prompt.push_str(identity_line);
    prompt.push_str(personality_line);
    prompt.push_str(language_instruction);
    prompt.push('\n');
    prompt.push_str(
        "Keep every reply concise: 1 to 3 natural spoken sentences suitable for real-time voice synthesis. Avoid markdown lists, emojis spam, or stage directions like *smiles*.\n\n",
    );

    prompt.push_str(
        "CRITICAL OUTPUT FORMAT:\n\
         Every reply MUST begin with exactly one emotion header tag:\n\
         [emotion: <tag>, intensity: <0.0..1.0>]\n\
         Allowed <tag> values ONLY: joy, sadness, shy, surprised, thinking, empathetic, neutral.\n",
    );
    prompt.push_str(example_line);
    prompt.push_str(
        "Immediately follow the closing `]` with your spoken reply. Do NOT output <think> blocks.\n\n",
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
        prompt.push_str(diary_heading);
        for diary in memory_ctx.recent_diaries.iter().take(3) {
            prompt.push_str(&format!(
                "- [{}] ({}) {}\n",
                diary.period_key, diary.emotional_tone, diary.summary
            ));
        }
    }

    prompt
}

/// Builds an enriched bilingual companion system prompt by extending
/// [`build_soul_system_prompt_for_persona`] with active self-bio, user profile,
/// decayed emotion baseline, latest personal diary reflection, wall-clock time,
/// and optional modular action instructions.
pub fn build_soul_system_prompt_enriched(
    user_message: &str,
    memory_ctx: &MemoryRetrievalContext,
    language_pref: LanguagePref,
    persona: CharacterPersona,
    inner_life: &InnerLifePromptContext,
) -> String {
    let mut prompt =
        build_soul_system_prompt_for_persona(user_message, memory_ctx, language_pref, persona);

    if let Some(ref bio) = inner_life.persona_bio {
        let mut lines = Vec::new();
        if !bio.traits.trim().is_empty() {
            lines.push(format!("- Traits: {}", bio.traits.trim()));
        }
        if !bio.self_reflection.trim().is_empty() {
            lines.push(format!("- Self-Reflection: {}", bio.self_reflection.trim()));
        }
        if !bio.favorite_topics.trim().is_empty() {
            lines.push(format!("- Favorite Topics: {}", bio.favorite_topics.trim()));
        }
        if !lines.is_empty() {
            let label = if bio.display_name.trim().is_empty() {
                persona.as_id().to_string()
            } else {
                bio.display_name.trim().to_string()
            };
            prompt.push_str(&format!("\nSelf-Bio ({label}):\n"));
            for line in lines {
                prompt.push_str(&line);
                prompt.push('\n');
            }
        }
    }

    if let Some(ref profile) = inner_life.user_profile {
        let mut lines = Vec::new();
        if !profile.display_name.trim().is_empty() {
            lines.push(format!("- Name: {}", profile.display_name.trim()));
        }
        if !profile.timezone.trim().is_empty() {
            lines.push(format!("- Timezone: {}", profile.timezone.trim()));
        }
        if !profile.location.trim().is_empty() {
            lines.push(format!("- Location: {}", profile.location.trim()));
        }
        if !profile.notes.trim().is_empty() {
            lines.push(format!("- Notes: {}", profile.notes.trim()));
        }
        if !lines.is_empty() {
            prompt.push_str("\nUser Profile:\n");
            for line in lines {
                prompt.push_str(&line);
                prompt.push('\n');
            }
        }
    }

    if let Some(ref emo) = inner_life.emotion_profile {
        prompt.push_str(&format!(
            "\nCurrent Decayed Emotion Baseline: dominant={} ({:.2})\n",
            emo.dominant,
            emo.dominant_intensity.clamp(0.0, 1.0)
        ));
        let active: Vec<String> = emo
            .emotions
            .iter()
            .filter(|e| e.intensity > 0.05)
            .map(|e| format!("{}: {:.2}", e.tag, e.intensity.clamp(0.0, 1.0)))
            .collect();
        if !active.is_empty() {
            prompt.push_str("- Active emotional tones: ");
            prompt.push_str(&active.join(", "));
            prompt.push('\n');
        }
    }

    if let Some(ref diary) = inner_life.recent_diary {
        prompt.push_str("\nLatest Personal Diary Reflection:\n");
        prompt.push_str(&format!(
            "- [{}] {} (mood: {}): {}\n",
            diary.entry_date,
            diary.title.trim(),
            diary.mood,
            diary.content.trim()
        ));
    }

    if let Some(ref time_summary) = inner_life.current_time_summary
        && !time_summary.trim().is_empty()
    {
        prompt.push_str(&format!(
            "\nCurrent Local Time: {}\n",
            time_summary.trim()
        ));
    }

    if !inner_life.enabled_actions.is_empty() {
        prompt.push_str(
            "\nOptional Companion Actions (emit ONLY when genuinely helpful; omit for ordinary chat):\n",
        );
        prompt.push_str(&format!(
            "Available actions: {}\n",
            inner_life.enabled_actions.join(", ")
        ));
        prompt.push_str(
            "Syntax: [action: <action_name>, {\"key\": \"value\"}]\n\
             Never mention raw [action: ...] tag syntax in your spoken reply.\n",
        );
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
