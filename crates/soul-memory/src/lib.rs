mod consolidation;
mod diary;
mod embedder;
mod emotion_decay;
mod facts;
mod ranking;
mod schema;
mod snapshot;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use rusqlite::{Connection, params};
use shared_types::{
    CharacterId, ChatRole, ChatTurn, DiaryEntry, DreamDiary, EmotionProfileState, EmotionTag,
    EpisodeRecord, MemoryEngine, MemoryFact, MemoryRetrievalContext, PersonaBio, RelationshipState,
    SoulError, SoulResult, UserProfile, new_id, now_timestamp_ms,
};

pub use embedder::{EMBEDDING_DIM, Embedder, char_ngram_embed, validate_onnx_embed_file};
pub use facts::{extract_facts_from_text, extract_facts_from_turn, normalize_fact_field};

use consolidation::{
    compact_redundant_facts_tx, consolidate_now_tx, store_dream_diary_tx, store_episode_tx,
    update_relationship_state_tx,
};
use diary::{
    add_or_merge_diary_entry_tx, get_persona_bio_query, get_user_profile_query,
    list_diary_entries_query, update_persona_bio_tx, update_user_profile_tx,
};
use emotion_decay::{
    get_emotion_profile_at_tx, get_emotion_profile_tx, record_emotion_impulse_at_tx,
    record_emotion_impulse_tx,
};
use facts::{list_facts_query, upsert_fact_tx};
use ranking::{get_relationship_state_query, list_dream_diaries_query, retrieve_context_sync};
use schema::{db_err, ensure_sqlite_vec_registered, initialize_database_for_character};
use snapshot::{export_snapshot_sync, import_snapshot_sync};

/// Persistent single-file SQLite + `sqlite-vec` + FTS5 memory store for Coral&Kai Companion.
#[derive(Debug)]
pub struct SoulMemoryStore {
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<RwLock<Embedder>>,
    db_path: Option<PathBuf>,
    character_id: CharacterId,
}

impl Default for SoulMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SoulMemoryStore {
    /// Create a default in-memory `SoulMemoryStore` using the deterministic char-ngram fallback embedder.
    pub fn new() -> Self {
        Self::in_memory(None).expect("in-memory SoulMemoryStore initialization should never fail")
    }

    /// Open (or create) a portable single-file SQLite memory database at `db_path` for `CharacterId::Coral`.
    ///
    /// When `embed_dir` is `Some(dir)` and contains `model_quantized.onnx` + `tokenizer.json`
    /// (with an available ONNX Runtime dynamic library), local ONNX embeddings are used;
    /// otherwise a deterministic 384-dim char-ngram unit-vector fallback is used.
    pub fn open(db_path: &Path, embed_dir: Option<&Path>) -> SoulResult<Self> {
        Self::open_for_character(db_path, embed_dir, CharacterId::Coral)
    }

    /// Open (or create) a portable single-file SQLite memory database at `db_path` for `character_id`.
    pub fn open_for_character(
        db_path: &Path,
        embed_dir: Option<&Path>,
        character_id: CharacterId,
    ) -> SoulResult<Self> {
        ensure_sqlite_vec_registered();

        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(db_path).map_err(db_err)?;
        initialize_database_for_character(&conn, true, character_id)?;
        let embedder = Embedder::from_dir(embed_dir)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder: Arc::new(RwLock::new(embedder)),
            db_path: Some(db_path.to_path_buf()),
            character_id,
        })
    }

    /// Open an isolated in-memory SQLite memory store for `CharacterId::Coral`.
    pub fn in_memory(embed_dir: Option<&Path>) -> SoulResult<Self> {
        Self::in_memory_for_character(embed_dir, CharacterId::Coral)
    }

    /// Open an isolated in-memory SQLite memory store for `character_id` (useful for unit tests and ephemeral sessions).
    pub fn in_memory_for_character(
        embed_dir: Option<&Path>,
        character_id: CharacterId,
    ) -> SoulResult<Self> {
        ensure_sqlite_vec_registered();

        let conn = Connection::open_in_memory().map_err(db_err)?;
        initialize_database_for_character(&conn, false, character_id)?;
        let embedder = Embedder::from_dir(embed_dir)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            embedder: Arc::new(RwLock::new(embedder)),
            db_path: None,
            character_id,
        })
    }

    /// Return the companion character identity bound to this memory store.
    pub fn character_id(&self) -> CharacterId {
        self.character_id
    }

    /// Return the configured database path if file-backed, or `None` if in-memory.
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Return `"onnx"` when backed by a loaded ONNX model, or `"ngram-fallback"` otherwise.
    pub fn embedder_kind(&self) -> &'static str {
        self.embedder
            .read()
            .map(|guard| guard.kind())
            .unwrap_or("ngram-fallback")
    }

    /// Return the currently loaded ONNX embedding model path, or `None` in char-ngram fallback mode.
    pub fn active_embed_model_path(&self) -> Option<PathBuf> {
        self.embedder
            .read()
            .ok()
            .and_then(|guard| guard.model_path().map(Path::to_path_buf))
    }

    /// Validate and hot-load a user-imported `.onnx` embedding model (requiring sibling `tokenizer.json`),
    /// replacing the active embedder atomically only after the ONNX session succeeds.
    pub fn reload_embedder_from_file(&self, onnx_path: &Path) -> SoulResult<()> {
        let next = Embedder::from_onnx_file(onnx_path)?;
        let mut guard = self
            .embedder
            .write()
            .map_err(|_| SoulError::Internal("SoulMemoryStore embedder lock poisoned".into()))?;
        *guard = next;
        Ok(())
    }

    /// Compute a 384-dim L2-normalized embedding vector using the active embedder.
    pub fn embed_text(&self, text: &str) -> SoulResult<Vec<f32>> {
        let embedder = self.read_embedder()?;
        embedder.embed(text)
    }

    /// Extract personal facts (name, city/location, job, likes/dislikes in Thai or English)
    /// from a `ChatTurn` and upsert them with automatic contradiction superseding.
    pub async fn extract_and_upsert_facts_from_turn(
        &self,
        turn: &ChatTurn,
    ) -> SoulResult<Vec<MemoryFact>> {
        let extracted = extract_facts_from_turn(turn);
        if extracted.is_empty() {
            return Ok(Vec::new());
        }

        let mut conn = self.lock_conn()?;
        let embedder = self.read_embedder()?;
        for fact in &extracted {
            upsert_fact_tx(&mut conn, fact.clone(), None, &embedder)?;
        }
        Ok(extracted)
    }

    /// Compute the 7-emotion decay profile at `now_ms`, persisting decayed values in SQLite.
    pub fn get_emotion_profile_at(&self, now_ms: i64) -> SoulResult<EmotionProfileState> {
        let mut conn = self.lock_conn()?;
        get_emotion_profile_at_tx(&mut conn, self.character_id, now_ms)
    }

    /// Compute the 7-emotion decay profile at the current wall-clock time, persisting decayed values in SQLite.
    pub fn get_emotion_profile(&self) -> SoulResult<EmotionProfileState> {
        let mut conn = self.lock_conn()?;
        get_emotion_profile_tx(&mut conn, self.character_id)
    }

    /// Decay emotions up to `now_ms`, apply an impulse to `tag`, persist to SQLite, and return the new profile.
    pub fn record_emotion_impulse_at(
        &self,
        tag: EmotionTag,
        intensity: f32,
        now_ms: i64,
    ) -> SoulResult<EmotionProfileState> {
        let mut conn = self.lock_conn()?;
        record_emotion_impulse_at_tx(&mut conn, self.character_id, tag, intensity, now_ms)
    }

    /// Decay emotions to now, apply an impulse to `tag`, persist to SQLite, and return the new profile.
    pub fn record_emotion_impulse(
        &self,
        tag: EmotionTag,
        intensity: f32,
    ) -> SoulResult<EmotionProfileState> {
        let mut conn = self.lock_conn()?;
        record_emotion_impulse_tx(&mut conn, self.character_id, tag, intensity)
    }

    /// Add a diary entry or merge into an existing same-day `(entry_date, source)` entry,
    /// keeping `diary_entries_fts` synchronized.
    pub fn add_or_merge_diary_entry(
        &self,
        title: &str,
        content: &str,
        mood: EmotionTag,
        source: &str,
        entry_date_opt: Option<&str>,
    ) -> SoulResult<DiaryEntry> {
        let mut conn = self.lock_conn()?;
        add_or_merge_diary_entry_tx(
            &mut conn,
            self.character_id,
            title,
            content,
            mood,
            source,
            entry_date_opt,
        )
    }

    /// List diary entries for this character ordered most-recent first.
    pub fn list_diary_entries(&self, limit: usize) -> SoulResult<Vec<DiaryEntry>> {
        let conn = self.lock_conn()?;
        list_diary_entries_query(&conn, self.character_id, limit)
    }

    /// Retrieve the persisted `PersonaBio` for this character.
    pub fn get_persona_bio(&self) -> SoulResult<PersonaBio> {
        let conn = self.lock_conn()?;
        get_persona_bio_query(&conn, self.character_id)
    }

    /// Update and persist the `PersonaBio` for this character.
    pub fn update_persona_bio(&self, bio: PersonaBio) -> SoulResult<PersonaBio> {
        let mut conn = self.lock_conn()?;
        update_persona_bio_tx(&mut conn, self.character_id, bio)
    }

    /// Retrieve the persisted `UserProfile`.
    pub fn get_user_profile(&self) -> SoulResult<UserProfile> {
        let conn = self.lock_conn()?;
        get_user_profile_query(&conn)
    }

    /// Update and persist the `UserProfile`.
    pub fn update_user_profile(&self, profile: UserProfile) -> SoulResult<UserProfile> {
        let mut conn = self.lock_conn()?;
        update_user_profile_tx(&mut conn, profile)
    }

    /// Compact redundant active facts into their newest entry, returning the number of superseded duplicates.
    pub fn compact_redundant_facts(&self) -> SoulResult<usize> {
        let mut conn = self.lock_conn()?;
        compact_redundant_facts_tx(&mut conn)
    }

    fn lock_conn(&self) -> SoulResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| SoulError::Internal("SoulMemoryStore mutex poisoned".into()))
    }

    fn read_embedder(&self) -> SoulResult<std::sync::RwLockReadGuard<'_, Embedder>> {
        self.embedder
            .read()
            .map_err(|_| SoulError::Internal("SoulMemoryStore embedder lock poisoned".into()))
    }
}

#[async_trait]
impl MemoryEngine for SoulMemoryStore {
    async fn append_turn(&self, mut turn: ChatTurn) -> SoulResult<()> {
        if turn.id.trim().is_empty() {
            turn.id = new_id();
        }
        if turn.timestamp_ms <= 0 {
            turn.timestamp_ms = now_timestamp_ms();
        }

        let mut conn = self.lock_conn()?;
        let tx = conn.transaction().map_err(db_err)?;

        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM turns WHERE id = ?1)",
                params![&turn.id],
                |r| r.get(0),
            )
            .map_err(db_err)?;

        let role_str = turn.role.to_string();
        let emotion_str = turn.emotion.map(|e| e.to_string());

        tx.execute(
            "INSERT INTO turns (id, role, content, emotion, timestamp_ms, consolidated)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)
             ON CONFLICT(id) DO UPDATE SET
                role = excluded.role,
                content = excluded.content,
                emotion = excluded.emotion,
                timestamp_ms = excluded.timestamp_ms",
            params![
                &turn.id,
                &role_str,
                &turn.content,
                &emotion_str,
                turn.timestamp_ms,
            ],
        )
        .map_err(db_err)?;

        if !exists {
            tx.execute(
                "UPDATE relationship_state
                 SET total_turns = total_turns + 1,
                     last_interaction_at = MAX(last_interaction_at, ?1)
                 WHERE id = 1",
                params![turn.timestamp_ms],
            )
            .map_err(db_err)?;
        }

        tx.commit().map_err(db_err)?;
        Ok(())
    }

    async fn recent_turns(&self, limit: usize) -> SoulResult<Vec<ChatTurn>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, role, content, emotion, timestamp_ms
                 FROM turns
                 ORDER BY timestamp_ms DESC, rowid DESC
                 LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(db_err)?;

        let mut turns = Vec::new();
        for item in rows {
            let (id, role_str, content, emotion_str, timestamp_ms) = item.map_err(db_err)?;
            let role = role_str.parse::<ChatRole>()?;
            let emotion = match emotion_str {
                Some(s) if !s.trim().is_empty() => Some(s.parse::<EmotionTag>()?),
                _ => None,
            };
            turns.push(ChatTurn {
                id,
                role,
                content,
                emotion,
                timestamp_ms,
            });
        }
        // Return in chronological order (oldest -> newest among the recent window).
        turns.reverse();
        Ok(turns)
    }

    async fn retrieve_context(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        fact_limit: usize,
        episode_limit: usize,
    ) -> SoulResult<MemoryRetrievalContext> {
        let mut conn = self.lock_conn()?;
        let embedder = self.read_embedder()?;
        retrieve_context_sync(
            &mut conn,
            &embedder,
            query,
            query_embedding,
            fact_limit,
            episode_limit,
        )
    }

    async fn upsert_fact(&self, fact: MemoryFact, embedding: Option<&[f32]>) -> SoulResult<()> {
        let mut conn = self.lock_conn()?;
        let embedder = self.read_embedder()?;
        upsert_fact_tx(&mut conn, fact, embedding, &embedder)
    }

    async fn list_facts(
        &self,
        limit: usize,
        include_superseded: bool,
    ) -> SoulResult<Vec<MemoryFact>> {
        let conn = self.lock_conn()?;
        list_facts_query(&conn, limit, include_superseded)
    }

    async fn store_episode(
        &self,
        episode: EpisodeRecord,
        embedding: Option<&[f32]>,
    ) -> SoulResult<()> {
        let mut conn = self.lock_conn()?;
        let embedder = self.read_embedder()?;
        store_episode_tx(&mut conn, episode, embedding, &embedder)
    }

    async fn list_dream_diaries(&self, limit: usize) -> SoulResult<Vec<DreamDiary>> {
        let conn = self.lock_conn()?;
        list_dream_diaries_query(&conn, limit)
    }

    async fn store_dream_diary(&self, diary: DreamDiary) -> SoulResult<()> {
        let mut conn = self.lock_conn()?;
        store_dream_diary_tx(&mut conn, diary)
    }

    async fn get_relationship_state(&self) -> SoulResult<RelationshipState> {
        let conn = self.lock_conn()?;
        get_relationship_state_query(&conn)
    }

    async fn update_relationship_state(&self, state: RelationshipState) -> SoulResult<()> {
        let mut conn = self.lock_conn()?;
        update_relationship_state_tx(&mut conn, state)
    }

    async fn consolidate_now(&self) -> SoulResult<Option<DreamDiary>> {
        let mut conn = self.lock_conn()?;
        let embedder = self.read_embedder()?;
        consolidate_now_tx(&mut conn, &embedder, self.character_id)
    }

    async fn export_snapshot(&self, target_path: &Path) -> SoulResult<String> {
        let conn = self.lock_conn()?;
        export_snapshot_sync(&conn, target_path)
    }

    async fn import_snapshot(&self, source_path: &Path) -> SoulResult<usize> {
        let mut conn = self.lock_conn()?;
        import_snapshot_sync(&mut conn, source_path, self.character_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("soulmate2d_test_{}_{}", new_id(), name))
    }

    fn make_fact(id: &str, category: &str, key: &str, value: &str, ts_ms: i64) -> MemoryFact {
        MemoryFact {
            id: id.to_string(),
            category: category.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            confidence: 0.9,
            importance: 0.6,
            memory_heat: 0.5,
            access_count: 0,
            created_at: ts_ms,
            last_accessed_at: ts_ms,
            superseded_by: None,
            superseded_at: None,
        }
    }

    #[tokio::test]
    async fn sqlite_vec_and_turns_persistence() {
        let db_path = temp_path("turns.sqlite");
        {
            let store = SoulMemoryStore::open(&db_path, None).expect("open file store");
            assert_eq!(store.embedder_kind(), "ngram-fallback");

            let t1 = ChatTurn {
                id: "t1".into(),
                role: ChatRole::User,
                content: "สวัสดีมีนา วันนี้อากาศดีจัง".into(),
                emotion: None,
                timestamp_ms: 1_000,
            };
            let t2 = ChatTurn {
                id: "t2".into(),
                role: ChatRole::Assistant,
                content: "สวัสดีค่ะ! วันนี้อยากคุยเรื่องอะไรดีคะ".into(),
                emotion: Some(EmotionTag::Joy),
                timestamp_ms: 2_000,
            };
            let t3 = ChatTurn {
                id: "t3".into(),
                role: ChatRole::User,
                content: "Let's talk about coffee".into(),
                emotion: None,
                timestamp_ms: 3_000,
            };

            store.append_turn(t1).await.unwrap();
            store.append_turn(t2).await.unwrap();
            store.append_turn(t3).await.unwrap();

            let recent = store.recent_turns(2).await.unwrap();
            assert_eq!(recent.len(), 2);
            assert_eq!(recent[0].id, "t2");
            assert_eq!(recent[1].id, "t3");
            assert_eq!(recent[0].emotion, Some(EmotionTag::Joy));
        }

        // Reopen persisted file and verify state survived.
        {
            let reopened = SoulMemoryStore::open(&db_path, None).expect("reopen file store");
            let all = reopened.recent_turns(10).await.unwrap();
            assert_eq!(all.len(), 3);
            assert_eq!(all[0].content, "สวัสดีมีนา วันนี้อากาศดีจัง");
            let rel = reopened.get_relationship_state().await.unwrap();
            assert_eq!(rel.total_turns, 3);
        }

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[tokio::test]
    async fn rrf_hybrid_retrieval_and_rehearsal_boost() {
        let store = SoulMemoryStore::in_memory(None).unwrap();
        let now = now_timestamp_ms();

        let coffee_fact = make_fact(
            "f-coffee",
            "preference",
            "drink",
            "User loves iced oat milk latte coffee every morning",
            now - 10_000,
        );
        let cat_fact = make_fact(
            "f-cat",
            "companion",
            "pet",
            "User has a ginger cat named Mochi",
            now - 5_000,
        );
        let thai_fact = make_fact(
            "f-thai",
            "location",
            "city",
            "ชอบไปเที่ยวเชียงใหม่ช่วงหน้าหนาว",
            now - 2_000,
        );

        let mut cold_boosted = make_fact("f-unrelated", "work", "tool", "Uses Neovim", now);
        cold_boosted.memory_heat = 0.95;
        cold_boosted.access_count = 8;

        store.upsert_fact(coffee_fact, None).await.unwrap();
        store.upsert_fact(cat_fact, None).await.unwrap();
        store.upsert_fact(thai_fact, None).await.unwrap();
        store.upsert_fact(cold_boosted, None).await.unwrap();

        let ep_coffee = EpisodeRecord {
            id: "ep-coffee".into(),
            summary: "Discussed pour-over coffee beans and cafe hopping".into(),
            raw_turn_count: 6,
            emotional_valence: 0.7,
            memory_heat: 0.5,
            created_at: now - 20_000,
            last_accessed_at: now - 20_000,
        };
        let ep_work = EpisodeRecord {
            id: "ep-work".into(),
            summary: "Debugged Rust async trait compiler error".into(),
            raw_turn_count: 8,
            emotional_valence: 0.1,
            memory_heat: 0.9,
            created_at: now - 1_000,
            last_accessed_at: now - 1_000,
        };
        store.store_episode(ep_coffee, None).await.unwrap();
        store.store_episode(ep_work, None).await.unwrap();

        let ctx = store
            .retrieve_context("iced latte coffee", None, 2, 1)
            .await
            .unwrap();
        assert_eq!(ctx.facts[0].id, "f-coffee");
        assert_eq!(ctx.facts[0].access_count, 1);
        assert!(ctx.facts[0].memory_heat > 0.5);
        assert_eq!(ctx.episodes.len(), 1);
        assert_eq!(ctx.episodes[0].id, "ep-coffee");
        assert!(ctx.episodes[0].memory_heat > 0.5);

        // Verify Thai retrieval works cleanly without space tokenization issues.
        let thai_ctx = store
            .retrieve_context("เที่ยวเชียงใหม่", None, 2, 1)
            .await
            .unwrap();
        assert_eq!(thai_ctx.facts[0].id, "f-thai");
    }

    #[tokio::test]
    async fn ebbinghaus_and_actr_decay_ordering() {
        let store = SoulMemoryStore::in_memory(None).unwrap();
        let now = now_timestamp_ms();
        let one_hour_ms = 3_600_000_i64;

        // Provide identical embeddings so vector cosine & FTS tie, isolating Ebbinghaus + ACT-R.
        let mut shared_vec = vec![0.0_f32; EMBEDDING_DIM];
        shared_vec[0] = 0.8;
        shared_vec[1] = 0.6;

        let mut stale_fact = make_fact(
            "f-stale",
            "note",
            "k1",
            "alpha memory item",
            now - 500 * one_hour_ms,
        );
        stale_fact.memory_heat = 0.4;
        stale_fact.access_count = 0;
        stale_fact.importance = 0.2;

        let mut fresh_rehearsed = make_fact(
            "f-fresh",
            "note",
            "k2",
            "alpha memory item",
            now - one_hour_ms,
        );
        fresh_rehearsed.memory_heat = 0.85;
        fresh_rehearsed.access_count = 10;
        fresh_rehearsed.importance = 0.8;

        store
            .upsert_fact(stale_fact, Some(&shared_vec))
            .await
            .unwrap();
        store
            .upsert_fact(fresh_rehearsed, Some(&shared_vec))
            .await
            .unwrap();

        let ctx = store
            .retrieve_context("alpha memory item", Some(&shared_vec), 2, 0)
            .await
            .unwrap();
        assert_eq!(ctx.facts.len(), 2);
        assert_eq!(ctx.facts[0].id, "f-fresh");
        assert_eq!(ctx.facts[1].id, "f-stale");
    }

    #[tokio::test]
    async fn contradiction_detection_superseding_and_bilingual_extraction() {
        let store = SoulMemoryStore::in_memory(None).unwrap();
        let now = now_timestamp_ms();

        // Direct upsert_fact contradiction test with whitespace/case normalization.
        let old_city = make_fact("city-1", " Location ", " City ", "Tokyo", now - 5_000);
        let new_city = make_fact("city-2", "location", "city", "Bangkok", now - 1_000);
        store.upsert_fact(old_city, None).await.unwrap();
        store.upsert_fact(new_city, None).await.unwrap();

        let active = store.list_facts(20, false).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "city-2");
        assert_eq!(active[0].value, "Bangkok");
        assert!(active[0].superseded_by.is_none());

        let all = store.list_facts(20, true).await.unwrap();
        assert_eq!(all.len(), 2);
        let old_row = all.iter().find(|f| f.id == "city-1").unwrap();
        assert_eq!(old_row.superseded_by.as_deref(), Some("city-2"));
        assert!(old_row.superseded_at.is_some());

        // Active retrieval must exclude the superseded Tokyo fact even when querying "Tokyo".
        let ctx = store.retrieve_context("Tokyo city", None, 10, 0).await.unwrap();
        assert!(!ctx.facts.iter().any(|f| f.id == "city-1"));

        // Bilingual extraction + automatic superseding from turns.
        let turn_en = ChatTurn {
            id: "u1".into(),
            role: ChatRole::User,
            content: "My name is Alice and I work as a designer. I don't like durian.".into(),
            emotion: None,
            timestamp_ms: now + 1_000,
        };
        let extracted_en = store
            .extract_and_upsert_facts_from_turn(&turn_en)
            .await
            .unwrap();
        assert!(extracted_en.iter().any(|f| f.key == "name" && f.value == "Alice"));
        assert!(
            extracted_en
                .iter()
                .any(|f| f.key == "occupation" && f.value == "designer")
        );
        assert!(
            extracted_en
                .iter()
                .any(|f| f.key == "dislikes" && f.value == "durian")
        );

        let turn_th = ChatTurn {
            id: "u2".into(),
            role: ChatRole::User,
            content: "ฉันชื่อมีนา ตอนนี้อยู่ที่เชียงใหม่ ฉันชอบชาเขียวครับ".into(),
            emotion: None,
            timestamp_ms: now + 2_000,
        };
        let extracted_th = store
            .extract_and_upsert_facts_from_turn(&turn_th)
            .await
            .unwrap();
        assert!(extracted_th.iter().any(|f| f.key == "name" && f.value == "มีนา"));
        assert!(
            extracted_th
                .iter()
                .any(|f| f.key == "city" && f.value == "เชียงใหม่")
        );
        assert!(
            extracted_th
                .iter()
                .any(|f| f.key == "likes" && f.value == "ชาเขียว")
        );

        let active_after = store.list_facts(20, false).await.unwrap();
        let active_name = active_after.iter().find(|f| f.key == "name").unwrap();
        let active_city = active_after.iter().find(|f| f.key == "city").unwrap();
        assert_eq!(active_name.value, "มีนา");
        assert_eq!(active_city.value, "เชียงใหม่");

        // Full history still holds Alice and Bangkok and Tokyo.
        let full_history = store.list_facts(50, true).await.unwrap();
        assert!(full_history.iter().any(|f| f.value == "Alice" && f.superseded_by.is_some()));
        assert!(full_history.iter().any(|f| f.value == "Bangkok" && f.superseded_by.is_some()));
        assert!(full_history.iter().any(|f| f.value == "Tokyo" && f.superseded_by.is_some()));
    }

    #[tokio::test]
    async fn dream_consolidation_and_bond_update() {
        let store = SoulMemoryStore::in_memory(None).unwrap();
        let initial_rel = store.get_relationship_state().await.unwrap();

        // Nothing to consolidate initially.
        assert!(store.consolidate_now().await.unwrap().is_none());

        let now = now_timestamp_ms();
        let turns = vec![
            ChatTurn {
                id: "c1".into(),
                role: ChatRole::User,
                content: "Today was exhausting, but talking with you helps.".into(),
                emotion: None,
                timestamp_ms: now - 3_000,
            },
            ChatTurn {
                id: "c2".into(),
                role: ChatRole::Assistant,
                content: "I'm always here for you. Let's take a deep breath together.".into(),
                emotion: Some(EmotionTag::Empathetic),
                timestamp_ms: now - 2_000,
            },
            ChatTurn {
                id: "c3".into(),
                role: ChatRole::User,
                content: "Thank you, Mina! That really cheered me up.".into(),
                emotion: Some(EmotionTag::Joy),
                timestamp_ms: now - 1_000,
            },
        ];
        for t in turns {
            store.append_turn(t).await.unwrap();
        }

        let diary = store
            .consolidate_now()
            .await
            .unwrap()
            .expect("consolidation should generate dream diary");
        assert_eq!(diary.period_type, "daily");
        assert!(diary.bond_delta > 0.0);
        assert!(!diary.summary.is_empty());

        let diaries = store.list_dream_diaries(10).await.unwrap();
        assert_eq!(diaries.len(), 1);
        assert_eq!(diaries[0].id, diary.id);

        let updated_rel = store.get_relationship_state().await.unwrap();
        assert_eq!(updated_rel.total_turns, 3);
        assert!(updated_rel.trust > initial_rel.trust);
        assert!(updated_rel.warmth > initial_rel.warmth);
        assert!(updated_rel.familiarity > initial_rel.familiarity);
        assert!((0.0..=1.0).contains(&updated_rel.trust));
        assert!((0.0..=1.0).contains(&updated_rel.warmth));
        assert!((0.0..=1.0).contains(&updated_rel.familiarity));
        assert!((0.0..=1.0).contains(&updated_rel.mood_baseline));

        let ctx = store
            .retrieve_context("exhausting cheered me up", None, 4, 4)
            .await
            .unwrap();
        assert_eq!(ctx.episodes.len(), 1);
        assert_eq!(ctx.episodes[0].raw_turn_count, 3);
        assert_eq!(ctx.recent_diaries.len(), 1);

        // No new unconsolidated turns -> returns None.
        assert!(store.consolidate_now().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn snapshot_export_import_roundtrip_and_validation() {
        let export_path = temp_path("snapshot.sqlite");
        let corrupt_path = temp_path("corrupt.sqlite");
        let wrong_schema_path = temp_path("wrong_schema.sqlite");

        let source = SoulMemoryStore::in_memory(None).unwrap();
        let now = now_timestamp_ms();
        source
            .append_turn(ChatTurn {
                id: "s1".into(),
                role: ChatRole::User,
                content: "Remember my favorite tea is matcha".into(),
                emotion: None,
                timestamp_ms: now,
            })
            .await
            .unwrap();
        source
            .upsert_fact(
                make_fact("sf1", "preference", "tea", "hojicha", now - 1_000),
                None,
            )
            .await
            .unwrap();
        source
            .upsert_fact(
                make_fact("sf2", "preference", "tea", "matcha", now),
                None,
            )
            .await
            .unwrap();
        let _ = source.consolidate_now().await.unwrap();

        let exported_str = source.export_snapshot(&export_path).await.unwrap();
        assert_eq!(exported_str, export_path.to_string_lossy());
        assert!(export_path.is_file());

        // Reject invalid non-SQLite file.
        std::fs::write(&corrupt_path, b"not a sqlite file at all, just random garbage bytes padding out to 128 bytes for header test").unwrap();
        let target = SoulMemoryStore::in_memory(None).unwrap();
        assert!(target.import_snapshot(&corrupt_path).await.is_err());

        // Reject SQLite database missing Coral&Kai Companion schema.
        {
            let conn = Connection::open(&wrong_schema_path).unwrap();
            conn.execute("CREATE TABLE unrelated (id INTEGER PRIMARY KEY)", [])
                .unwrap();
        }
        assert!(target.import_snapshot(&wrong_schema_path).await.is_err());

        // Valid import succeeds and restores relational, FTS5, and vector tables.
        let imported_count = target.import_snapshot(&export_path).await.unwrap();
        assert!(imported_count >= 5); // 1 turn + 2 facts + 1 episode + 1 dream_diary

        let restored_turns = target.recent_turns(10).await.unwrap();
        assert_eq!(restored_turns.len(), 1);
        assert_eq!(restored_turns[0].content, "Remember my favorite tea is matcha");

        let restored_active = target.list_facts(10, false).await.unwrap();
        assert_eq!(restored_active.len(), 1);
        assert_eq!(restored_active[0].value, "matcha");

        let restored_all = target.list_facts(10, true).await.unwrap();
        assert_eq!(restored_all.len(), 2);

        let restored_ctx = target
            .retrieve_context("matcha tea", None, 5, 5)
            .await
            .unwrap();
        assert_eq!(restored_ctx.facts.len(), 1);
        assert_eq!(restored_ctx.facts[0].value, "matcha");
        assert_eq!(restored_ctx.episodes.len(), 1);
        assert_eq!(restored_ctx.recent_diaries.len(), 1);

        let _ = std::fs::remove_file(&export_path);
        let _ = std::fs::remove_file(&corrupt_path);
        let _ = std::fs::remove_file(&wrong_schema_path);
    }

    #[test]
    fn deterministic_fallback_embeddings_and_optional_onnx_integration() {
        let v1 = char_ngram_embed("ฉันชอบดื่มกาแฟลาเต้เย็นตอนเช้า");
        let v2 = char_ngram_embed("กาแฟลาเต้เย็นตอนเช้า");
        let v3 = char_ngram_embed("quantum chromodynamics lattice simulation");

        assert_eq!(v1.len(), EMBEDDING_DIM);
        assert_eq!(v2.len(), EMBEDDING_DIM);
        let norm1: f32 = v1.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm1 - 1.0).abs() < 1e-4);

        let sim_12: f32 = v1.iter().zip(v2.iter()).map(|(a, b)| a * b).sum();
        let sim_13: f32 = v1.iter().zip(v3.iter()).map(|(a, b)| a * b).sum();
        assert!(
            sim_12 > sim_13,
            "Thai semantic overlap ({sim_12}) should exceed unrelated English ({sim_13})"
        );

        // Optional integration check if both model_quantized.onnx + tokenizer.json + ORT dylib exist.
        let main_embed_dir = Path::new(
            "/Users/goshi/.gemini/antigravity/scratch/soulmate2d/src-tauri/resources/models/embed",
        );
        if main_embed_dir.join("model_quantized.onnx").is_file()
            && main_embed_dir.join("tokenizer.json").is_file()
        {
            let embedder = Embedder::from_dir(Some(main_embed_dir)).unwrap();
            let emb = embedder.embed("Hello Mina, I live in Bangkok").unwrap();
            assert_eq!(emb.len(), EMBEDDING_DIM);
            let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3);
        }
    }

    #[test]
    #[ignore = "requires local model_quantized.onnx, tokenizer.json, and libonnxruntime.dylib"]
    fn onnx_quantized_minilm_smoke_test() {
        let dir = std::env::var_os("SOUL_EMBED_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    "/Users/goshi/.gemini/antigravity/scratch/soulmate2d/src-tauri/resources/models/embed",
                )
            });
        let embedder = Embedder::from_dir(Some(&dir)).expect("Embedder::from_dir");
        assert_eq!(
            embedder.kind(),
            "onnx",
            "Expected real ONNX backend to load from {}",
            dir.display()
        );

        let th = embedder
            .embed("ฉันชอบดื่มกาแฟลาเต้ตอนเช้า")
            .expect("embed Thai");
        let en_related = embedder
            .embed("I love drinking morning latte coffee")
            .expect("embed related English");
        let en_unrelated = embedder
            .embed("quantum chromodynamics lattice simulation")
            .expect("embed unrelated English");

        assert_eq!(th.len(), EMBEDDING_DIM);
        let norm: f32 = th.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3);

        let sim_related: f32 = th.iter().zip(en_related.iter()).map(|(a, b)| a * b).sum();
        let sim_unrelated: f32 = th.iter().zip(en_unrelated.iter()).map(|(a, b)| a * b).sum();
        println!(
            "ONNX cross-lingual cosine sim: related={sim_related:.4}, unrelated={sim_unrelated:.4}"
        );
        assert!(
            sim_related > sim_unrelated + 0.15,
            "Cross-lingual MiniLM similarity ({sim_related}) should exceed unrelated ({sim_unrelated})"
        );
    }

    #[test]
    fn onnx_embed_validation_and_reload_graceful_errors() {
        let dir = temp_path("embed_byom");
        std::fs::create_dir_all(&dir).unwrap();

        // Store boots cleanly in zero-model mode even when embed_dir has no ONNX assets.
        let store = SoulMemoryStore::in_memory(Some(&dir)).unwrap();
        assert_eq!(store.embedder_kind(), "ngram-fallback");
        assert!(store.active_embed_model_path().is_none());

        // 1. Missing file -> ModelNotFound
        let missing = dir.join("missing.onnx");
        assert!(matches!(
            validate_onnx_embed_file(&missing),
            Err(SoulError::ModelNotFound(_))
        ));

        // 2. Wrong extension -> InvalidInput
        let wrong_ext = dir.join("model.bin");
        std::fs::write(&wrong_ext, [0x08, 0x07, 0x12, 0x00]).unwrap();
        assert!(matches!(
            validate_onnx_embed_file(&wrong_ext),
            Err(SoulError::InvalidInput(_))
        ));

        // 3. Empty file -> InvalidInput
        let empty_onnx = dir.join("empty.onnx");
        std::fs::write(&empty_onnx, b"").unwrap();
        assert!(matches!(
            validate_onnx_embed_file(&empty_onnx),
            Err(SoulError::InvalidInput(_))
        ));

        // 4. Invalid header bytes (e.g. HTML) -> InvalidInput
        let html_onnx = dir.join("html.onnx");
        std::fs::write(&html_onnx, b"<html>not onnx</html>").unwrap();
        assert!(matches!(
            validate_onnx_embed_file(&html_onnx),
            Err(SoulError::InvalidInput(_))
        ));

        // 5. Valid protobuf start byte without sibling tokenizer.json -> ModelNotFound
        let valid_proto_onnx = dir.join("minilm.onnx");
        std::fs::write(&valid_proto_onnx, [0x08, 0x08, 0x12, 0x04, 0x00, 0x00, 0x00, 0x00]).unwrap();
        let err = validate_onnx_embed_file(&valid_proto_onnx).expect_err("requires tokenizer.json");
        assert!(matches!(err, SoulError::ModelNotFound(ref msg) if msg.contains("tokenizer.json")));

        // Reload with dummy protobuf bytes (even with tokenizer.json) fails cleanly without
        // corrupting the active fallback embedder.
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        validate_onnx_embed_file(&valid_proto_onnx).expect("header + tokenizer.json passes validation");
        assert!(store.reload_embedder_from_file(&valid_proto_onnx).is_err());
        assert_eq!(store.embedder_kind(), "ngram-fallback");
        assert!(store.active_embed_model_path().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn emotion_decay_half_life_and_dominant_selection() {
        let db_path = temp_path("emotion_decay.sqlite");
        let t0: i64 = 1_700_000_000_000;

        let (joy_baseline, joy_half_life_min) = {
            let store = SoulMemoryStore::open_for_character(&db_path, None, CharacterId::Coral)
                .expect("open store");
            let initial = store.get_emotion_profile_at(t0).unwrap();
            assert_eq!(initial.character_id, CharacterId::Coral);
            assert_eq!(initial.emotions.len(), 7);
            assert_eq!(initial.dominant, EmotionTag::Neutral);

            // t = 0: impulse sets Joy to 0.90 -> Joy becomes dominant.
            let p0 = store
                .record_emotion_impulse_at(EmotionTag::Joy, 0.90, t0)
                .unwrap();
            assert_eq!(p0.dominant, EmotionTag::Joy);
            assert!((p0.dominant_intensity - 0.90).abs() < 1e-5);
            let joy0 = p0.emotions.iter().find(|e| e.tag == EmotionTag::Joy).unwrap();
            assert!((joy0.intensity - 0.90).abs() < 1e-5);

            let half_life_ms = (joy0.half_life_minutes * 60_000.0).round() as i64;

            // t = H: halfway between 0.90 and baseline.
            let p_h = store.get_emotion_profile_at(t0 + half_life_ms).unwrap();
            let joy_h = p_h.emotions.iter().find(|e| e.tag == EmotionTag::Joy).unwrap();
            let expected_h = joy0.baseline + (0.90 - joy0.baseline) * 0.5;
            assert!(
                (joy_h.intensity - expected_h).abs() < 1e-4,
                "t=H expected {expected_h}, got {}",
                joy_h.intensity
            );

            // t = 2H: quarter of excess above baseline remains.
            let p_2h = store.get_emotion_profile_at(t0 + 2 * half_life_ms).unwrap();
            let joy_2h = p_2h.emotions.iter().find(|e| e.tag == EmotionTag::Joy).unwrap();
            let expected_2h = joy0.baseline + (0.90 - joy0.baseline) * 0.25;
            assert!(
                (joy_2h.intensity - expected_2h).abs() < 1e-4,
                "t=2H expected {expected_2h}, got {}",
                joy_2h.intensity
            );

            // t = 100H: converges cleanly to baseline, Neutral dominates again.
            let p_conv = store.get_emotion_profile_at(t0 + 100 * half_life_ms).unwrap();
            let joy_conv = p_conv.emotions.iter().find(|e| e.tag == EmotionTag::Joy).unwrap();
            assert!((joy_conv.intensity - joy0.baseline).abs() < 1e-5);
            assert_eq!(p_conv.dominant, EmotionTag::Neutral);

            // Multi-emotion impulses: Shy 0.85, then Sadness 0.95 -> Sadness dominates.
            let t1 = t0 + 101 * half_life_ms;
            let p_shy = store
                .record_emotion_impulse_at(EmotionTag::Shy, 0.85, t1)
                .unwrap();
            assert_eq!(p_shy.dominant, EmotionTag::Shy);

            let p_sad = store
                .record_emotion_impulse_at(EmotionTag::Sadness, 0.95, t1 + 5_000)
                .unwrap();
            assert_eq!(p_sad.dominant, EmotionTag::Sadness);
            assert!((p_sad.dominant_intensity - 0.95).abs() < 1e-5);

            (joy0.baseline, joy0.half_life_minutes)
        };

        // Reopen DB and verify persisted emotion state + half-life decay continuity.
        {
            let reopened =
                SoulMemoryStore::open_for_character(&db_path, None, CharacterId::Coral).unwrap();
            let half_life_ms = (joy_half_life_min * 60_000.0).round() as i64;
            let t1 = t0 + 101 * half_life_ms + 5_000;
            let p = reopened.get_emotion_profile_at(t1).unwrap();
            assert_eq!(p.dominant, EmotionTag::Sadness);
            let joy = p.emotions.iter().find(|e| e.tag == EmotionTag::Joy).unwrap();
            assert!((joy.intensity - joy_baseline).abs() < 1e-4);
        }

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[test]
    fn diary_same_day_merge_and_fts_sync() {
        let store = SoulMemoryStore::in_memory(None).unwrap();

        let first = store
            .add_or_merge_diary_entry(
                "Cozy Morning",
                "Shared iced matcha latte together.",
                EmotionTag::Joy,
                "manual",
                Some("2026-11-24"),
            )
            .unwrap();
        assert_eq!(first.character_id, CharacterId::Coral);
        assert_eq!(first.merged_count, 1);
        assert_eq!(first.entry_date, "2026-11-24");

        // Same (entry_date, source) merges into existing entry cleanly.
        let merged = store
            .add_or_merge_diary_entry(
                "Evening Note",
                "Also talked about Chiang Mai winter trip.",
                EmotionTag::Empathetic,
                "manual",
                Some("2026-11-24"),
            )
            .unwrap();
        assert_eq!(merged.id, first.id);
        assert_eq!(merged.merged_count, 2);
        assert_eq!(merged.mood, EmotionTag::Empathetic);
        assert!(merged.content.contains("Shared iced matcha latte together."));
        assert!(merged.content.contains("Also talked about Chiang Mai winter trip."));
        assert!(merged.updated_at >= first.created_at);

        // Different source on same day creates a distinct row.
        let inner = store
            .add_or_merge_diary_entry(
                "Inner Loop Note",
                "Checked in quietly during the afternoon.",
                EmotionTag::Thinking,
                "inner-loop",
                Some("2026-11-24"),
            )
            .unwrap();
        assert_ne!(inner.id, first.id);
        assert_eq!(inner.merged_count, 1);

        let entries = store.list_diary_entries(10).unwrap();
        assert_eq!(entries.len(), 2);

        // Verify diary_entries_fts has both merged tokens indexed.
        let conn = store.lock_conn().unwrap();
        let fts_match_id: String = conn
            .query_row(
                "SELECT diary_id FROM diary_entries_fts WHERE diary_entries_fts MATCH 'Chiang'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_match_id, first.id);
    }

    #[test]
    fn persona_bio_and_user_profile_persistence() {
        let db_path = temp_path("profiles.sqlite");
        {
            let store =
                SoulMemoryStore::open_for_character(&db_path, None, CharacterId::Kai).unwrap();
            let bio = store.get_persona_bio().unwrap();
            assert_eq!(bio.character_id, CharacterId::Kai);
            assert_eq!(bio.display_name, "Kai · ไค");

            let updated_bio = store
                .update_persona_bio(PersonaBio {
                    character_id: CharacterId::Kai,
                    display_name: "Kai · ไค (Dev)".into(),
                    traits: "Analytical, calm, supportive".into(),
                    self_reflection: "Helping build clean Rust systems.".into(),
                    favorite_topics: "Rust, SQLite, coffee".into(),
                    updated_at: 0,
                })
                .unwrap();
            assert!(updated_bio.updated_at > 0);

            let updated_user = store
                .update_user_profile(UserProfile {
                    display_name: "Goshi".into(),
                    timezone: "Asia/Bangkok".into(),
                    location: "Bangkok".into(),
                    notes: "Prefers concise Thai/English replies".into(),
                    updated_at: 0,
                })
                .unwrap();
            assert!(updated_user.updated_at > 0);
        }

        {
            let reopened =
                SoulMemoryStore::open_for_character(&db_path, None, CharacterId::Kai).unwrap();
            let bio = reopened.get_persona_bio().unwrap();
            assert_eq!(bio.character_id, CharacterId::Kai);
            assert_eq!(bio.display_name, "Kai · ไค (Dev)");
            assert_eq!(bio.favorite_topics, "Rust, SQLite, coffee");

            let profile = reopened.get_user_profile().unwrap();
            assert_eq!(profile.display_name, "Goshi");
            assert_eq!(profile.timezone, "Asia/Bangkok");
            assert_eq!(profile.location, "Bangkok");
            assert_eq!(profile.notes, "Prefers concise Thai/English replies");
        }

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[tokio::test]
    async fn redundant_fact_compaction_during_consolidate_now() {
        let store = SoulMemoryStore::in_memory_for_character(None, CharacterId::Kai).unwrap();
        let now = now_timestamp_ms();

        // 1. Two active facts in same norm_category with different keys but identical normalized value.
        let mut old_drink = make_fact("f-old-val", "preference", "drink", "Iced Latte", now - 20_000);
        old_drink.access_count = 4;
        old_drink.memory_heat = 0.88;

        let mut new_drink = make_fact(
            "f-new-val",
            "Preference",
            "favorite_coffee",
            "  iced   latte ",
            now - 5_000,
        );
        new_drink.access_count = 2;
        new_drink.memory_heat = 0.50;

        store.upsert_fact(old_drink, None).await.unwrap();
        store.upsert_fact(new_drink, None).await.unwrap();

        // 2. Also insert an uncompacted duplicate active row with identical (norm_category, norm_key).
        {
            let conn = store.lock_conn().unwrap();
            conn.execute(
                "INSERT INTO facts (
                    id, category, key, norm_category, norm_key, value,
                    confidence, importance, memory_heat, access_count,
                    created_at, last_accessed_at, superseded_by, superseded_at
                 ) VALUES (?1, 'location', 'city', 'location', 'city', 'Bangkok', 0.8, 0.7, 0.92, 5, ?2, ?2, NULL, NULL)",
                params!["f-city-old", now - 30_000],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO facts (
                    id, category, key, norm_category, norm_key, value,
                    confidence, importance, memory_heat, access_count,
                    created_at, last_accessed_at, superseded_by, superseded_at
                 ) VALUES (?1, 'location', 'city', 'location', 'city', 'Chiang Mai', 0.9, 0.8, 0.40, 1, ?2, ?2, NULL, NULL)",
                params!["f-city-new", now - 2_000],
            )
            .unwrap();
        }

        store
            .append_turn(ChatTurn {
                id: "k1".into(),
                role: ChatRole::User,
                content: "Hey Kai, how was your morning?".into(),
                emotion: None,
                timestamp_ms: now - 1_000,
            })
            .await
            .unwrap();
        store
            .append_turn(ChatTurn {
                id: "k2".into(),
                role: ChatRole::Assistant,
                content: "Quiet and focused, glad you checked in.".into(),
                emotion: Some(EmotionTag::Thinking),
                timestamp_ms: now,
            })
            .await
            .unwrap();

        let dream = store
            .consolidate_now()
            .await
            .unwrap()
            .expect("consolidation returns DreamDiary");
        assert!(dream.summary.contains("Kai:"));
        assert!(!dream.summary.contains("Mina:"));

        // Verify consolidation also merged daily reflection into diary_entries.
        let diaries = store.list_diary_entries(10).unwrap();
        assert_eq!(diaries.len(), 1);
        assert_eq!(diaries[0].source, "consolidation");
        assert_eq!(diaries[0].character_id, CharacterId::Kai);
        assert_eq!(diaries[0].merged_count, 1);

        // Verify active vs superseded facts after compaction.
        let active = store.list_facts(20, false).await.unwrap();
        assert_eq!(active.len(), 2);

        let compacted_drink = active.iter().find(|f| f.id == "f-new-val").unwrap();
        assert_eq!(compacted_drink.access_count, 6);
        assert!((compacted_drink.memory_heat - 0.88).abs() < 1e-5);

        let compacted_city = active.iter().find(|f| f.id == "f-city-new").unwrap();
        assert_eq!(compacted_city.access_count, 6);
        assert!((compacted_city.memory_heat - 0.92).abs() < 1e-5);

        let all = store.list_facts(20, true).await.unwrap();
        let old_drink_row = all.iter().find(|f| f.id == "f-old-val").unwrap();
        assert_eq!(old_drink_row.superseded_by.as_deref(), Some("f-new-val"));
        assert!(old_drink_row.superseded_at.is_some());

        let old_city_row = all.iter().find(|f| f.id == "f-city-old").unwrap();
        assert_eq!(old_city_row.superseded_by.as_deref(), Some("f-city-new"));
        assert!(old_city_row.superseded_at.is_some());
    }

    #[tokio::test]
    async fn strict_coral_vs_kai_profile_isolation() {
        let coral_path = temp_path("coral.sqlite");
        let kai_path = temp_path("kai.sqlite");

        let coral = SoulMemoryStore::open_for_character(&coral_path, None, CharacterId::Coral)
            .expect("open coral");
        let kai = SoulMemoryStore::open_for_character(&kai_path, None, CharacterId::Kai)
            .expect("open kai");

        assert_eq!(coral.character_id(), CharacterId::Coral);
        assert_eq!(kai.character_id(), CharacterId::Kai);
        assert_eq!(coral.get_persona_bio().unwrap().display_name, "Coral · คอรัล");
        assert_eq!(kai.get_persona_bio().unwrap().display_name, "Kai · ไค");

        // Divergent emotions.
        let t0 = now_timestamp_ms();
        let coral_emo = coral
            .record_emotion_impulse_at(EmotionTag::Joy, 0.92, t0)
            .unwrap();
        let kai_emo = kai
            .record_emotion_impulse_at(EmotionTag::Thinking, 0.88, t0)
            .unwrap();
        assert_eq!(coral_emo.dominant, EmotionTag::Joy);
        assert_eq!(kai_emo.dominant, EmotionTag::Thinking);
        assert_eq!(coral.get_emotion_profile_at(t0).unwrap().dominant, EmotionTag::Joy);
        assert_eq!(kai.get_emotion_profile_at(t0).unwrap().dominant, EmotionTag::Thinking);

        // Divergent diaries & turns & consolidation speaker labels.
        coral
            .add_or_merge_diary_entry(
                "Coral Day",
                "Listened to acoustic guitar.",
                EmotionTag::Joy,
                "manual",
                Some("2026-11-24"),
            )
            .unwrap();
        assert_eq!(coral.list_diary_entries(10).unwrap().len(), 1);
        assert!(kai.list_diary_entries(10).unwrap().is_empty());

        coral
            .append_turn(ChatTurn {
                id: "ct1".into(),
                role: ChatRole::Assistant,
                content: "Good morning!".into(),
                emotion: Some(EmotionTag::Joy),
                timestamp_ms: t0,
            })
            .await
            .unwrap();
        let coral_dream = coral.consolidate_now().await.unwrap().unwrap();
        assert!(coral_dream.summary.contains("Coral:"));
        assert!(!coral_dream.summary.contains("Kai:"));
        assert!(kai.recent_turns(10).await.unwrap().is_empty());

        let _ = std::fs::remove_file(&coral_path);
        let _ = std::fs::remove_file(format!("{}-wal", coral_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", coral_path.display()));
        let _ = std::fs::remove_file(&kai_path);
        let _ = std::fs::remove_file(format!("{}-wal", kai_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", kai_path.display()));
    }
}
