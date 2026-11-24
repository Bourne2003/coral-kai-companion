use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use shared_types::{
    CharacterId, DiaryEntry, EmotionTag, PersonaBio, SoulError, SoulResult, UserProfile, new_id,
    now_timestamp_ms,
};

use crate::schema::{db_err, default_persona_bio};

/// Add a new diary entry or merge into an existing same-day `(character_id, entry_date, source)` entry,
/// syncing `diary_entries_fts` inside an existing SQLite transaction.
#[allow(clippy::too_many_arguments)]
pub fn add_or_merge_diary_entry_in_tx(
    tx: &Transaction<'_>,
    character_id: CharacterId,
    title: &str,
    content: &str,
    mood: EmotionTag,
    source: &str,
    entry_date_opt: Option<&str>,
    now_ms: i64,
) -> SoulResult<DiaryEntry> {
    let trimmed_content = content.trim();
    if trimmed_content.is_empty() {
        return Err(SoulError::InvalidInput(
            "Diary entry content cannot be empty".into(),
        ));
    }

    let trimmed_title = title.trim();
    let resolved_title = if trimmed_title.is_empty() {
        "Daily Reflection"
    } else {
        trimmed_title
    };

    let trimmed_source = source.trim();
    let resolved_source = if trimmed_source.is_empty() {
        "manual"
    } else {
        trimmed_source
    };

    let resolved_date = match entry_date_opt.map(str::trim).filter(|s| !s.is_empty()) {
        Some(date) => date.to_string(),
        None => Utc::now().format("%Y-%m-%d").to_string(),
    };

    let char_str = character_id.as_str();

    let existing: Option<(String, String, String, u32, i64)> = tx
        .query_row(
            "SELECT id, title, content, merged_count, created_at
             FROM diary_entries
             WHERE character_id = ?1 AND entry_date = ?2 AND source = ?3
             ORDER BY updated_at DESC, rowid DESC
             LIMIT 1",
            params![char_str, &resolved_date, resolved_source],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(db_err)?;

    let entry = if let Some((id, existing_title, existing_content, merged_count, created_at)) =
        existing
    {
        let next_title = if existing_title.trim().is_empty() {
            resolved_title.to_string()
        } else {
            existing_title
        };
        let next_content = if existing_content.trim().is_empty() {
            trimmed_content.to_string()
        } else {
            format!("{}\n\n{}", existing_content.trim_end(), trimmed_content)
        };
        let next_merged_count = merged_count.saturating_add(1);
        let next_updated_at = now_ms.max(created_at);

        tx.execute(
            "UPDATE diary_entries
             SET title = ?1,
                 content = ?2,
                 mood = ?3,
                 merged_count = ?4,
                 updated_at = ?5
             WHERE id = ?6",
            params![
                &next_title,
                &next_content,
                mood.to_string(),
                next_merged_count,
                next_updated_at,
                &id,
            ],
        )
        .map_err(db_err)?;

        sync_diary_fts(tx, &id, &next_title, &next_content)?;

        DiaryEntry {
            id,
            character_id,
            entry_date: resolved_date,
            title: next_title,
            content: next_content,
            mood,
            source: resolved_source.to_string(),
            merged_count: next_merged_count,
            created_at,
            updated_at: next_updated_at,
        }
    } else {
        let id = new_id();
        let entry = DiaryEntry {
            id: id.clone(),
            character_id,
            entry_date: resolved_date,
            title: resolved_title.to_string(),
            content: trimmed_content.to_string(),
            mood,
            source: resolved_source.to_string(),
            merged_count: 1,
            created_at: now_ms,
            updated_at: now_ms,
        };

        tx.execute(
            "INSERT INTO diary_entries (
                id, character_id, entry_date, title, content, mood, source,
                merged_count, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &entry.id,
                char_str,
                &entry.entry_date,
                &entry.title,
                &entry.content,
                entry.mood.to_string(),
                &entry.source,
                entry.merged_count,
                entry.created_at,
                entry.updated_at,
            ],
        )
        .map_err(db_err)?;

        sync_diary_fts(tx, &entry.id, &entry.title, &entry.content)?;
        entry
    };

    Ok(entry)
}

/// Add or merge a diary entry in its own transaction.
pub fn add_or_merge_diary_entry_tx(
    conn: &mut Connection,
    character_id: CharacterId,
    title: &str,
    content: &str,
    mood: EmotionTag,
    source: &str,
    entry_date_opt: Option<&str>,
) -> SoulResult<DiaryEntry> {
    let now_ms = now_timestamp_ms();
    let tx = conn.transaction().map_err(db_err)?;
    let entry = add_or_merge_diary_entry_in_tx(
        &tx,
        character_id,
        title,
        content,
        mood,
        source,
        entry_date_opt,
        now_ms,
    )?;
    tx.commit().map_err(db_err)?;
    Ok(entry)
}

fn sync_diary_fts(
    tx: &Transaction<'_>,
    diary_id: &str,
    title: &str,
    content: &str,
) -> SoulResult<()> {
    tx.execute(
        "DELETE FROM diary_entries_fts WHERE diary_id = ?1",
        params![diary_id],
    )
    .map_err(db_err)?;
    tx.execute(
        "INSERT INTO diary_entries_fts (diary_id, title, content) VALUES (?1, ?2, ?3)",
        params![diary_id, title, content],
    )
    .map_err(db_err)?;
    Ok(())
}

/// List persisted diary entries for `character_id`, ordered most-recent first.
pub fn list_diary_entries_query(
    conn: &Connection,
    character_id: CharacterId,
    limit: usize,
) -> SoulResult<Vec<DiaryEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare(
            "SELECT id, character_id, entry_date, title, content, mood, source,
                    merged_count, created_at, updated_at
             FROM diary_entries
             WHERE character_id = ?1
             ORDER BY entry_date DESC, updated_at DESC, rowid DESC
             LIMIT ?2",
        )
        .map_err(db_err)?;

    let rows = stmt
        .query_map(params![character_id.as_str(), limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, u32>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })
        .map_err(db_err)?;

    let mut out = Vec::new();
    for item in rows {
        let (
            id,
            char_str,
            entry_date,
            title,
            content,
            mood_str,
            source,
            merged_count,
            created_at,
            updated_at,
        ) = item.map_err(db_err)?;
        let parsed_char = char_str.parse::<CharacterId>()?;
        let mood = mood_str.parse::<EmotionTag>()?;
        out.push(DiaryEntry {
            id,
            character_id: parsed_char,
            entry_date,
            title,
            content,
            mood,
            source,
            merged_count,
            created_at,
            updated_at,
        });
    }
    Ok(out)
}

/// Retrieve the persisted `PersonaBio` (or default seed for `character_id` if absent).
pub fn get_persona_bio_query(
    conn: &Connection,
    character_id: CharacterId,
) -> SoulResult<PersonaBio> {
    let row: Option<(String, String, String, String, String, i64)> = conn
        .query_row(
            "SELECT character_id, display_name, traits, self_reflection, favorite_topics, updated_at
             FROM persona_bio
             WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(db_err)?;

    match row {
        Some((char_str, display_name, traits, self_reflection, favorite_topics, updated_at)) => {
            let parsed_char = char_str.parse::<CharacterId>().unwrap_or(character_id);
            Ok(PersonaBio {
                character_id: parsed_char,
                display_name,
                traits,
                self_reflection,
                favorite_topics,
                updated_at,
            })
        }
        None => Ok(default_persona_bio(character_id, now_timestamp_ms())),
    }
}

/// Update and persist the companion `PersonaBio` for `character_id`.
pub fn update_persona_bio_tx(
    conn: &mut Connection,
    character_id: CharacterId,
    mut bio: PersonaBio,
) -> SoulResult<PersonaBio> {
    bio.character_id = character_id;
    bio.display_name = bio.display_name.trim().to_string();
    bio.traits = bio.traits.trim().to_string();
    bio.self_reflection = bio.self_reflection.trim().to_string();
    bio.favorite_topics = bio.favorite_topics.trim().to_string();
    bio.updated_at = now_timestamp_ms();

    conn.execute(
        "INSERT INTO persona_bio (
            id, character_id, display_name, traits, self_reflection, favorite_topics, updated_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            character_id = excluded.character_id,
            display_name = excluded.display_name,
            traits = excluded.traits,
            self_reflection = excluded.self_reflection,
            favorite_topics = excluded.favorite_topics,
            updated_at = excluded.updated_at",
        params![
            bio.character_id.as_str(),
            &bio.display_name,
            &bio.traits,
            &bio.self_reflection,
            &bio.favorite_topics,
            bio.updated_at,
        ],
    )
    .map_err(db_err)?;

    Ok(bio)
}

/// Retrieve the persisted `UserProfile` (or default if absent).
pub fn get_user_profile_query(conn: &Connection) -> SoulResult<UserProfile> {
    let row: Option<UserProfile> = conn
        .query_row(
            "SELECT display_name, timezone, location, notes, updated_at
             FROM user_profile
             WHERE id = 1",
            [],
            |r| {
                Ok(UserProfile {
                    display_name: r.get(0)?,
                    timezone: r.get(1)?,
                    location: r.get(2)?,
                    notes: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            },
        )
        .optional()
        .map_err(db_err)?;

    Ok(row.unwrap_or_else(|| UserProfile {
        updated_at: now_timestamp_ms(),
        ..UserProfile::default()
    }))
}

/// Update and persist the `UserProfile`.
pub fn update_user_profile_tx(
    conn: &mut Connection,
    mut profile: UserProfile,
) -> SoulResult<UserProfile> {
    profile.display_name = profile.display_name.trim().to_string();
    profile.timezone = profile.timezone.trim().to_string();
    profile.location = profile.location.trim().to_string();
    profile.notes = profile.notes.trim().to_string();
    profile.updated_at = now_timestamp_ms();

    conn.execute(
        "INSERT INTO user_profile (
            id, display_name, timezone, location, notes, updated_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET
            display_name = excluded.display_name,
            timezone = excluded.timezone,
            location = excluded.location,
            notes = excluded.notes,
            updated_at = excluded.updated_at",
        params![
            &profile.display_name,
            &profile.timezone,
            &profile.location,
            &profile.notes,
            profile.updated_at,
        ],
    )
    .map_err(db_err)?;

    Ok(profile)
}
