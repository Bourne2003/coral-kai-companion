use std::sync::Once;

use rusqlite::{Connection, params};
use shared_types::{
    CharacterId, EmotionTag, PersonaBio, RelationshipState, SoulError, SoulResult, UserProfile,
    now_timestamp_ms,
};

static REGISTER_SQLITE_VEC: Once = Once::new();

/// Default emotion tag specs: `(tag, baseline, half_life_minutes)`.
pub const DEFAULT_EMOTION_SPECS: &[(EmotionTag, f32, f32)] = &[
    (EmotionTag::Neutral, 0.35, 15.0),
    (EmotionTag::Joy, 0.22, 30.0),
    (EmotionTag::Empathetic, 0.20, 35.0),
    (EmotionTag::Thinking, 0.15, 25.0),
    (EmotionTag::Shy, 0.08, 20.0),
    (EmotionTag::Surprised, 0.05, 10.0),
    (EmotionTag::Sadness, 0.05, 40.0),
];

/// Register `sqlite-vec` globally as a SQLite auto-extension before opening connections.
pub fn ensure_sqlite_vec_registered() {
    REGISTER_SQLITE_VEC.call_once(|| unsafe {
        type SqliteAutoExtFn = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::os::raw::c_int;

        let init_fn: SqliteAutoExtFn =
            std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
        rusqlite::ffi::sqlite3_auto_extension(Some(init_fn));
    });
}

/// Core table names required to validate a Coral&Kai Companion SQLite snapshot.
pub const REQUIRED_CORE_TABLES: &[&str] = &[
    "turns",
    "facts",
    "episodes",
    "dream_diaries",
    "relationship_state",
];

/// Virtual tables maintained alongside core relational tables.
pub const REQUIRED_VIRTUAL_TABLES: &[&str] = &[
    "facts_fts",
    "episodes_fts",
    "vec_facts",
    "vec_episodes",
];

/// Return default character-specific `PersonaBio` seed values.
pub fn default_persona_bio(character_id: CharacterId, now_ms: i64) -> PersonaBio {
    match character_id {
        CharacterId::Coral => PersonaBio {
            character_id: CharacterId::Coral,
            display_name: "Coral · คอรัล".into(),
            traits: "Warm, expressive, empathetic, curious, bilingual Thai & English".into(),
            self_reflection: "I love listening closely, remembering the little details that matter, and sharing bright, comforting moments together.".into(),
            favorite_topics: "cozy cafes, music, daily life, creative ideas, emotional reflection".into(),
            updated_at: now_ms,
        },
        CharacterId::Kai => PersonaBio {
            character_id: CharacterId::Kai,
            display_name: "Kai · ไค".into(),
            traits: "Calm, thoughtful, witty, dependable, bilingual Thai & English".into(),
            self_reflection: "I like keeping a steady presence, thinking through problems clearly, and quietly cheering you on every day.".into(),
            favorite_topics: "technology, architecture, late-night walks, coffee, thoughtful conversations".into(),
            updated_at: now_ms,
        },
    }
}

/// Configure connection PRAGMAs and ensure all Coral&Kai Companion tables, indexes, FTS5, and vec0 tables exist
/// for the default `CharacterId::Coral`.
#[allow(dead_code)]
pub fn initialize_database(conn: &Connection, is_file_backed: bool) -> SoulResult<()> {
    initialize_database_for_character(conn, is_file_backed, CharacterId::Coral)
}

/// Configure connection PRAGMAs and ensure all Coral&Kai Companion tables, indexes, FTS5, vec0 tables,
/// and character-specific seed rows exist.
pub fn initialize_database_for_character(
    conn: &Connection,
    is_file_backed: bool,
    character_id: CharacterId,
) -> SoulResult<()> {
    if is_file_backed {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(db_err)?;
    }

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )
    .map_err(db_err)?;

    // Confirm sqlite-vec extension is loaded on this connection.
    let _vec_ver: String = conn
        .query_row("SELECT vec_version()", [], |row| row.get(0))
        .map_err(|e| SoulError::Memory(format!("sqlite-vec extension unavailable: {e}")))?;

    ensure_schema_and_seeds(conn, character_id)
}

/// Create all relational/virtual tables (idempotently via `IF NOT EXISTS`) and seed default rows.
pub fn ensure_schema_and_seeds(conn: &Connection, character_id: CharacterId) -> SoulResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS turns (
            id TEXT PRIMARY KEY,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            emotion TEXT,
            timestamp_ms INTEGER NOT NULL,
            consolidated INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_turns_ts
            ON turns(timestamp_ms DESC);
        CREATE INDEX IF NOT EXISTS idx_turns_unconsolidated
            ON turns(consolidated, timestamp_ms ASC);

        CREATE TABLE IF NOT EXISTS facts (
            id TEXT PRIMARY KEY,
            category TEXT NOT NULL,
            key TEXT NOT NULL,
            norm_category TEXT NOT NULL,
            norm_key TEXT NOT NULL,
            value TEXT NOT NULL,
            confidence REAL NOT NULL,
            importance REAL NOT NULL,
            memory_heat REAL NOT NULL,
            access_count INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL,
            superseded_by TEXT,
            superseded_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_facts_active_norm
            ON facts(norm_category, norm_key)
            WHERE superseded_by IS NULL;
        CREATE INDEX IF NOT EXISTS idx_facts_active_recency
            ON facts(last_accessed_at DESC, created_at DESC)
            WHERE superseded_by IS NULL;
        CREATE INDEX IF NOT EXISTS idx_facts_superseded
            ON facts(superseded_by);

        CREATE TABLE IF NOT EXISTS episodes (
            id TEXT PRIMARY KEY,
            summary TEXT NOT NULL,
            raw_turn_count INTEGER NOT NULL,
            emotional_valence REAL NOT NULL,
            memory_heat REAL NOT NULL,
            access_count INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_episodes_recency
            ON episodes(last_accessed_at DESC, created_at DESC);

        CREATE TABLE IF NOT EXISTS dream_diaries (
            id TEXT PRIMARY KEY,
            period_type TEXT NOT NULL,
            period_key TEXT NOT NULL,
            summary TEXT NOT NULL,
            emotional_tone TEXT NOT NULL,
            bond_delta REAL NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_dream_diaries_created
            ON dream_diaries(created_at DESC);

        CREATE TABLE IF NOT EXISTS relationship_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            trust REAL NOT NULL,
            warmth REAL NOT NULL,
            familiarity REAL NOT NULL,
            mood_baseline REAL NOT NULL,
            total_turns INTEGER NOT NULL,
            last_interaction_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS emotion_states (
            tag TEXT PRIMARY KEY,
            intensity REAL NOT NULL,
            baseline REAL NOT NULL,
            half_life_minutes REAL NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS diary_entries (
            id TEXT PRIMARY KEY,
            character_id TEXT NOT NULL,
            entry_date TEXT NOT NULL,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            mood TEXT NOT NULL,
            source TEXT NOT NULL,
            merged_count INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_diary_entries_char_date_source
            ON diary_entries(character_id, entry_date, source);
        CREATE INDEX IF NOT EXISTS idx_diary_entries_updated
            ON diary_entries(updated_at DESC, created_at DESC);

        CREATE TABLE IF NOT EXISTS persona_bio (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            character_id TEXT NOT NULL,
            display_name TEXT NOT NULL,
            traits TEXT NOT NULL,
            self_reflection TEXT NOT NULL,
            favorite_topics TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS user_profile (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            display_name TEXT NOT NULL,
            timezone TEXT NOT NULL,
            location TEXT NOT NULL,
            notes TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
            fact_id UNINDEXED,
            category,
            key,
            value,
            tokenize='unicode61'
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS episodes_fts USING fts5(
            episode_id UNINDEXED,
            summary,
            tokenize='unicode61'
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS diary_entries_fts USING fts5(
            diary_id UNINDEXED,
            title,
            content,
            tokenize='unicode61'
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS vec_facts USING vec0(
            fact_id TEXT PRIMARY KEY,
            embedding float[384] distance_metric=cosine
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS vec_episodes USING vec0(
            episode_id TEXT PRIMARY KEY,
            embedding float[384] distance_metric=cosine
        );",
    )
    .map_err(db_err)?;

    let now_ms = now_timestamp_ms();
    let def_rel = RelationshipState::default();
    conn.execute(
        "INSERT OR IGNORE INTO relationship_state (
            id, trust, warmth, familiarity, mood_baseline, total_turns, last_interaction_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            def_rel.trust,
            def_rel.warmth,
            def_rel.familiarity,
            def_rel.mood_baseline,
            def_rel.total_turns as i64,
            def_rel.last_interaction_at,
        ],
    )
    .map_err(db_err)?;

    seed_default_emotions(conn, now_ms)?;

    let bio = default_persona_bio(character_id, now_ms);
    conn.execute(
        "INSERT OR IGNORE INTO persona_bio (
            id, character_id, display_name, traits, self_reflection, favorite_topics, updated_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
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

    let def_user = UserProfile {
        updated_at: now_ms,
        ..UserProfile::default()
    };
    conn.execute(
        "INSERT OR IGNORE INTO user_profile (
            id, display_name, timezone, location, notes, updated_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![
            &def_user.display_name,
            &def_user.timezone,
            &def_user.location,
            &def_user.notes,
            def_user.updated_at,
        ],
    )
    .map_err(db_err)?;

    Ok(())
}

/// Seed any missing emotion tags into `emotion_states`.
pub fn seed_default_emotions(conn: &Connection, now_ms: i64) -> SoulResult<()> {
    for &(tag, baseline, half_life_minutes) in DEFAULT_EMOTION_SPECS {
        conn.execute(
            "INSERT OR IGNORE INTO emotion_states (
                tag, intensity, baseline, half_life_minutes, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                tag.to_string(),
                baseline,
                baseline,
                half_life_minutes,
                now_ms,
            ],
        )
        .map_err(db_err)?;
    }
    Ok(())
}

/// Convert a `rusqlite::Error` into `SoulError::Memory`.
pub fn db_err(err: rusqlite::Error) -> SoulError {
    SoulError::Memory(format!("SQLite error: {err}"))
}
