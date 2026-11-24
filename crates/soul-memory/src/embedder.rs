use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ndarray::Array2;
use ort::session::Session;
use ort::value::TensorRef;
use shared_types::{SoulError, SoulResult};
use tokenizers::Tokenizer;

/// Fixed embedding dimensionality used by `paraphrase-multilingual-MiniLM-L12-v2` and `vec0` tables.
pub const EMBEDDING_DIM: usize = 384;

const MAX_SEQUENCE_TOKENS: usize = 512;

static GLOBAL_ORT_ENV: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();

/// Validates a user-provided `.onnx` embedding model file and its required sibling `tokenizer.json`.
pub fn validate_onnx_embed_file(onnx_path: &Path) -> SoulResult<()> {
    if !onnx_path.exists() || !onnx_path.is_file() {
        return Err(SoulError::ModelNotFound(format!(
            "ONNX embedding file not found: {}",
            onnx_path.display()
        )));
    }

    let is_onnx_ext = onnx_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"));
    if !is_onnx_ext {
        return Err(SoulError::InvalidInput(format!(
            "Embedding model file must have .onnx extension: {}",
            onnx_path.display()
        )));
    }

    let size_bytes = std::fs::metadata(onnx_path)?.len();
    if size_bytes == 0 {
        return Err(SoulError::InvalidInput(format!(
            "ONNX embedding file is empty: {}",
            onnx_path.display()
        )));
    }

    let mut file = File::open(onnx_path)?;
    let mut header = [0u8; 8];
    let n = file.read(&mut header)?;
    if n == 0 || !is_valid_onnx_protobuf_start(header[0]) {
        return Err(SoulError::InvalidInput(format!(
            "File does not appear to be a valid ONNX protobuf model: {}",
            onnx_path.display()
        )));
    }

    let parent = onnx_path.parent().ok_or_else(|| {
        SoulError::InvalidInput(format!(
            "Cannot resolve parent directory for ONNX model: {}",
            onnx_path.display()
        ))
    })?;
    let tokenizer_path = parent.join("tokenizer.json");
    if !tokenizer_path.is_file() {
        return Err(SoulError::ModelNotFound(format!(
            "Sibling tokenizer.json not found in {}: an ONNX embedding model requires tokenizer.json alongside the .onnx file",
            parent.display()
        )));
    }

    Ok(())
}

/// ONNX `ModelProto` root message protobuf field tags (wire types varint=0 or length-delimited=2).
fn is_valid_onnx_protobuf_start(first_byte: u8) -> bool {
    matches!(
        first_byte,
        0x08 | 0x12 | 0x1A | 0x22 | 0x2A | 0x30 | 0x3A | 0x42 | 0x72
    )
}

/// Local embedding backend supporting offline ONNX MiniLM inference and deterministic fallback.
pub struct Embedder {
    backend: EmbedderBackend,
}

enum EmbedderBackend {
    Onnx(OnnxState),
    CharNgramFallback,
}

struct OnnxState {
    model_path: PathBuf,
    tokenizer: Tokenizer,
    session: Mutex<Session>,
    has_token_type_ids: bool,
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedder")
            .field("kind", &self.kind())
            .field("model_path", &self.model_path())
            .finish()
    }
}

impl Embedder {
    /// Initialize from an optional model directory containing `model_quantized.onnx` and `tokenizer.json`.
    ///
    /// Falls back cleanly to a deterministic 384-dim char-ngram unit-vector embedder when
    /// `embed_dir` is `None`, the ONNX files are absent, or no ONNX Runtime dylib is available.
    pub fn from_dir(embed_dir: Option<&Path>) -> SoulResult<Self> {
        let Some(dir) = embed_dir else {
            return Ok(Self {
                backend: EmbedderBackend::CharNgramFallback,
            });
        };

        let model_path = dir.join("model_quantized.onnx");
        let tokenizer_path = dir.join("tokenizer.json");
        if !model_path.is_file() || !tokenizer_path.is_file() {
            tracing::debug!(
                embed_dir = %dir.display(),
                "ONNX embedding assets missing; using deterministic 384-dim char-ngram fallback"
            );
            return Ok(Self {
                backend: EmbedderBackend::CharNgramFallback,
            });
        }

        match Self::try_load_onnx(dir, &model_path, &tokenizer_path) {
            Ok(state) => {
                tracing::info!(
                    model = %model_path.display(),
                    "Loaded local ONNX embedding model"
                );
                Ok(Self {
                    backend: EmbedderBackend::Onnx(state),
                })
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    model = %model_path.display(),
                    "Failed to initialize ONNX Runtime embedder; falling back to char-ngram embeddings"
                );
                Ok(Self {
                    backend: EmbedderBackend::CharNgramFallback,
                })
            }
        }
    }

    /// Strictly validate and load an ONNX embedding model from `onnx_path` and its sibling
    /// `tokenizer.json`. Unlike [`Self::from_dir`], errors are returned directly to the caller.
    pub fn from_onnx_file(onnx_path: &Path) -> SoulResult<Self> {
        validate_onnx_embed_file(onnx_path)?;
        let embed_dir = onnx_path.parent().unwrap_or_else(|| Path::new("."));
        let tokenizer_path = embed_dir.join("tokenizer.json");
        let state = Self::try_load_onnx(embed_dir, onnx_path, &tokenizer_path)?;
        tracing::info!(
            model = %state.model_path.display(),
            "Hot-loaded user ONNX embedding model"
        );
        Ok(Self {
            backend: EmbedderBackend::Onnx(state),
        })
    }

    /// Return `"onnx"` when backed by a loaded ONNX session, or `"ngram-fallback"` otherwise.
    pub fn kind(&self) -> &'static str {
        match self.backend {
            EmbedderBackend::Onnx(_) => "onnx",
            EmbedderBackend::CharNgramFallback => "ngram-fallback",
        }
    }

    /// Return the active ONNX model file path if an ONNX embedder is loaded, or `None` in fallback mode.
    pub fn model_path(&self) -> Option<&Path> {
        match &self.backend {
            EmbedderBackend::Onnx(state) => Some(state.model_path.as_path()),
            EmbedderBackend::CharNgramFallback => None,
        }
    }

    /// Compute an L2-normalized 384-dimensional embedding for `text`.
    pub fn embed(&self, text: &str) -> SoulResult<Vec<f32>> {
        match &self.backend {
            EmbedderBackend::Onnx(state) => Self::embed_onnx(state, text),
            EmbedderBackend::CharNgramFallback => Ok(char_ngram_embed(text)),
        }
    }

    fn try_load_onnx(
        embed_dir: &Path,
        model_path: &Path,
        tokenizer_path: &Path,
    ) -> SoulResult<OnnxState> {
        let dylib_path = find_onnxruntime_dylib(embed_dir).ok_or_else(|| {
            SoulError::Memory(
                "libonnxruntime dynamic library not found (set ORT_DYLIB_PATH or place libonnxruntime.dylib in embed_dir or runtime/)".into(),
            )
        })?;

        let env_res = GLOBAL_ORT_ENV.get_or_init(|| {
            ort::init_from(&dylib_path)
                .map_err(|e| format!("ort::init_from({}) failed: {e}", dylib_path.display()))
                .and_then(|builder| {
                    builder
                        .with_name("soulmate2d-embed")
                        .with_telemetry(false)
                        .build()
                        .map_err(|e| format!("ort environment build failed: {e}"))
                })
        });

        let env = match env_res {
            Ok(env) => env,
            Err(msg) => return Err(SoulError::Memory(msg.clone())),
        };

        let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| {
            SoulError::Memory(format!(
                "Failed to load tokenizer {}: {e}",
                tokenizer_path.display()
            ))
        })?;

        let session = Session::builder(env)
            .map_err(|e| SoulError::Memory(format!("Session::builder failed: {e}")))?
            .with_intra_threads(2)
            .map_err(|e| SoulError::Memory(format!("with_intra_threads failed: {e}")))?
            .commit_from_file(model_path)
            .map_err(|e| {
                SoulError::Memory(format!(
                    "Failed to load ONNX model {}: {e}",
                    model_path.display()
                ))
            })?;

        let has_token_type_ids = session
            .inputs()
            .iter()
            .any(|outlet| outlet.name() == "token_type_ids");

        let canonical_model = std::fs::canonicalize(model_path).unwrap_or_else(|_| model_path.to_path_buf());
        Ok(OnnxState {
            model_path: canonical_model,
            tokenizer,
            session: Mutex::new(session),
            has_token_type_ids,
        })
    }

    fn embed_onnx(state: &OnnxState, text: &str) -> SoulResult<Vec<f32>> {
        let cleaned = if text.trim().is_empty() { " " } else { text };
        let encoding = state
            .tokenizer
            .encode(cleaned, true)
            .map_err(|e| SoulError::Memory(format!("Tokenizer encode failed: {e}")))?;

        let raw_ids = encoding.get_ids();
        let raw_mask = encoding.get_attention_mask();
        let raw_types = encoding.get_type_ids();

        let seq_len = raw_ids.len().min(MAX_SEQUENCE_TOKENS).max(1);
        let mut ids = Vec::with_capacity(seq_len);
        let mut mask = Vec::with_capacity(seq_len);
        let mut types = Vec::with_capacity(seq_len);

        if raw_ids.is_empty() {
            ids.push(0_i64);
            mask.push(1_i64);
            types.push(0_i64);
        } else {
            for i in 0..seq_len {
                ids.push(raw_ids[i] as i64);
                mask.push(*raw_mask.get(i).unwrap_or(&1) as i64);
                types.push(*raw_types.get(i).unwrap_or(&0) as i64);
            }
        }

        let ids_arr = Array2::from_shape_vec((1, seq_len), ids)
            .map_err(|e| SoulError::Memory(format!("Failed to build input_ids tensor: {e}")))?;
        let mask_arr = Array2::from_shape_vec((1, seq_len), mask.clone())
            .map_err(|e| SoulError::Memory(format!("Failed to build attention_mask tensor: {e}")))?;
        let types_arr = Array2::from_shape_vec((1, seq_len), types)
            .map_err(|e| SoulError::Memory(format!("Failed to build token_type_ids tensor: {e}")))?;

        let mut inputs = ort::inputs![
            "input_ids" => TensorRef::from_array_view(ids_arr.view())
                .map_err(|e| SoulError::Memory(format!("TensorRef input_ids error: {e}")))?,
            "attention_mask" => TensorRef::from_array_view(mask_arr.view())
                .map_err(|e| SoulError::Memory(format!("TensorRef attention_mask error: {e}")))?,
        ];

        if state.has_token_type_ids {
            inputs.push((
                "token_type_ids".into(),
                TensorRef::from_array_view(types_arr.view())
                    .map_err(|e| SoulError::Memory(format!("TensorRef token_type_ids error: {e}")))?
                    .into(),
            ));
        }

        let mut session = state
            .session
            .lock()
            .map_err(|_| SoulError::Internal("ONNX session mutex poisoned".into()))?;
        let outputs = session
            .run(inputs)
            .map_err(|e| SoulError::Memory(format!("ONNX inference failed: {e}")))?;

        if outputs.len() == 0 {
            return Err(SoulError::Memory("ONNX model returned zero outputs".into()));
        }

        let output_val = if outputs.contains_key("last_hidden_state") {
            &outputs["last_hidden_state"]
        } else if outputs.contains_key("sentence_embedding") {
            &outputs["sentence_embedding"]
        } else {
            &outputs[0]
        };

        let extracted = output_val
            .try_extract_array::<f32>()
            .map_err(|e| SoulError::Memory(format!("Failed to extract f32 ONNX output: {e}")))?;
        let shape = extracted.shape();

        let mut pooled = vec![0.0_f32; EMBEDDING_DIM];
        match shape.len() {
            3 => {
                let out_seq = shape[1].min(seq_len);
                let hidden = shape[2].min(EMBEDDING_DIM);
                let mut mask_sum = 0.0_f32;
                for t in 0..out_seq {
                    let m = if mask[t] > 0 { 1.0_f32 } else { 0.0_f32 };
                    if m > 0.0 {
                        mask_sum += m;
                        for d in 0..hidden {
                            pooled[d] += extracted[[0, t, d]] * m;
                        }
                    }
                }
                let denom = mask_sum.max(1e-9);
                for val in pooled.iter_mut().take(hidden) {
                    *val /= denom;
                }
            }
            2 => {
                let hidden = shape[1].min(EMBEDDING_DIM);
                for d in 0..hidden {
                    pooled[d] = extracted[[0, d]];
                }
            }
            other => {
                return Err(SoulError::Memory(format!(
                    "Unexpected ONNX embedding output rank {other} (shape {shape:?})"
                )));
            }
        }

        if !normalize_embedding(&mut pooled) {
            return Ok(char_ngram_embed(text));
        }

        Ok(pooled)
    }
}

/// Deterministic 384-dim signed char-ngram + token feature-hashed unit vector (supports Thai & English).
pub fn char_ngram_embed(text: &str) -> Vec<f32> {
    let mut vec = vec![0.0_f32; EMBEDDING_DIM];
    let normalized: String = text
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if normalized.is_empty() {
        vec[0] = 1.0;
        return vec;
    }

    let chars: Vec<char> = normalized.chars().collect();

    // Character unigrams, bigrams, and trigrams capture Thai unspaced morphemes and English subwords.
    for (idx, &ch) in chars.iter().enumerate() {
        if !ch.is_whitespace() {
            add_hashed_feature(&mut vec, 1, &[ch as u32], 0.5);
        }
        if idx + 1 < chars.len() {
            add_hashed_feature(&mut vec, 2, &[chars[idx] as u32, chars[idx + 1] as u32], 1.0);
        }
        if idx + 2 < chars.len() {
            add_hashed_feature(
                &mut vec,
                3,
                &[chars[idx] as u32, chars[idx + 1] as u32, chars[idx + 2] as u32],
                1.5,
            );
        }
        if idx + 3 < chars.len() {
            add_hashed_feature(
                &mut vec,
                4,
                &[
                    chars[idx] as u32,
                    chars[idx + 1] as u32,
                    chars[idx + 2] as u32,
                    chars[idx + 3] as u32,
                ],
                1.25,
            );
        }
    }

    // Whole whitespace/punctuation-delimited word tokens give strong exact-word alignment.
    for token in normalized.split(|c: char| !c.is_alphanumeric() && !('\u{0E00}'..='\u{0E7F}').contains(&c)) {
        if token.chars().count() >= 2 {
            let mut codepoints = [0_u32; 32];
            let mut count = 0;
            for ch in token.chars().take(32) {
                codepoints[count] = ch as u32;
                count += 1;
            }
            add_hashed_feature(&mut vec, 5, &codepoints[..count], 2.5);
        }
    }

    if !normalize_embedding(&mut vec) {
        vec[0] = 1.0;
    }
    vec
}

fn add_hashed_feature(vec: &mut [f32], namespace: u64, codepoints: &[u32], weight: f32) {
    let mut hash: u64 = 0xcbf29ce484222325 ^ namespace.wrapping_mul(0x9e3779b97f4a7c15);
    for &cp in codepoints {
        for byte in cp.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    let idx = (hash as usize) % EMBEDDING_DIM;
    let sign = if ((hash >> 32) & 1) == 0 { 1.0 } else { -1.0 };
    vec[idx] += sign * weight;
}

/// Normalize a vector in place to L2 unit length. Returns `false` if zero-norm or non-finite.
pub fn normalize_embedding(vec: &mut [f32]) -> bool {
    if vec.is_empty() || !vec.iter().all(|v| v.is_finite()) {
        return false;
    }
    let norm_sq: f64 = vec.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
    let norm = norm_sq.sqrt();
    if norm <= 1e-12 || !norm.is_finite() {
        return false;
    }
    let inv = (1.0 / norm) as f32;
    for v in vec.iter_mut() {
        *v *= inv;
    }
    true
}

/// Convert a `&[f32]` embedding slice into SQLite `vec0` little-endian byte representation.
pub fn embedding_to_le_bytes(vec: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for &v in vec {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn find_onnxruntime_dylib(embed_dir: &Path) -> Option<PathBuf> {
    if let Some(env_path) = std::env::var_os("ORT_DYLIB_PATH") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Some(found) = find_dylib_in_dir(embed_dir) {
        return Some(found);
    }

    for runtime_dir in [
        PathBuf::from("src-tauri/resources/runtime"),
        PathBuf::from("resources/runtime"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/resources/runtime"),
    ] {
        if let Some(found) = find_dylib_in_dir(&runtime_dir) {
            return Some(found);
        }
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            for candidate_dir in [
                exe_dir.join("../Resources/resources/runtime"),
                exe_dir.to_path_buf(),
                exe_dir.join("../Frameworks"),
                exe_dir.join("../Resources"),
                exe_dir.join("../Resources/models/embed"),
            ] {
                if let Some(found) = find_dylib_in_dir(&candidate_dir) {
                    return Some(found);
                }
            }
        }
    }

    for sys_dir in ["/opt/homebrew/lib", "/usr/local/lib"] {
        if let Some(found) = find_dylib_in_dir(Path::new(sys_dir)) {
            return Some(found);
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let ort_cache = PathBuf::from(home).join("Library/Caches/ort.pyke.io");
        if ort_cache.is_dir() {
            if let Some(found) = find_dylib_recursive(&ort_cache, 5) {
                return Some(found);
            }
        }
    }

    None
}

fn find_dylib_in_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let exact = dir.join("libonnxruntime.dylib");
    if exact.is_file() {
        return Some(exact);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut matches: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("libonnxruntime") && n.ends_with(".dylib"))
        })
        .collect();
    matches.sort();
    matches.pop()
}

fn find_dylib_recursive(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth == 0 || !dir.is_dir() {
        return None;
    }
    if let Some(found) = find_dylib_in_dir(dir) {
        return Some(found);
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_dylib_recursive(&p, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}
