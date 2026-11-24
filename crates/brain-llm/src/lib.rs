use std::sync::Mutex;

use async_trait::async_trait;
use shared_types::{
    BrainEngine, BrainReply, ChatTurn, MemoryRetrievalContext, ModelInfo, SoulError, SoulResult,
    SystemRamStatus, TokenCallback,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

const DEFAULT_8GB_CEILING_MB: u64 = 4800;

/// Local LLM inference and hot-swap manager stub (implemented in Phase 2).
#[derive(Debug, Default)]
pub struct LlmBrainManager {
    active_model: Mutex<Option<ModelInfo>>,
}

impl LlmBrainManager {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BrainEngine for LlmBrainManager {
    async fn load_model(&self, model: &ModelInfo) -> SoulResult<()> {
        let mut guard = self
            .active_model
            .lock()
            .map_err(|_| SoulError::Internal("active_model mutex poisoned".into()))?;
        *guard = Some(model.clone());
        Err(SoulError::NotImplemented(
            "LlmBrainManager::load_model backend is not implemented yet".into(),
        ))
    }

    async fn unload_model(&self) -> SoulResult<()> {
        let mut guard = self
            .active_model
            .lock()
            .map_err(|_| SoulError::Internal("active_model mutex poisoned".into()))?;
        *guard = None;
        Ok(())
    }

    fn active_model(&self) -> Option<ModelInfo> {
        self.active_model.lock().ok().and_then(|g| g.clone())
    }

    async fn generate_reply(
        &self,
        _user_message: &str,
        _history: &[ChatTurn],
        _memory_ctx: &MemoryRetrievalContext,
        _on_token: TokenCallback,
    ) -> SoulResult<BrainReply> {
        Err(SoulError::NotImplemented(
            "LlmBrainManager::generate_reply is not implemented yet".into(),
        ))
    }

    async fn embed_text(&self, _text: &str) -> SoulResult<Vec<f32>> {
        Err(SoulError::NotImplemented(
            "LlmBrainManager::embed_text is not implemented yet".into(),
        ))
    }

    fn system_ram_status(&self) -> SystemRamStatus {
        let mut sys = System::new_with_specifics(
            RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::everything()),
        );
        sys.refresh_memory();
        let pid = Pid::from_u32(std::process::id());
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );

        let total_ram_mb = sys.total_memory() / (1024 * 1024);
        let available_ram_mb = sys.available_memory() / (1024 * 1024);
        let process_ram_mb = sys
            .process(pid)
            .map(|p| p.memory() / (1024 * 1024))
            .unwrap_or(0);
        let is_8gb_class = total_ram_mb <= 9216;
        let budget_ceiling_mb = if is_8gb_class {
            DEFAULT_8GB_CEILING_MB
        } else {
            (total_ram_mb as f64 * 0.70) as u64
        };

        SystemRamStatus {
            total_ram_mb,
            available_ram_mb,
            process_ram_mb,
            budget_ceiling_mb,
            is_8gb_class,
            active_llm_id: self.active_model().map(|m| m.id),
            active_asr_id: None,
        }
    }
}
