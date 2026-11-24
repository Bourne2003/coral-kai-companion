use std::path::Path;

use async_trait::async_trait;
use shared_types::{
    ChatTurn, DreamDiary, EpisodeRecord, MemoryEngine, MemoryFact, MemoryRetrievalContext,
    RelationshipState, SoulError, SoulResult,
};

/// Persistent SoulMate2D memory store stub (implemented in Phase 2).
#[derive(Debug, Default)]
pub struct SoulMemoryStore;

impl SoulMemoryStore {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MemoryEngine for SoulMemoryStore {
    async fn append_turn(&self, _turn: ChatTurn) -> SoulResult<()> {
        Ok(())
    }

    async fn recent_turns(&self, _limit: usize) -> SoulResult<Vec<ChatTurn>> {
        Ok(Vec::new())
    }

    async fn retrieve_context(
        &self,
        _query: &str,
        _query_embedding: Option<&[f32]>,
        _fact_limit: usize,
        _episode_limit: usize,
    ) -> SoulResult<MemoryRetrievalContext> {
        Ok(MemoryRetrievalContext::default())
    }

    async fn upsert_fact(&self, _fact: MemoryFact, _embedding: Option<&[f32]>) -> SoulResult<()> {
        Ok(())
    }

    async fn list_facts(
        &self,
        _limit: usize,
        _include_superseded: bool,
    ) -> SoulResult<Vec<MemoryFact>> {
        Ok(Vec::new())
    }

    async fn store_episode(
        &self,
        _episode: EpisodeRecord,
        _embedding: Option<&[f32]>,
    ) -> SoulResult<()> {
        Ok(())
    }

    async fn list_dream_diaries(&self, _limit: usize) -> SoulResult<Vec<DreamDiary>> {
        Ok(Vec::new())
    }

    async fn store_dream_diary(&self, _diary: DreamDiary) -> SoulResult<()> {
        Ok(())
    }

    async fn get_relationship_state(&self) -> SoulResult<RelationshipState> {
        Ok(RelationshipState::default())
    }

    async fn update_relationship_state(&self, _state: RelationshipState) -> SoulResult<()> {
        Ok(())
    }

    async fn consolidate_now(&self) -> SoulResult<Option<DreamDiary>> {
        Ok(None)
    }

    async fn export_snapshot(&self, _target_path: &Path) -> SoulResult<String> {
        Err(SoulError::NotImplemented(
            "SoulMemoryStore::export_snapshot is not implemented yet".into(),
        ))
    }

    async fn import_snapshot(&self, _source_path: &Path) -> SoulResult<usize> {
        Err(SoulError::NotImplemented(
            "SoulMemoryStore::import_snapshot is not implemented yet".into(),
        ))
    }
}
