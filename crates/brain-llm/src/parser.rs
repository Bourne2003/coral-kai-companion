use std::str::FromStr;

use shared_types::{EmotionState, EmotionTag, TokenCallback, TokenChunk, now_timestamp_ms};

/// Callback invoked immediately once the companion's leading emotion tag is resolved.
pub type EmotionCallback = Box<dyn FnMut(EmotionState) + Send>;

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";
const MAX_PREFIX_BUFFER_BYTES: usize = 160;
const DEFAULT_INTENSITY: f32 = 0.65;
const FALLBACK_INTENSITY: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkState {
    Outside,
    Inside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefixState {
    Detecting,
    Streaming,
}

/// Stateful streaming parser for LLM output.
///
/// 1. Strips `<think>...</think>` blocks cleanly even when tags span split token boundaries.
/// 2. Buffers only the leading `[emotion: <tag>, intensity: <0.0..1.0>]` prefix, resolves
///    [`EmotionState`], and invokes `on_emotion` IMMEDIATELY before any spoken text streams.
/// 3. Emits clean [`TokenChunk`]s to `on_token` with tag/think/stop markers removed.
pub struct EmotionStreamParser {
    on_emotion: EmotionCallback,
    on_token: TokenCallback,
    think_state: ThinkState,
    think_buf: String,
    prefix_state: PrefixState,
    prefix_buf: String,
    pending_leading_trim: bool,
    emitted_emotion: Option<EmotionState>,
    full_text: String,
}

impl EmotionStreamParser {
    pub fn new(on_emotion: EmotionCallback, on_token: TokenCallback) -> Self {
        Self {
            on_emotion,
            on_token,
            think_state: ThinkState::Outside,
            think_buf: String::new(),
            prefix_state: PrefixState::Detecting,
            prefix_buf: String::new(),
            pending_leading_trim: true,
            emitted_emotion: None,
            full_text: String::new(),
        }
    }

    /// Feeds the next decoded UTF-8 piece from the LLM token stream.
    pub fn push(&mut self, piece: &str) {
        if piece.is_empty() {
            return;
        }
        self.think_buf.push_str(piece);
        self.drain_think_filter(false);
    }

    /// Finalizes the stream, flushing any buffered text and ensuring both
    /// `on_emotion` and a final `TokenChunk { is_final: true }` are emitted.
    pub fn finish(mut self) -> (EmotionState, String) {
        self.drain_think_filter(true);

        if self.prefix_state == PrefixState::Detecting {
            self.resolve_unclosed_prefix_at_eof();
        }

        let clean_final = self.full_text.trim_end().to_string();
        self.full_text = clean_final.clone();

        (self.on_token)(TokenChunk {
            text: String::new(),
            is_final: true,
            full_text: clean_final.clone(),
        });

        let emotion = self.emitted_emotion.unwrap_or_else(|| EmotionState {
            tag: EmotionTag::Neutral,
            intensity: FALLBACK_INTENSITY,
            reason: "fallback_no_emotion".to_string(),
            timestamp_ms: now_timestamp_ms(),
        });

        (emotion, clean_final)
    }

    fn drain_think_filter(&mut self, at_eof: bool) {
        loop {
            match self.think_state {
                ThinkState::Outside => {
                    if let Some(idx) = find_ascii_case_insensitive(&self.think_buf, THINK_OPEN) {
                        let before = self.think_buf[..idx].to_string();
                        self.think_buf.drain(..idx + THINK_OPEN.len());
                        self.think_state = ThinkState::Inside;
                        if !before.is_empty() {
                            self.push_after_think(&before);
                        }
                        continue;
                    }

                    if at_eof {
                        let remaining = std::mem::take(&mut self.think_buf);
                        if !remaining.is_empty() {
                            self.push_after_think(&remaining);
                        }
                        break;
                    }

                    let keep_tail = longest_partial_prefix_suffix(&self.think_buf, THINK_OPEN);
                    let emit_len = self.think_buf.len() - keep_tail;
                    if emit_len > 0 {
                        let ready = self.think_buf[..emit_len].to_string();
                        self.think_buf.drain(..emit_len);
                        self.push_after_think(&ready);
                    }
                    break;
                }
                ThinkState::Inside => {
                    if let Some(idx) = find_ascii_case_insensitive(&self.think_buf, THINK_CLOSE) {
                        self.think_buf.drain(..idx + THINK_CLOSE.len());
                        self.think_state = ThinkState::Outside;
                        continue;
                    }

                    if at_eof {
                        // Discard unclosed <think> tail rather than leaking reasoning to the user.
                        self.think_buf.clear();
                        break;
                    }

                    let keep_tail = longest_partial_prefix_suffix(&self.think_buf, THINK_CLOSE);
                    let drop_len = self.think_buf.len() - keep_tail;
                    if drop_len > 0 {
                        self.think_buf.drain(..drop_len);
                    }
                    break;
                }
            }
        }
    }

    fn push_after_think(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.prefix_state {
            PrefixState::Detecting => {
                self.prefix_buf.push_str(text);
                self.try_resolve_prefix();
            }
            PrefixState::Streaming => {
                self.emit_body_chunk(text);
            }
        }
    }

    fn try_resolve_prefix(&mut self) {
        let trimmed_start_offset = self.prefix_buf.len() - self.prefix_buf.trim_start().len();
        let trimmed = &self.prefix_buf[trimmed_start_offset..];
        if trimmed.is_empty() {
            return;
        }

        if !trimmed.starts_with('[') {
            let body = trimmed.to_string();
            self.prefix_buf.clear();
            self.emit_emotion_once(EmotionState {
                tag: EmotionTag::Neutral,
                intensity: FALLBACK_INTENSITY,
                reason: "no_leading_emotion_tag".to_string(),
                timestamp_ms: now_timestamp_ms(),
            });
            self.prefix_state = PrefixState::Streaming;
            self.pending_leading_trim = false;
            self.emit_body_chunk(&body);
            return;
        }

        // Verify `[` could still be `[emotion: ...]` while streaming split tokens.
        let after_bracket = trimmed[1..].trim_start();
        let lower_after = after_bracket.to_ascii_lowercase();
        let could_be_emotion_header = "emotion:".starts_with(&lower_after)
            || lower_after.starts_with("emotion:")
            || lower_after.starts_with("emotion");

        if !could_be_emotion_header {
            let body = trimmed.to_string();
            self.prefix_buf.clear();
            self.emit_emotion_once(EmotionState {
                tag: EmotionTag::Neutral,
                intensity: FALLBACK_INTENSITY,
                reason: "non_emotion_bracket_prefix".to_string(),
                timestamp_ms: now_timestamp_ms(),
            });
            self.prefix_state = PrefixState::Streaming;
            self.pending_leading_trim = false;
            self.emit_body_chunk(&body);
            return;
        }

        if let Some(close_idx) = trimmed.find(']') {
            let inner = &trimmed[1..close_idx];
            let rest = trimmed[close_idx + 1..].to_string();
            let parsed = parse_emotion_tag_inner(inner);
            self.emit_emotion_once(parsed);
            self.prefix_state = PrefixState::Streaming;
            self.prefix_buf.clear();
            self.pending_leading_trim = true;
            if !rest.is_empty() {
                self.emit_body_chunk(&rest);
            }
            return;
        }

        if self.prefix_buf.len() >= MAX_PREFIX_BUFFER_BYTES {
            let buffered = std::mem::take(&mut self.prefix_buf);
            let (emotion, body) = recover_malformed_prefix(&buffered);
            self.emit_emotion_once(emotion);
            self.prefix_state = PrefixState::Streaming;
            self.pending_leading_trim = true;
            if !body.is_empty() {
                self.emit_body_chunk(&body);
            }
        }
    }

    fn resolve_unclosed_prefix_at_eof(&mut self) {
        let buffered = std::mem::take(&mut self.prefix_buf);
        let trimmed = buffered.trim_start();
        if trimmed.is_empty() {
            self.emit_emotion_once(EmotionState {
                tag: EmotionTag::Neutral,
                intensity: FALLBACK_INTENSITY,
                reason: "empty_output_fallback".to_string(),
                timestamp_ms: now_timestamp_ms(),
            });
            self.prefix_state = PrefixState::Streaming;
            return;
        }

        let (emotion, body) = recover_malformed_prefix(trimmed);
        self.emit_emotion_once(emotion);
        self.prefix_state = PrefixState::Streaming;
        if !body.is_empty() {
            self.pending_leading_trim = false;
            self.emit_body_chunk(&body);
        }
    }

    fn emit_emotion_once(&mut self, emotion: EmotionState) {
        if self.emitted_emotion.is_none() {
            self.emitted_emotion = Some(emotion.clone());
            (self.on_emotion)(emotion);
        }
    }

    fn emit_body_chunk(&mut self, raw_piece: &str) {
        let cleaned = strip_chat_stop_markers(raw_piece);
        let slice = if self.pending_leading_trim {
            let trimmed = cleaned.trim_start();
            if trimmed.is_empty() {
                return;
            }
            self.pending_leading_trim = false;
            trimmed
        } else {
            cleaned.as_str()
        };

        if slice.is_empty() {
            return;
        }

        self.full_text.push_str(slice);
        (self.on_token)(TokenChunk {
            text: slice.to_string(),
            is_final: false,
            full_text: self.full_text.clone(),
        });
    }
}

/// Parses the contents inside `[emotion: <tag>, intensity: <float>]`.
pub fn parse_emotion_tag_inner(inner: &str) -> EmotionState {
    let mut tag = EmotionTag::Neutral;
    let mut intensity = DEFAULT_INTENSITY;
    let mut recognized_tag = false;

    for part in inner.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((key, val)) = part.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let val = val.trim();
            match key.as_str() {
                "emotion" => {
                    if let Ok(parsed) = EmotionTag::from_str(val) {
                        tag = parsed;
                        recognized_tag = true;
                    }
                }
                "intensity" => {
                    if let Ok(num) = val.parse::<f32>()
                        && num.is_finite()
                    {
                        intensity = num.clamp(0.0, 1.0);
                    }
                }
                _ => {}
            }
        }
    }

    let reason = if recognized_tag {
        format!("parsed:{tag}")
    } else {
        "malformed_emotion_fallback".to_string()
    };

    EmotionState {
        tag,
        intensity: intensity.clamp(0.0, 1.0),
        reason,
        timestamp_ms: now_timestamp_ms(),
    }
}

/// Splits completed companion text into clean sentences for downstream TTS/caption alignment.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        current.push(ch);
        if matches!(ch, '.' | '!' | '?' | '\n') {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                sentences.push(trimmed.to_string());
            }
            current.clear();
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }
    sentences
}

fn recover_malformed_prefix(trimmed: &str) -> (EmotionState, String) {
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("[emotion:") || lower.starts_with("[ emotion:") {
        let (tag_region, rest) = match trimmed.split_once('\n') {
            Some((first_line, tail)) => (first_line, tail.trim_start().to_string()),
            None => (trimmed, String::new()),
        };
        let inner = tag_region.trim_start_matches('[').trim_end_matches(']');
        let mut emotion = parse_emotion_tag_inner(inner);
        emotion.reason = format!("unclosed_tag_fallback:{}", emotion.tag);
        (emotion, rest)
    } else {
        (
            EmotionState {
                tag: EmotionTag::Neutral,
                intensity: FALLBACK_INTENSITY,
                reason: "malformed_prefix_fallback".to_string(),
                timestamp_ms: now_timestamp_ms(),
            },
            trimmed.to_string(),
        )
    }
}

fn strip_chat_stop_markers(input: &str) -> String {
    let mut out = input.to_string();
    for marker in ["<|im_end|>", "<|endoftext|>", "<|eot_id|>", "</s>"] {
        if out.contains(marker) {
            out = out.replace(marker, "");
        }
    }
    out
}

/// Finds an ASCII needle case-insensitively while returning a valid UTF-8 byte index.
fn find_ascii_case_insensitive(haystack: &str, needle_lower: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&idx| {
        haystack.is_char_boundary(idx)
            && h[idx..idx + n.len()]
                .iter()
                .zip(n.iter())
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// Returns the byte length of the longest suffix of `buf` that matches a prefix of `needle_lower`.
fn longest_partial_prefix_suffix(buf: &str, needle_lower: &str) -> usize {
    let b = buf.as_bytes();
    let n = needle_lower.as_bytes();
    let max_k = (n.len().saturating_sub(1)).min(b.len());
    for k in (1..=max_k).rev() {
        let start = b.len() - k;
        if !buf.is_char_boundary(start) {
            continue;
        }
        if b[start..]
            .iter()
            .zip(&n[..k])
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
        {
            return k;
        }
    }
    0
}
