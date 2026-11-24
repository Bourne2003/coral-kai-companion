use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::{LogOptions, send_logs_to_tracing};
use shared_types::{ModelInfo, SoulError, SoulResult, now_timestamp_ms};

use crate::prompt::format_chatml_prompt;
use crate::ram_guard::AdaptiveInferenceParams;

const DEFAULT_MAX_GEN_TOKENS: u32 = 256;

static LLAMA_BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

fn global_llama_backend() -> SoulResult<&'static LlamaBackend> {
    let res = LLAMA_BACKEND.get_or_init(|| {
        send_logs_to_tracing(LogOptions::default().with_logs_enabled(true));
        LlamaBackend::init().map_err(|err| err.to_string())
    });
    res.as_ref()
        .map_err(|err| SoulError::Llm(format!("Failed to initialize llama backend: {err}")))
}

/// A live loaded LLM instance capable of streaming token pieces.
pub trait LoadedModelInstance: Send + Sync {
    fn model_info(&self) -> &ModelInfo;

    fn generate(
        &self,
        messages: &[(String, String)],
        max_tokens: u32,
        on_piece: &mut dyn FnMut(&str),
    ) -> SoulResult<()>;
}

/// Backend loader abstraction enabling Metal llama.cpp inference in production
/// and deterministic drop-before-load lifecycle verification in unit tests.
pub trait ModelBackend: Send + Sync {
    fn load(
        &self,
        model: &ModelInfo,
        params: AdaptiveInferenceParams,
    ) -> SoulResult<Box<dyn LoadedModelInstance>>;
}

/// Production Apple Silicon Metal backend powered by `llama-cpp-2`.
#[derive(Debug, Default)]
pub struct LlamaCppMetalBackend;

impl ModelBackend for LlamaCppMetalBackend {
    fn load(
        &self,
        model: &ModelInfo,
        params: AdaptiveInferenceParams,
    ) -> SoulResult<Box<dyn LoadedModelInstance>> {
        let backend = global_llama_backend()?;
        let model_params = LlamaModelParams::default()
            .with_n_gpu_layers(99)
            .with_use_mlock(false)
            .with_use_mmap(true);

        let llama_model = Box::new(
            LlamaModel::load_from_file(backend, Path::new(&model.path), &model_params).map_err(
                |err| {
                    SoulError::Llm(format!(
                        "Failed to load GGUF model '{}': {err}",
                        model.path
                    ))
                },
            )?,
        );

        let effective_ctx = params
            .n_ctx
            .min(llama_model.n_ctx_train().max(512))
            .max(512);
        let effective_batch = params.n_batch.min(effective_ctx).max(64);
        let effective_ubatch = effective_batch.min(256);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(effective_ctx))
            .with_n_batch(effective_batch)
            .with_n_ubatch(effective_ubatch)
            .with_n_threads(4)
            .with_n_threads_batch(4);

        let ctx = llama_model
            .new_context(backend, ctx_params)
            .map_err(|err| SoulError::Llm(format!("Failed to create Metal KV context: {err}")))?;

        // SAFETY: `llama_model` is heap-allocated inside `Box<LlamaModel>` and declared AFTER `ctx`
        // in `LlamaCppLoadedModel`. Rust drops fields in declaration order, guaranteeing `ctx`
        // (and its Metal KV-cache) is always freed before `model` is dropped. `ctx` never escapes.
        let ctx_static: LlamaContext<'static> = unsafe { std::mem::transmute(ctx) };

        Ok(Box::new(LlamaCppLoadedModel {
            info: model.clone(),
            ctx: Mutex::new(ctx_static),
            model: llama_model,
            n_ctx: effective_ctx,
            n_batch: effective_batch,
        }))
    }
}

struct LlamaCppLoadedModel {
    info: ModelInfo,
    // Declared before `model` so KV-cache always drops prior to weight buffers.
    ctx: Mutex<LlamaContext<'static>>,
    model: Box<LlamaModel>,
    n_ctx: u32,
    n_batch: u32,
}

impl LoadedModelInstance for LlamaCppLoadedModel {
    fn model_info(&self) -> &ModelInfo {
        &self.info
    }

    fn generate(
        &self,
        messages: &[(String, String)],
        max_tokens: u32,
        on_piece: &mut dyn FnMut(&str),
    ) -> SoulResult<()> {
        let mut ctx = self
            .ctx
            .lock()
            .map_err(|_| SoulError::Internal("llama context mutex poisoned".into()))?;
        ctx.clear_kv_cache();

        let target_gen = if max_tokens == 0 {
            DEFAULT_MAX_GEN_TOKENS
        } else {
            max_tokens
        }
        .min(self.n_ctx / 2)
        .max(32);

        let max_prompt_tokens = (self.n_ctx as usize).saturating_sub(target_gen as usize);
        let prompt_tokens = self.tokenize_with_budget(messages, max_prompt_tokens)?;
        if prompt_tokens.is_empty() {
            return Err(SoulError::Llm("Formatted prompt produced zero tokens".into()));
        }

        let batch_cap = (self.n_batch as usize).max(1);
        let mut batch = LlamaBatch::new(batch_cap, 1);

        let total_prompt = prompt_tokens.len();
        let mut offset = 0usize;
        while offset < total_prompt {
            batch.clear();
            let end = (offset + batch_cap).min(total_prompt);
            for (idx, &token) in prompt_tokens[offset..end].iter().enumerate() {
                let pos = (offset + idx) as i32;
                let request_logits = (offset + idx + 1) == total_prompt;
                batch
                    .add(token, pos, &[0], request_logits)
                    .map_err(|err| SoulError::Llm(format!("Batch add failed: {err}")))?;
            }
            ctx.decode(&mut batch)
                .map_err(|err| SoulError::Llm(format!("Prompt decode failed: {err}")))?;
            offset = end;
        }

        let seed = (now_timestamp_ms() & 0xFFFF_FFFF) as u32;
        let vocab = self.model.vocab();
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::penalties(vocab.n_tokens(), 64, 1.12, 0.0, 0.0),
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.9, 1),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(0.72),
            LlamaSampler::dist(seed),
        ]);

        let mut n_cur = total_prompt as i32;
        let mut piece_bytes = Vec::with_capacity(32);
        let mut utf8_pending = Vec::with_capacity(32);

        for _ in 0..target_gen {
            if n_cur >= self.n_ctx as i32 {
                break;
            }
            let token = sampler.sample(&ctx, -1);
            if vocab.is_eog(token) {
                break;
            }
            sampler.accept(token);

            piece_bytes.clear();
            vocab.token_to_piece_into(token, &mut piece_bytes, false, None);
            utf8_pending.extend_from_slice(&piece_bytes);
            flush_valid_utf8(&mut utf8_pending, on_piece);

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|err| SoulError::Llm(format!("Generation batch add failed: {err}")))?;
            ctx.decode(&mut batch)
                .map_err(|err| SoulError::Llm(format!("Generation decode failed: {err}")))?;
            n_cur += 1;
        }

        if !utf8_pending.is_empty() {
            let lossy = String::from_utf8_lossy(&utf8_pending).into_owned();
            if !lossy.is_empty() {
                on_piece(&lossy);
            }
        }

        Ok(())
    }
}

impl LlamaCppLoadedModel {
    fn format_prompt(&self, messages: &[(String, String)]) -> String {
        if let Ok(tmpl) = self.model.chat_template(None) {
            let chat_msgs: Result<Vec<_>, _> = messages
                .iter()
                .map(|(role, content)| LlamaChatMessage::new(role.clone(), content.clone()))
                .collect();
            if let Ok(msgs) = chat_msgs
                && let Ok(rendered) = self.model.apply_chat_template(&tmpl, &msgs, true)
            {
                return rendered;
            }
        }
        format_chatml_prompt(messages)
    }

    fn tokenize_with_budget(
        &self,
        messages: &[(String, String)],
        max_prompt_tokens: usize,
    ) -> SoulResult<Vec<llama_cpp_2::token::LlamaToken>> {
        let vocab = self.model.vocab();
        let mut window = messages.to_vec();

        // Trim oldest history turns (preserving index 0 system prompt and final user turn)
        // until the formatted prompt fits comfortably within the context window.
        loop {
            let prompt = self.format_prompt(&window);
            let tokens = vocab.tokenize(prompt.as_bytes(), true, true);
            if tokens.len() <= max_prompt_tokens || window.len() <= 2 {
                if tokens.len() <= max_prompt_tokens {
                    return Ok(tokens);
                }
                let head_keep = (max_prompt_tokens / 2).min(tokens.len());
                let tail_keep = max_prompt_tokens.saturating_sub(head_keep);
                let mut trimmed = Vec::with_capacity(max_prompt_tokens);
                trimmed.extend_from_slice(&tokens[..head_keep]);
                if tail_keep > 0 && tokens.len() > head_keep {
                    let start = tokens.len() - tail_keep.min(tokens.len() - head_keep);
                    trimmed.extend_from_slice(&tokens[start..]);
                }
                return Ok(trimmed);
            }
            window.remove(1);
        }
    }
}

fn flush_valid_utf8(pending: &mut Vec<u8>, on_piece: &mut dyn FnMut(&str)) {
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                if !valid.is_empty() {
                    on_piece(valid);
                }
                pending.clear();
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0 {
                    let valid = std::str::from_utf8(&pending[..valid_up_to])
                        .expect("valid_up_to guarantees valid UTF-8");
                    on_piece(valid);
                    pending.drain(..valid_up_to);
                }
                match err.error_len() {
                    Some(invalid_len) => {
                        on_piece("\u{FFFD}");
                        pending.drain(..invalid_len);
                    }
                    None => break,
                }
            }
        }
    }
}
