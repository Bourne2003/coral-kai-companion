use std::collections::HashMap;

use rusqlite::{Connection, params};
use shared_types::{
    CharacterId, EmotionDecayEntry, EmotionProfileState, EmotionTag, SoulError, SoulResult,
    now_timestamp_ms,
};

use crate::schema::{DEFAULT_EMOTION_SPECS, db_err, seed_default_emotions};

/// Canonical emotion display & tie-break order.
const CANONICAL_EMOTION_ORDER: &[EmotionTag] = &[
    EmotionTag::Joy,
    EmotionTag::Empathetic,
    EmotionTag::Shy,
    EmotionTag::Surprised,
    EmotionTag::Thinking,
    EmotionTag::Sadness,
    EmotionTag::Neutral,
];

/// Compute exponentially decayed emotion intensity toward `baseline`:
/// `decayed = baseline + (intensity - baseline) * 2^(-elapsed_minutes / half_life_minutes)`,
/// clamped to `0.0..=1.0`.
pub fn compute_decayed_intensity(
    intensity: f32,
    baseline: f32,
    half_life_minutes: f32,
    updated_at_ms: i64,
    now_ms: i64,
) -> f32 {
    if now_ms <= updated_at_ms {
        return intensity.clamp(0.0, 1.0);
    }
    if half_life_minutes <= 0.0 || !half_life_minutes.is_finite() {
        return baseline.clamp(0.0, 1.0);
    }
    let elapsed_minutes = (now_ms - updated_at_ms) as f32 / 60_000.0;
    let decayed = baseline + (intensity - baseline) * 2.0_f32.powf(-elapsed_minutes / half_life_minutes);
    decayed.clamp(0.0, 1.0)
}

/// Read the current emotion profile decayed to `now_ms`, persisting updated intensities in SQLite.
pub fn get_emotion_profile_at_tx(
    conn: &mut Connection,
    character_id: CharacterId,
    now_ms: i64,
) -> SoulResult<EmotionProfileState> {
    apply_emotion_update_at(conn, character_id, now_ms, None)
}

/// Read the current emotion profile decayed to `now_timestamp_ms()`.
pub fn get_emotion_profile_tx(
    conn: &mut Connection,
    character_id: CharacterId,
) -> SoulResult<EmotionProfileState> {
    get_emotion_profile_at_tx(conn, character_id, now_timestamp_ms())
}

/// Decay all emotions to `now_ms`, apply an impulse to `tag`, persist to SQLite, and return the new profile.
pub fn record_emotion_impulse_at_tx(
    conn: &mut Connection,
    character_id: CharacterId,
    tag: EmotionTag,
    intensity: f32,
    now_ms: i64,
) -> SoulResult<EmotionProfileState> {
    if !intensity.is_finite() {
        return Err(SoulError::InvalidInput(
            "Emotion intensity must be a finite number".into(),
        ));
    }
    apply_emotion_update_at(conn, character_id, now_ms, Some((tag, intensity.clamp(0.0, 1.0))))
}

/// Decay all emotions to `now_timestamp_ms()`, apply an impulse to `tag`, and persist to SQLite.
pub fn record_emotion_impulse_tx(
    conn: &mut Connection,
    character_id: CharacterId,
    tag: EmotionTag,
    intensity: f32,
) -> SoulResult<EmotionProfileState> {
    record_emotion_impulse_at_tx(conn, character_id, tag, intensity, now_timestamp_ms())
}

fn apply_emotion_update_at(
    conn: &mut Connection,
    character_id: CharacterId,
    now_ms: i64,
    impulse: Option<(EmotionTag, f32)>,
) -> SoulResult<EmotionProfileState> {
    let tx = conn.transaction().map_err(db_err)?;
    seed_default_emotions(&tx, now_ms)?;

    let mut rows_by_tag: HashMap<EmotionTag, EmotionDecayEntry> = {
        let mut stmt = tx
            .prepare(
                "SELECT tag, intensity, baseline, half_life_minutes, updated_at_ms
                 FROM emotion_states",
            )
            .map_err(db_err)?;
        let mapped = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f32>(1)?,
                    row.get::<_, f32>(2)?,
                    row.get::<_, f32>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(db_err)?;

        let mut map = HashMap::new();
        for item in mapped {
            let (tag_str, intensity, baseline, half_life_minutes, updated_at_ms) =
                item.map_err(db_err)?;
            let tag = tag_str.parse::<EmotionTag>()?;
            map.insert(
                tag,
                EmotionDecayEntry {
                    tag,
                    intensity,
                    baseline,
                    half_life_minutes,
                    updated_at_ms,
                },
            );
        }
        map
    };

    let mut updated_entries = Vec::with_capacity(CANONICAL_EMOTION_ORDER.len());

    for &tag in CANONICAL_EMOTION_ORDER {
        let entry = rows_by_tag.remove(&tag).unwrap_or_else(|| {
            let (_, baseline, half_life_minutes) = DEFAULT_EMOTION_SPECS
                .iter()
                .find(|(t, _, _)| *t == tag)
                .copied()
                .unwrap_or((tag, 0.1, 20.0));
            EmotionDecayEntry {
                tag,
                intensity: baseline,
                baseline,
                half_life_minutes,
                updated_at_ms: now_ms,
            }
        });

        let mut next_intensity = compute_decayed_intensity(
            entry.intensity,
            entry.baseline,
            entry.half_life_minutes,
            entry.updated_at_ms,
            now_ms,
        );
        let mut next_updated_at = entry.updated_at_ms.max(now_ms);

        if let Some((impulse_tag, impulse_intensity)) = impulse {
            if impulse_tag == tag {
                next_intensity = impulse_intensity.clamp(0.0, 1.0);
                next_updated_at = now_ms;
            }
        }

        tx.execute(
            "UPDATE emotion_states
             SET intensity = ?1,
                 updated_at_ms = ?2
             WHERE tag = ?3",
            params![next_intensity, next_updated_at, tag.to_string()],
        )
        .map_err(db_err)?;

        updated_entries.push(EmotionDecayEntry {
            tag,
            intensity: next_intensity,
            baseline: entry.baseline,
            half_life_minutes: entry.half_life_minutes,
            updated_at_ms: next_updated_at,
        });
    }

    tx.commit().map_err(db_err)?;

    let (dominant, dominant_intensity) = select_dominant(&updated_entries);

    Ok(EmotionProfileState {
        character_id,
        dominant,
        dominant_intensity,
        emotions: updated_entries,
        computed_at_ms: now_ms,
    })
}

fn select_dominant(entries: &[EmotionDecayEntry]) -> (EmotionTag, f32) {
    let mut best_tag = EmotionTag::Neutral;
    let mut best_intensity = -1.0_f32;
    for entry in entries {
        if entry.intensity > best_intensity {
            best_intensity = entry.intensity;
            best_tag = entry.tag;
        }
    }
    if best_intensity < 0.0 {
        (EmotionTag::Neutral, 0.0)
    } else {
        (best_tag, best_intensity)
    }
}
