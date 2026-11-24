use shared_types::{RamSafetyTier, SoulError, SoulResult};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// Maximum RAM ceiling for 8GB-class Macs (system + app + LLM headroom guard).
pub const DEFAULT_8GB_CEILING_MB: u64 = 4800;

/// Strict LLM-only RAM ceiling on 8GB-class Macs (`estimated_ram_mb < 3800`).
pub const LLM_8GB_CEILING_MB: u64 = 3800;

/// Total system RAM threshold below which a host is treated as 8GB-class.
pub const EIGHT_GB_CLASS_MAX_MB: u64 = 9216;

/// Total system RAM threshold at or above which 16GB+ tuning parameters unlock.
pub const SIXTEEN_GB_CLASS_MIN_MB: u64 = 14336;

/// Snapshot of current host and process memory in megabytes (MiB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamSnapshot {
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub process_ram_mb: u64,
}

/// Hardware-adaptive llama.cpp inference and memory budget parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveInferenceParams {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub budget_ceiling_mb: u64,
    pub llm_ceiling_mb: Option<u64>,
    pub is_8gb_class: bool,
}

/// Abstraction for querying host and process RAM so unit tests can simulate 8GB and 16GB+ hosts.
pub trait RamProbe: Send + Sync {
    fn snapshot(&self) -> RamSnapshot;
}

/// Live `sysinfo`-backed RAM probe inspecting macOS Unified Memory and current process RSS.
#[derive(Debug, Default)]
pub struct SysinfoRamProbe;

impl RamProbe for SysinfoRamProbe {
    fn snapshot(&self) -> RamSnapshot {
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

        RamSnapshot {
            total_ram_mb,
            available_ram_mb,
            process_ram_mb,
        }
    }
}

/// Computes adaptive context window, batch size, and RAM ceilings from total host RAM.
pub fn adaptive_params_for_ram(total_ram_mb: u64) -> AdaptiveInferenceParams {
    if total_ram_mb <= EIGHT_GB_CLASS_MAX_MB {
        AdaptiveInferenceParams {
            n_ctx: 2048,
            n_batch: 256,
            budget_ceiling_mb: DEFAULT_8GB_CEILING_MB,
            llm_ceiling_mb: Some(LLM_8GB_CEILING_MB),
            is_8gb_class: true,
        }
    } else if total_ram_mb >= SIXTEEN_GB_CLASS_MIN_MB {
        AdaptiveInferenceParams {
            n_ctx: 4096,
            n_batch: 512,
            budget_ceiling_mb: (total_ram_mb as f64 * 0.70) as u64,
            llm_ceiling_mb: None,
            is_8gb_class: false,
        }
    } else {
        AdaptiveInferenceParams {
            n_ctx: 2048,
            n_batch: 256,
            budget_ceiling_mb: (total_ram_mb as f64 * 0.70) as u64,
            llm_ceiling_mb: None,
            is_8gb_class: false,
        }
    }
}

/// Classifies a GGUF model's estimated runtime memory footprint into a safety tier.
pub fn classify_ram_safety_tier(estimated_ram_mb: u64) -> RamSafetyTier {
    if estimated_ram_mb <= 3200 {
        RamSafetyTier::Safe8Gb
    } else if estimated_ram_mb <= 4500 {
        RamSafetyTier::Tight8Gb
    } else {
        RamSafetyTier::Recommended16GbPlus
    }
}

/// Estimates total runtime RAM (weights overhead + Metal KV cache) from GGUF file size and `n_ctx`.
pub fn estimate_gguf_ram_mb(file_size_bytes: u64, n_ctx: u32) -> u64 {
    let file_size_mb = (file_size_bytes as f64) / (1024.0 * 1024.0);
    let kv_cache_mb = ((u64::from(n_ctx) * 200) / 2048).max(128);
    (file_size_mb * 1.25).ceil() as u64 + kv_cache_mb
}

/// Checks the static per-LLM ceiling before dropping any currently loaded model.
pub fn validate_static_llm_ceiling(
    total_ram_mb: u64,
    estimated_ram_mb: u64,
    current_process_ram_mb: u64,
) -> SoulResult<()> {
    let params = adaptive_params_for_ram(total_ram_mb);
    if let Some(llm_ceiling) = params.llm_ceiling_mb
        && estimated_ram_mb >= llm_ceiling
    {
        let budget_remaining = params
            .budget_ceiling_mb
            .saturating_sub(current_process_ram_mb);
        let available_mb = llm_ceiling.saturating_sub(1).min(budget_remaining);
        return Err(SoulError::InsufficientRam {
            needed_mb: estimated_ram_mb,
            available_mb,
        });
    }
    Ok(())
}

/// Checks that the current process RAM plus the candidate LLM fits within the adaptive budget ceiling.
pub fn validate_footprint_against_budget(
    snapshot: RamSnapshot,
    estimated_ram_mb: u64,
) -> SoulResult<()> {
    let params = adaptive_params_for_ram(snapshot.total_ram_mb);
    validate_static_llm_ceiling(
        snapshot.total_ram_mb,
        estimated_ram_mb,
        snapshot.process_ram_mb,
    )?;

    let projected_mb = snapshot.process_ram_mb.saturating_add(estimated_ram_mb);
    if projected_mb > params.budget_ceiling_mb {
        let available_mb = params
            .budget_ceiling_mb
            .saturating_sub(snapshot.process_ram_mb);
        return Err(SoulError::InsufficientRam {
            needed_mb: projected_mb,
            available_mb,
        });
    }

    Ok(())
}
