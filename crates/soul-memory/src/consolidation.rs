use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use shared_types::{
    ChatRole, ChatTurn, DreamDiary, EmotionTag, EpisodeRecord, RelationshipState, SoulError,
    SoulResult, new_id, now_timestamp_ms,
};

use crate::embedder::{Embedder, embedding_to_le_bytes};
use crate::facts::resolve_embedding;
use crate::ranking::get_relationship_state_query;
use crate::schema::db_err;

/// Persist an `EpisodeRecord` and synchronize its FTS5 + `vec0` indexes.
pub fn store_episode_tx(
    conn: &mut Connection,
    mut episode: EpisodeRecord,
    embedding: Option<&[f32]>,
    embedder: &Embedder,
) -> SoulResult<()> {
    if episode.id.trim().is_empty() {
        episode.id = new_id();
    }
    if episode.summary.trim().is_empty() {
        return Err(SoulError::InvalidInput(
            "Episode summary cannot be empty".into(),
        ));
    }
    if episode.created_at <= 0 {
        episode.created_at = now_timestamp_ms();
    }
    if episode.last_accessed_at <= 0 {
        episode.last_accessed_at = episode.created_at;
    }

    let resolved = resolve_embedding(embedding, &episode.summary, embedder)?;
    let vec_bytes = embedding_to_le_bytes(&resolved);

    let tx = conn.transaction().map_err(db_err)?;
    insert_episode_in_tx(&tx, &episode, &vec_bytes)?;
    tx.commit().map_err(db_err)?;
    Ok(())
}

fn insert_episode_in_tx(
    tx: &rusqlite::Transaction<'_>,
    episode: &EpisodeRecord,
    vec_bytes: &[u8],
) -> SoulResult<()> {
    tx.execute(
        "INSERT INTO episodes (
            id, summary, raw_turn_count, emotional_valence, memory_heat,
            access_count, created_at, last_accessed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            summary = excluded.summary,
            raw_turn_count = excluded.raw_turn_count,
            emotional_valence = excluded.emotional_valence,
            memory_heat = excluded.memory_heat,
            last_accessed_at = excluded.last_accessed_at",
        params![
            &episode.id,
            &episode.summary,
            episode.raw_turn_count,
            episode.emotional_valence.clamp(-1.0, 1.0),
            episode.memory_heat.clamp(0.0, 1.0),
            episode.created_at,
            episode.last_accessed_at,
        ],
    )
    .map_err(db_err)?;

    tx.execute(
        "DELETE FROM episodes_fts WHERE episode_id = ?1",
        params![&episode.id],
    )
    .map_err(db_err)?;
    tx.execute(
        "INSERT INTO episodes_fts (episode_id, summary) VALUES (?1, ?2)",
        params![&episode.id, &episode.summary],
    )
    .map_err(db_err)?;

    tx.execute(
        "DELETE FROM vec_episodes WHERE episode_id = ?1",
        params![&episode.id],
    )
    .map_err(db_err)?;
    tx.execute(
        "INSERT INTO vec_episodes (episode_id, embedding) VALUES (?1, ?2)",
        params![&episode.id, vec_bytes],
    )
    .map_err(db_err)?;

    Ok(())
}

/// Persist a `DreamDiary` entry.
pub fn store_dream_diary_tx(conn: &mut Connection, mut diary: DreamDiary) -> SoulResult<()> {
    if diary.id.trim().is_empty() {
        diary.id = new_id();
    }
    if diary.created_at <= 0 {
        diary.created_at = now_timestamp_ms();
    }

    let tx = conn.transaction().map_err(db_err)?;
    insert_diary_in_tx(&tx, &diary)?;
    tx.commit().map_err(db_err)?;
    Ok(())
}

fn insert_diary_in_tx(tx: &rusqlite::Transaction<'_>, diary: &DreamDiary) -> SoulResult<()> {
    tx.execute(
        "INSERT INTO dream_diaries (
            id, period_type, period_key, summary, emotional_tone, bond_delta, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            period_type = excluded.period_type,
            period_key = excluded.period_key,
            summary = excluded.summary,
            emotional_tone = excluded.emotional_tone,
            bond_delta = excluded.bond_delta,
            created_at = excluded.created_at",
        params![
            &diary.id,
            &diary.period_type,
            &diary.period_key,
            &diary.summary,
            &diary.emotional_tone,
            diary.bond_delta,
            diary.created_at,
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

/// Update `RelationshipState`, clamping affective dimensions cleanly to `0.0..=1.0`.
pub fn update_relationship_state_tx(
    conn: &mut Connection,
    mut state: RelationshipState,
) -> SoulResult<()> {
    state.trust = state.trust.clamp(0.0, 1.0);
    state.warmth = state.warmth.clamp(0.0, 1.0);
    state.familiarity = state.familiarity.clamp(0.0, 1.0);
    state.mood_baseline = state.mood_baseline.clamp(0.0, 1.0);

    conn.execute(
        "INSERT INTO relationship_state (
            id, trust, warmth, familiarity, mood_baseline, total_turns, last_interaction_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            trust = excluded.trust,
            warmth = excluded.warmth,
            familiarity = excluded.familiarity,
            mood_baseline = excluded.mood_baseline,
            total_turns = excluded.total_turns,
            last_interaction_at = excluded.last_interaction_at",
        params![
            state.trust,
            state.warmth,
            state.familiarity,
            state.mood_baseline,
            state.total_turns as i64,
            state.last_interaction_at,
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

/// Consolidate unconsolidated turns into an `EpisodeRecord`, generate a `DreamDiary`,
/// and update `RelationshipState`. Returns `Ok(None)` if no new turns await consolidation.
pub fn consolidate_now_tx(
    conn: &mut Connection,
    embedder: &Embedder,
) -> SoulResult<Option<DreamDiary>> {
    let turns = load_unconsolidated_turns(conn)?;
    if turns.is_empty() {
        return Ok(None);
    }

    let now_ms = now_timestamp_ms();
    let raw_turn_count = turns.len() as u32;
    let total_turns_in_db: u64 = conn
        .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get::<_, i64>(0))
        .map_err(db_err)?
        .max(0) as u64;
    let existing_episode_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .map_err(db_err)?;
    let current_rel = get_relationship_state_query(conn)?;

    let (valence, emotional_tone) = analyze_turns_emotion(&turns);
    let episode_summary = build_episode_summary(&turns);
    let episode_vec = embedder.embed(&episode_summary)?;
    let episode_vec_bytes = embedding_to_le_bytes(&episode_vec);

    let episode = EpisodeRecord {
        id: new_id(),
        summary: episode_summary.clone(),
        raw_turn_count,
        emotional_valence: valence,
        memory_heat: (0.65 + 0.15 * valence.abs()).clamp(0.0, 1.0),
        created_at: now_ms,
        last_accessed_at: now_ms,
    };

    let next_ep_index = existing_episode_count + 1;
    let is_monthly = next_ep_index > 0 && next_ep_index % 30 == 0;
    let period_type = if is_monthly { "monthly" } else { "daily" }.to_string();
    let dt: DateTime<Utc> =
        DateTime::from_timestamp_millis(now_ms).unwrap_or_else(Utc::now);
    let period_key = if is_monthly {
        dt.format("%Y-%m").to_string()
    } else {
        dt.format("%Y-%m-%d").to_string()
    };

    let engagement = (raw_turn_count as f32).min(20.0) / 20.0;
    let bond_delta = (0.015 + 0.035 * engagement + 0.01 * valence.max(0.0)).clamp(0.01, 0.08);

    let diary_summary = format!(
        "Reflected on {} recent turns with a {} tone. {}",
        raw_turn_count,
        emotional_tone,
        truncate_chars(&episode_summary, 280),
    );

    let diary = DreamDiary {
        id: new_id(),
        period_type,
        period_key,
        summary: diary_summary,
        emotional_tone,
        bond_delta,
        created_at: now_ms,
    };

    let last_turn_ts = turns
        .iter()
        .map(|t| t.timestamp_ms)
        .max()
        .unwrap_or(now_ms);
    let next_rel = RelationshipState {
        trust: (current_rel.trust + bond_delta * 0.6).clamp(0.0, 1.0),
        warmth: (current_rel.warmth + bond_delta * 0.8 + 0.01 * valence.max(0.0)).clamp(0.0, 1.0),
        familiarity: (current_rel.familiarity + bond_delta).clamp(0.0, 1.0),
        mood_baseline: (current_rel.mood_baseline + 0.05 * valence).clamp(0.0, 1.0),
        total_turns: total_turns_in_db.max(current_rel.total_turns),
        last_interaction_at: last_turn_ts.max(current_rel.last_interaction_at),
    };

    let tx = conn.transaction().map_err(db_err)?;
    insert_episode_in_tx(&tx, &episode, &episode_vec_bytes)?;
    insert_diary_in_tx(&tx, &diary)?;

    tx.execute(
        "UPDATE relationship_state
         SET trust = ?1,
             warmth = ?2,
             familiarity = ?3,
             mood_baseline = ?4,
             total_turns = ?5,
             last_interaction_at = ?6
         WHERE id = 1",
        params![
            next_rel.trust,
            next_rel.warmth,
            next_rel.familiarity,
            next_rel.mood_baseline,
            next_rel.total_turns as i64,
            next_rel.last_interaction_at,
        ],
    )
    .map_err(db_err)?;

    for turn in &turns {
        tx.execute(
            "UPDATE turns SET consolidated = 1 WHERE id = ?1",
            params![&turn.id],
        )
        .map_err(db_err)?;
    }

    tx.commit().map_err(db_err)?;
    Ok(Some(diary))
}

fn load_unconsolidated_turns(conn: &Connection) -> SoulResult<Vec<ChatTurn>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, role, content, emotion, timestamp_ms
             FROM turns
             WHERE consolidated = 0
             ORDER BY timestamp_ms ASC, rowid ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| {
            let role_str: String = row.get(1)?;
            let emotion_str: Option<String> = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                role_str,
                row.get::<_, String>(2)?,
                emotion_str,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(db_err)?;

    let mut out = Vec::new();
    for item in rows {
        let (id, role_str, content, emotion_str, timestamp_ms) = item.map_err(db_err)?;
        let role = role_str.parse::<ChatRole>()?;
        let emotion = match emotion_str {
            Some(s) if !s.trim().is_empty() => Some(s.parse::<EmotionTag>()?),
            _ => None,
        };
        out.push(ChatTurn {
            id,
            role,
            content,
            emotion,
            timestamp_ms,
        });
    }
    Ok(out)
}

fn analyze_turns_emotion(turns: &[ChatTurn]) -> (f32, String) {
    let mut counts: HashMap<EmotionTag, usize> = HashMap::new();
    let mut valence_sum = 0.0_f32;
    let mut tagged_count = 0_usize;

    for turn in turns {
        if let Some(tag) = turn.emotion {
            *counts.entry(tag).or_insert(0) += 1;
            valence_sum += emotion_to_valence(tag);
            tagged_count += 1;
        }
    }

    let avg_valence = if tagged_count > 0 {
        (valence_sum / tagged_count as f32).clamp(-1.0, 1.0)
    } else {
        0.15
    };

    let dominant = counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(tag, _)| tag)
        .unwrap_or(EmotionTag::Neutral);

    let tone = match dominant {
        EmotionTag::Joy => "joyful",
        EmotionTag::Empathetic => "empathetic",
        EmotionTag::Shy => "affectionate",
        EmotionTag::Sadness => "tender",
        EmotionTag::Thinking => "reflective",
        EmotionTag::Surprised => "lively",
        EmotionTag::Neutral => {
            if avg_valence >= 0.25 {
                "warm"
            } else {
                "calm"
            }
        }
    };

    (avg_valence, tone.to_string())
}

fn emotion_to_valence(tag: EmotionTag) -> f32 {
    match tag {
        EmotionTag::Joy => 0.8,
        EmotionTag::Empathetic => 0.6,
        EmotionTag::Shy => 0.5,
        EmotionTag::Surprised => 0.25,
        EmotionTag::Thinking => 0.1,
        EmotionTag::Neutral => 0.05,
        EmotionTag::Sadness => -0.5,
    }
}

fn build_episode_summary(turns: &[ChatTurn]) -> String {
    let mut snippets = Vec::new();
    for turn in turns.iter().take(8) {
        let speaker = match turn.role {
            ChatRole::User => "User",
            ChatRole::Assistant => "Mina",
            ChatRole::System => "System",
        };
        let compact: String = turn
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let short = truncate_chars(&compact, 72);
        if !short.is_empty() {
            snippets.push(format!("{speaker}: {short}"));
        }
    }
    let joined = snippets.join(" | ");
    if turns.len() > 8 {
        format!(
            "Conversation ({} turns): {} ...",
            turns.len(),
            truncate_chars(&joined, 420)
        )
    } else {
        format!("Conversation ({} turns): {}", turns.len(), joined)
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        text.to_string()
    } else if max_chars > 3 {
        let mut s: String = text.chars().take(max_chars - 3).collect();
        s.push_str("...");
        s
    } else {
        text.chars().take(max_chars).collect()
    }
}
