use std::sync::Once;

use rusqlite::Connection;
use shared_types::{RelationshipState, SoulError, SoulResult};

static REGISTER_SQLITE_VEC: Once = Once::new();

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

/// Core table names required to validate a SoulMate2D SQLite snapshot.
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

/// Configure connection PRAGMAs and ensure all SoulMate2D tables, indexes, FTS5, and vec0 tables exist.
pub fn initialize_database(conn: &Connection, is_file_backed: bool) -> SoulResult<()> {
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

    let def = RelationshipState::default();
    conn.execute(
        "INSERT OR IGNORE INTO relationship_state (
            id, trust, warmth, familiarity, mood_baseline, total_turns, last_interaction_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            def.trust,
            def.warmth,
            def.familiarity,
            def.mood_baseline,
            def.total_turns as i64,
            def.last_interaction_at,
        ],
    )
    .map_err(db_err)?;

    Ok(())
}

/// Convert a `rusqlite::Error` into `SoulError::Memory`.
pub fn db_err(err: rusqlite::Error) -> SoulError {
    SoulError::Memory(format!("SQLite error: {err}"))
}
