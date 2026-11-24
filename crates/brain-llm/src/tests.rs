use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use shared_types::{
    BrainEngine, ChatRole, ChatTurn, DreamDiary, EmotionState, EmotionTag, EpisodeRecord,
    LanguagePref, MemoryFact, MemoryRetrievalContext, ModelInfo, ModelKind, ModelSource,
    RamSafetyTier, RelationshipState, SoulError, SoulResult,
};

use crate::backend::{LoadedModelInstance, ModelBackend};
use crate::manager::LlmBrainManager;
use crate::parser::EmotionStreamParser;
use crate::prompt::{ResolvedLanguage, build_chat_messages, build_soul_system_prompt, resolve_reply_language};
use crate::ram_guard::{
    AdaptiveInferenceParams, RamProbe, RamSnapshot, adaptive_params_for_ram,
    classify_ram_safety_tier, estimate_gguf_ram_mb,
};

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{prefix}_{}", shared_types::new_id()));
        fs::create_dir_all(&dir).expect("create temp test dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_dummy_gguf(path: &Path, magic: &[u8], total_bytes: usize) {
    let mut data = vec![0u8; total_bytes.max(magic.len())];
    data[..magic.len()].copy_from_slice(magic);
    fs::write(path, data).expect("write dummy gguf");
}

struct MockRamProbe {
    snapshot: StdMutex<RamSnapshot>,
}

impl MockRamProbe {
    fn new(total_ram_mb: u64, available_ram_mb: u64, process_ram_mb: u64) -> Self {
        Self {
            snapshot: StdMutex::new(RamSnapshot {
                total_ram_mb,
                available_ram_mb,
                process_ram_mb,
            }),
        }
    }
}

impl RamProbe for MockRamProbe {
    fn snapshot(&self) -> RamSnapshot {
        *self.snapshot.lock().expect("mock ram snapshot lock")
    }
}

struct MockBackend {
    events: Arc<StdMutex<Vec<String>>>,
    active_count: Arc<AtomicUsize>,
    peak_active: Arc<AtomicUsize>,
    scripted_chunks: Vec<String>,
}

impl MockBackend {
    fn new(scripted_chunks: Vec<&str>) -> Self {
        Self {
            events: Arc::new(StdMutex::new(Vec::new())),
            active_count: Arc::new(AtomicUsize::new(0)),
            peak_active: Arc::new(AtomicUsize::new(0)),
            scripted_chunks: scripted_chunks.into_iter().map(ToString::to_string).collect(),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events lock").clone()
    }

    fn peak_active(&self) -> usize {
        self.peak_active.load(Ordering::SeqCst)
    }
}

struct MockLoadedModel {
    info: ModelInfo,
    events: Arc<StdMutex<Vec<String>>>,
    active_count: Arc<AtomicUsize>,
    scripted_chunks: Vec<String>,
}

impl Drop for MockLoadedModel {
    fn drop(&mut self) {
        self.active_count.fetch_sub(1, Ordering::SeqCst);
        self.events
            .lock()
            .expect("events lock")
            .push(format!("drop:{}", self.info.id));
    }
}

impl LoadedModelInstance for MockLoadedModel {
    fn model_info(&self) -> &ModelInfo {
        &self.info
    }

    fn generate(
        &self,
        _messages: &[(String, String)],
        _max_tokens: u32,
        on_piece: &mut dyn FnMut(&str),
    ) -> SoulResult<()> {
        for chunk in &self.scripted_chunks {
            on_piece(chunk);
        }
        Ok(())
    }
}

impl ModelBackend for MockBackend {
    fn load(
        &self,
        model: &ModelInfo,
        _params: AdaptiveInferenceParams,
    ) -> SoulResult<Box<dyn LoadedModelInstance>> {
        let now_active = self.active_count.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.peak_active.fetch_max(now_active, Ordering::SeqCst);
        self.events
            .lock()
            .expect("events lock")
            .push(format!("load:{}", model.id));

        Ok(Box::new(MockLoadedModel {
            info: model.clone(),
            events: Arc::clone(&self.events),
            active_count: Arc::clone(&self.active_count),
            scripted_chunks: self.scripted_chunks.clone(),
        }))
    }
}

#[test]
fn emotion_stream_parser_parses_all_seven_emotions_and_strips_tag() {
    let cases = [
        ("joy", EmotionTag::Joy, 0.91_f32),
        ("sadness", EmotionTag::Sadness, 0.42),
        ("shy", EmotionTag::Shy, 0.77),
        ("surprised", EmotionTag::Surprised, 0.88),
        ("thinking", EmotionTag::Thinking, 0.53),
        ("empathetic", EmotionTag::Empathetic, 0.95),
        ("neutral", EmotionTag::Neutral, 0.30),
    ];

    for (raw_tag, expected_tag, expected_intensity) in cases {
        let trace = Arc::new(StdMutex::new(Vec::<String>::new()));
        let captured_emotion = Arc::new(StdMutex::new(None::<EmotionState>));

        let t1 = Arc::clone(&trace);
        let ce = Arc::clone(&captured_emotion);
        let on_emotion = Box::new(move |state: EmotionState| {
            t1.lock().unwrap().push("emotion".into());
            *ce.lock().unwrap() = Some(state);
        });

        let t2 = Arc::clone(&trace);
        let streamed_tokens = Arc::new(StdMutex::new(Vec::<String>::new()));
        let st = Arc::clone(&streamed_tokens);
        let on_token = Box::new(move |chunk: shared_types::TokenChunk| {
            if !chunk.is_final {
                t2.lock().unwrap().push(format!("token:{}", chunk.text));
                st.lock().unwrap().push(chunk.text);
            }
        });

        let mut parser = EmotionStreamParser::new(on_emotion, on_token);
        let input = format!("[emotion: {raw_tag}, intensity: {expected_intensity:.2}] สวัสดีค่ะ!");
        parser.push(&input);
        let (final_emotion, full_text) = parser.finish();

        assert_eq!(final_emotion.tag, expected_tag);
        assert!((final_emotion.intensity - expected_intensity).abs() < 1e-4);
        assert_eq!(full_text, "สวัสดีค่ะ!");
        assert!(!full_text.contains("[emotion:"));

        let events = trace.lock().unwrap().clone();
        assert_eq!(events.first().map(String::as_str), Some("emotion"));
        assert_eq!(streamed_tokens.lock().unwrap().concat(), "สวัสดีค่ะ!");
    }
}

#[test]
fn emotion_stream_parser_handles_split_chunks_and_split_think_blocks() {
    let trace = Arc::new(StdMutex::new(Vec::<String>::new()));
    let t1 = Arc::clone(&trace);
    let t2 = Arc::clone(&trace);

    let mut parser = EmotionStreamParser::new(
        Box::new(move |emo| {
            t1.lock()
                .unwrap()
                .push(format!("emotion:{}:{:.2}", emo.tag, emo.intensity));
        }),
        Box::new(move |chunk| {
            t2.lock()
                .unwrap()
                .push(format!("token:{}:final={}", chunk.text, chunk.is_final));
        }),
    );

    let split_tokens = [
        "<th",
        "ink>internal reasoning",
        " that must not leak</th",
        "ink>\n\n[emo",
        "tion: s",
        "hy, inten",
        "sity: 0.8",
        "4]   ",
        "เอ่อ... ",
        "<TH",
        "INK>nested ",
        "thoughts</THI",
        "NK>",
        "ขอบคุณนะคะ",
        "<|im_end|>",
    ];

    for piece in split_tokens {
        parser.push(piece);
    }

    let (emotion, full_text) = parser.finish();
    assert_eq!(emotion.tag, EmotionTag::Shy);
    assert!((emotion.intensity - 0.84).abs() < 1e-4);
    assert_eq!(full_text, "เอ่อ... ขอบคุณนะคะ");
    assert!(!full_text.contains("internal reasoning"));
    assert!(!full_text.contains("nested"));
    assert!(!full_text.contains("<|im_end|>"));

    let events = trace.lock().unwrap().clone();
    assert_eq!(events[0], "emotion:shy:0.84");
    assert!(events[1].starts_with("token:เอ่อ... "));
}

#[test]
fn emotion_stream_parser_malformed_and_missing_fallback() {
    // 1. Completely missing emotion prefix -> Neutral fallback, text intact.
    let got_emotion = Arc::new(StdMutex::new(None));
    let ge = Arc::clone(&got_emotion);
    let mut p1 = EmotionStreamParser::new(
        Box::new(move |e| *ge.lock().unwrap() = Some(e)),
        Box::new(|_| {}),
    );
    p1.push("Hello there, friend!");
    let (e1, text1) = p1.finish();
    assert_eq!(e1.tag, EmotionTag::Neutral);
    assert_eq!(text1, "Hello there, friend!");

    // 2. Out-of-bounds intensity + unknown emotion tag -> clamped & Neutral fallback.
    let mut p2 = EmotionStreamParser::new(Box::new(|_| {}), Box::new(|_| {}));
    p2.push("[emotion: hyped, intensity: 4.5] Let's go!");
    let (e2, text2) = p2.finish();
    assert_eq!(e2.tag, EmotionTag::Neutral);
    assert_eq!(e2.intensity, 1.0);
    assert_eq!(text2, "Let's go!");

    // 3. Unclosed <think> at EOF is discarded instead of leaked.
    let mut p3 = EmotionStreamParser::new(Box::new(|_| {}), Box::new(|_| {}));
    p3.push("[emotion: joy, intensity: 0.7] สวัสดี<think>unclosed hidden chain");
    let (e3, text3) = p3.finish();
    assert_eq!(e3.tag, EmotionTag::Joy);
    assert_eq!(text3, "สวัสดี");

    // 4. Short non-emotion bracket prefix is preserved as spoken text with Neutral fallback.
    let mut p4 = EmotionStreamParser::new(Box::new(|_| {}), Box::new(|_| {}));
    p4.push("[hi] good to see you");
    let (e4, text4) = p4.finish();
    assert_eq!(e4.tag, EmotionTag::Neutral);
    assert_eq!(text4, "[hi] good to see you");

    // 5. Unclosed emotion tag at EOF recovers emotion and trims tag.
    let mut p5 = EmotionStreamParser::new(Box::new(|_| {}), Box::new(|_| {}));
    p5.push("[emotion: surprised, intensity: 0.75");
    let (e5, text5) = p5.finish();
    assert_eq!(e5.tag, EmotionTag::Surprised);
    assert!((e5.intensity - 0.75).abs() < 1e-4);
    assert_eq!(text5, "");
}

#[test]
fn bilingual_soul_prompt_with_memory_injection() {
    let memory_ctx = MemoryRetrievalContext {
        facts: vec![
            MemoryFact {
                id: "f1".into(),
                category: "preference".into(),
                key: "favorite_drink".into(),
                value: "iced matcha".into(),
                confidence: 0.95,
                importance: 0.8,
                memory_heat: 0.9,
                access_count: 3,
                created_at: 1000,
                last_accessed_at: 2000,
                superseded_by: None,
                superseded_at: None,
            },
            MemoryFact {
                id: "f2".into(),
                category: "city".into(),
                key: "home".into(),
                value: "Old City".into(),
                confidence: 0.9,
                importance: 0.7,
                memory_heat: 0.2,
                access_count: 1,
                created_at: 500,
                last_accessed_at: 600,
                superseded_by: Some("f3".into()),
                superseded_at: Some(900),
            },
        ],
        episodes: vec![EpisodeRecord {
            id: "ep1".into(),
            summary: "Watched rain together while coding Rust".into(),
            raw_turn_count: 6,
            emotional_valence: 0.75,
            memory_heat: 0.8,
            created_at: 1500,
            last_accessed_at: 2500,
        }],
        recent_diaries: vec![DreamDiary {
            id: "dd1".into(),
            period_type: "daily".into(),
            period_key: "2026-11-24".into(),
            summary: "Felt grateful for our late night chat".into(),
            emotional_tone: "warm".into(),
            bond_delta: 0.05,
            created_at: 3000,
        }],
        relationship: RelationshipState {
            trust: 0.82,
            warmth: 0.88,
            familiarity: 0.65,
            mood_baseline: 0.72,
            total_turns: 42,
            last_interaction_at: 3000,
        },
    };

    assert_eq!(
        resolve_reply_language("วันนี้เหนื่อยจังเลย", LanguagePref::Auto),
        ResolvedLanguage::Thai
    );
    assert_eq!(
        resolve_reply_language("Good morning Mina!", LanguagePref::Auto),
        ResolvedLanguage::English
    );
    assert_eq!(
        resolve_reply_language("Good morning Mina!", LanguagePref::Thai),
        ResolvedLanguage::Thai
    );

    let thai_prompt = build_soul_system_prompt("วันนี้เหนื่อยจัง", &memory_ctx, LanguagePref::Auto);
    assert!(thai_prompt.contains("/no_think"));
    assert!(thai_prompt.contains("Mina (มีนา)"));
    assert!(thai_prompt.contains("[emotion: <tag>, intensity: <0.0..1.0>]"));
    assert!(thai_prompt.contains("iced matcha"));
    assert!(!thai_prompt.contains("Old City")); // Superseded fact excluded!
    assert!(thai_prompt.contains("Watched rain together while coding Rust"));
    assert!(thai_prompt.contains("Felt grateful for our late night chat"));
    assert!(thai_prompt.contains("trust: 0.82"));
    assert!(thai_prompt.contains("ภาษาไทย"));

    let en_prompt = build_soul_system_prompt("Hey Mina!", &memory_ctx, LanguagePref::English);
    assert!(en_prompt.contains("conversational English"));
    assert!(en_prompt.contains("/no_think"));

    // Deduplicate trailing user turn when already persisted into history by caller.
    let history = vec![
        ChatTurn {
            id: "1".into(),
            role: ChatRole::User,
            content: "Hi".into(),
            emotion: None,
            timestamp_ms: 1,
        },
        ChatTurn {
            id: "2".into(),
            role: ChatRole::Assistant,
            content: "Hey!".into(),
            emotion: Some(EmotionTag::Joy),
            timestamp_ms: 2,
        },
        ChatTurn {
            id: "3".into(),
            role: ChatRole::User,
            content: "How are you?".into(),
            emotion: None,
            timestamp_ms: 3,
        },
    ];
    let msgs = build_chat_messages("SYS", &history, "How are you?", 10);
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[3], ("user".into(), "How are you?".into()));
}

#[test]
fn ram_safety_tier_and_adaptive_tuning() {
    assert_eq!(classify_ram_safety_tier(2800), RamSafetyTier::Safe8Gb);
    assert_eq!(classify_ram_safety_tier(3200), RamSafetyTier::Safe8Gb);
    assert_eq!(classify_ram_safety_tier(3201), RamSafetyTier::Tight8Gb);
    assert_eq!(classify_ram_safety_tier(4500), RamSafetyTier::Tight8Gb);
    assert_eq!(
        classify_ram_safety_tier(4501),
        RamSafetyTier::Recommended16GbPlus
    );

    let p8 = adaptive_params_for_ram(8192);
    assert!(p8.is_8gb_class);
    assert_eq!(p8.budget_ceiling_mb, 4800);
    assert_eq!(p8.llm_ceiling_mb, Some(3800));
    assert_eq!(p8.n_ctx, 2048);
    assert_eq!(p8.n_batch, 256);

    let p16 = adaptive_params_for_ram(16384);
    assert!(!p16.is_8gb_class);
    assert_eq!(p16.budget_ceiling_mb, 11468);
    assert_eq!(p16.llm_ceiling_mb, None);
    assert_eq!(p16.n_ctx, 4096);
    assert_eq!(p16.n_batch, 512);

    let est_typhoon_4b = estimate_gguf_ram_mb(2_497_276_128, p8.n_ctx);
    assert_eq!(est_typhoon_4b, 3177);
    assert_eq!(classify_ram_safety_tier(est_typhoon_4b), RamSafetyTier::Safe8Gb);

    let est_tight = estimate_gguf_ram_mb(2600 * 1024 * 1024, p8.n_ctx);
    assert!(est_tight < 3800);
    assert_eq!(classify_ram_safety_tier(est_tight), RamSafetyTier::Tight8Gb);
}

#[test]
fn dual_folder_scanner_validates_gguf_magic_and_custom_registration() {
    let root = TempDirGuard::new("brain_llm_scan");
    let bundled = root.path().join("bundled");
    let user = root.path().join("user");
    let custom_dir = root.path().join("custom");
    fs::create_dir_all(&bundled).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::create_dir_all(&custom_dir).unwrap();

    // Non-GGUF helper files should be ignored completely.
    fs::write(bundled.join(".gitkeep"), b"").unwrap();
    fs::write(
        bundled.join("manifest.json"),
        br#"{"filename": "typhoon2.5.gguf", "id": "typhoon-4b", "name": "Typhoon 2.5 4B"}"#,
    )
    .unwrap();

    let bundled_gguf = bundled.join("typhoon2.5.gguf");
    write_dummy_gguf(&bundled_gguf, b"GGUF", 1024 * 1024);

    let bad_gguf = user.join("corrupt.gguf");
    write_dummy_gguf(&bad_gguf, b"NOPE", 1024 * 1024);

    let custom_good = custom_dir.join("qwen-custom.gguf");
    write_dummy_gguf(&custom_good, b"GGUF", 2 * 1024 * 1024);

    let custom_bad = custom_dir.join("bad-custom.gguf");
    write_dummy_gguf(&custom_bad, b"HTML", 512);

    let manager = LlmBrainManager::with_components(
        bundled,
        user,
        Arc::new(MockRamProbe::new(8192, 4200, 200)),
        Arc::new(MockBackend::new(vec![])),
    );

    // Registering invalid GGUF custom model must fail cleanly.
    let err = manager
        .register_custom_model(&custom_bad, None)
        .expect_err("corrupt custom GGUF should fail");
    assert!(matches!(err, SoulError::InvalidInput(_)));

    let registered = manager
        .register_custom_model(&custom_good, Some("My Qwen Custom"))
        .expect("valid custom GGUF registers");
    assert_eq!(registered.source, ModelSource::Custom);
    assert_eq!(registered.name, "My Qwen Custom");
    assert!(registered.valid);

    let models = manager.scan_models().expect("scan models");
    assert_eq!(models.len(), 3);

    let bundled_found = models
        .iter()
        .find(|m| m.source == ModelSource::Bundled)
        .expect("bundled model found");
    assert_eq!(bundled_found.id, "typhoon-4b");
    assert_eq!(bundled_found.name, "Typhoon 2.5 4B");
    assert!(bundled_found.valid);
    assert_eq!(bundled_found.safety_tier, RamSafetyTier::Safe8Gb);

    let user_corrupt = models
        .iter()
        .find(|m| m.source == ModelSource::UserData)
        .expect("corrupt model surfaced as invalid");
    assert!(!user_corrupt.valid);
    assert!(user_corrupt.notes.is_some());

    let custom_found = models
        .iter()
        .find(|m| m.source == ModelSource::Custom)
        .expect("custom model found");
    assert_eq!(custom_found.name, "My Qwen Custom");
    assert!(custom_found.valid);
}

#[tokio::test]
async fn zero_leak_hot_swap_strictly_drops_before_load_and_enforces_ram_ceiling() {
    let root = TempDirGuard::new("brain_llm_hotswap");
    let model_a_path = root.path().join("model_a.gguf");
    let model_b_path = root.path().join("model_b.gguf");
    let oversized_path = root.path().join("oversized.gguf");

    write_dummy_gguf(&model_a_path, b"GGUF", 1024 * 1024);
    write_dummy_gguf(&model_b_path, b"GGUF", 1024 * 1024);
    write_dummy_gguf(&oversized_path, b"GGUF", 1024 * 1024);

    let model_a = ModelInfo {
        id: "model-a".into(),
        name: "Model A".into(),
        kind: ModelKind::Llm,
        path: model_a_path.to_string_lossy().into_owned(),
        size_bytes: 1024 * 1024,
        estimated_ram_mb: 2600,
        source: ModelSource::Bundled,
        safety_tier: RamSafetyTier::Safe8Gb,
        is_active: false,
        valid: true,
        notes: None,
    };

    let model_b = ModelInfo {
        id: "model-b".into(),
        name: "Model B".into(),
        kind: ModelKind::Llm,
        path: model_b_path.to_string_lossy().into_owned(),
        size_bytes: 1024 * 1024,
        estimated_ram_mb: 2900,
        source: ModelSource::UserData,
        safety_tier: RamSafetyTier::Safe8Gb,
        is_active: false,
        valid: true,
        notes: None,
    };

    let oversized = ModelInfo {
        id: "oversized-7b".into(),
        name: "Oversized 7B".into(),
        kind: ModelKind::Llm,
        path: oversized_path.to_string_lossy().into_owned(),
        size_bytes: 1024 * 1024,
        estimated_ram_mb: 4200,
        source: ModelSource::Custom,
        safety_tier: RamSafetyTier::Tight8Gb,
        is_active: false,
        valid: true,
        notes: None,
    };

    let backend = Arc::new(MockBackend::new(vec![
        "<think>quiet</think>",
        "[emotion: ",
        "empathetic, ",
        "intensity: 0.9] ",
        "ไม่ต้องห่วงนะ ",
        "มีนาอยู่ตรงนี้เสมอค่ะ",
    ]));

    let manager_8gb = LlmBrainManager::with_components(
        root.path().to_path_buf(),
        root.path().to_path_buf(),
        Arc::new(MockRamProbe::new(8192, 4200, 350)),
        Arc::clone(&backend) as Arc<dyn ModelBackend>,
    );

    // 1. Load model A
    manager_8gb.load_model(&model_a).await.expect("load model A");
    assert_eq!(manager_8gb.active_model().unwrap().id, "model-a");

    // 2. Reject oversized model (>= 3800 MB on 8GB class) WITHOUT dropping active model A!
    let reject = manager_8gb.hot_swap(&oversized).await;
    assert!(matches!(
        reject,
        Err(SoulError::InsufficientRam {
            needed_mb: 4200,
            ..
        })
    ));
    assert_eq!(manager_8gb.active_model().unwrap().id, "model-a");

    // 3. Hot-swap to model B -> must strictly drop A before loading B!
    manager_8gb
        .hot_swap(&model_b)
        .await
        .expect("hot swap to model B");
    assert_eq!(manager_8gb.active_model().unwrap().id, "model-b");
    assert_eq!(backend.peak_active(), 1);
    assert_eq!(
        backend.events(),
        vec!["load:model-a", "drop:model-a", "load:model-b"]
    );

    // 4. Stream reply via generate_reply_with_callbacks and confirm emotion arrives before tokens.
    let order = Arc::new(StdMutex::new(Vec::<String>::new()));
    let o1 = Arc::clone(&order);
    let o2 = Arc::clone(&order);

    let reply = manager_8gb
        .generate_reply_with_callbacks(
            "วันนี้เครียดจัง",
            &[],
            &MemoryRetrievalContext::default(),
            LanguagePref::Auto,
            Box::new(move |emo| {
                o1.lock()
                    .unwrap()
                    .push(format!("emotion:{}:{:.1}", emo.tag, emo.intensity));
            }),
            Box::new(move |tok| {
                if !tok.is_final {
                    o2.lock().unwrap().push(format!("token:{}", tok.text));
                }
            }),
        )
        .await
        .expect("generate reply");

    assert_eq!(reply.emotion.tag, EmotionTag::Empathetic);
    assert_eq!(reply.turn.content, "ไม่ต้องห่วงนะ มีนาอยู่ตรงนี้เสมอค่ะ");
    assert_eq!(
        order.lock().unwrap().first().map(String::as_str),
        Some("emotion:empathetic:0.9")
    );

    // 5. Verify local embedding output is 384-dim unit vector.
    let emb = manager_8gb
        .embed_text("สวัสดี มีนา")
        .await
        .expect("local embedding");
    assert_eq!(emb.len(), 384);
    let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4);

    // 6. Unload explicitly drops model B.
    manager_8gb.unload_model().await.expect("unload");
    assert!(manager_8gb.active_model().is_none());
    assert_eq!(
        backend.events(),
        vec![
            "load:model-a",
            "drop:model-a",
            "load:model-b",
            "drop:model-b"
        ]
    );

    // 7. Same 4200 MB model succeeds on 16GB+ hardware!
    let backend_16gb = Arc::new(MockBackend::new(vec![]));
    let manager_16gb = LlmBrainManager::with_components(
        root.path().to_path_buf(),
        root.path().to_path_buf(),
        Arc::new(MockRamProbe::new(16384, 12000, 400)),
        backend_16gb,
    );
    manager_16gb
        .load_model(&oversized)
        .await
        .expect("16GB+ allows 4200 MB model");
    let status_16gb = manager_16gb.system_ram_status();
    assert!(!status_16gb.is_8gb_class);
    assert_eq!(status_16gb.budget_ceiling_mb, 11468);
}

#[tokio::test]
async fn brain_engine_trait_object_and_real_sysinfo_probe_work() {
    let root = TempDirGuard::new("brain_llm_trait");
    let model_path = root.path().join("mini.gguf");
    write_dummy_gguf(&model_path, b"GGUF", 1024 * 1024);

    let model = ModelInfo {
        id: "mini".into(),
        name: "Mini".into(),
        kind: ModelKind::Llm,
        path: model_path.to_string_lossy().into_owned(),
        size_bytes: 1024 * 1024,
        estimated_ram_mb: 1200,
        source: ModelSource::Bundled,
        safety_tier: RamSafetyTier::Safe8Gb,
        is_active: false,
        valid: true,
        notes: None,
    };

    let backend = Arc::new(MockBackend::new(vec![
        "[emotion: joy, intensity: 0.8] Good to see you!",
    ]));
    let mgr = LlmBrainManager::with_components(
        root.path().to_path_buf(),
        root.path().to_path_buf(),
        Arc::new(crate::ram_guard::SysinfoRamProbe),
        backend,
    );
    let engine: &dyn BrainEngine = &mgr;

    let ram = engine.system_ram_status();
    assert!(ram.total_ram_mb > 0);
    assert!(ram.budget_ceiling_mb > 0);

    engine.load_model(&model).await.expect("load mini model");
    let tokens = Arc::new(StdMutex::new(Vec::<String>::new()));
    let tok_clone = Arc::clone(&tokens);
    let reply = engine
        .generate_reply(
            "Hello Mina",
            &[],
            &MemoryRetrievalContext::default(),
            Box::new(move |chunk| {
                if !chunk.is_final {
                    tok_clone.lock().unwrap().push(chunk.text);
                }
            }),
        )
        .await
        .expect("trait generate_reply");

    assert_eq!(reply.emotion.tag, EmotionTag::Joy);
    assert_eq!(reply.turn.content, "Good to see you!");
    assert_eq!(tokens.lock().unwrap().concat(), "Good to see you!");

    let sim_a = engine.embed_text("สวัสดีตอนเช้า มีนา").await.unwrap();
    let sim_b = engine.embed_text("สวัสดีตอนเช้าครับ มีนา").await.unwrap();
    let sim_c = engine.embed_text("quantum thermodynamics matrix").await.unwrap();
    let dot_ab: f32 = sim_a.iter().zip(sim_b.iter()).map(|(x, y)| x * y).sum();
    let dot_ac: f32 = sim_a.iter().zip(sim_c.iter()).map(|(x, y)| x * y).sum();
    assert!(dot_ab > dot_ac);
}

#[tokio::test]
#[ignore = "Requires provisioned 2.5GB Typhoon2.5 GGUF model on disk"]
async fn real_metal_gguf_smoke_test() {
    let bundled = PathBuf::from("/Users/goshi/.gemini/antigravity/scratch/soulmate2d/src-tauri/resources/models/llm");
    let user = PathBuf::from("/Users/goshi/.gemini/antigravity/scratch/soulmate2d/src-tauri/resources/models/llm-user-empty");
    let manager = LlmBrainManager::with_dirs(bundled, user);
    let models = manager.scan_models().expect("scan real models");
    let model = models
        .into_iter()
        .find(|m| m.valid && m.kind == ModelKind::Llm)
        .expect("provisioned GGUF model must exist");

    manager.load_model(&model).await.expect("load real Metal GGUF");
    let emotion_seen = Arc::new(StdMutex::new(None::<EmotionState>));
    let es = Arc::clone(&emotion_seen);

    let reply = manager
        .generate_reply_with_callbacks(
            "สวัสดีจ้ามีนา วันนี้เหนื่อยจัง",
            &[],
            &MemoryRetrievalContext::default(),
            LanguagePref::Auto,
            Box::new(move |e| {
                *es.lock().unwrap() = Some(e);
            }),
            Box::new(|_chunk| {}),
        )
        .await
        .expect("real Metal generation");

    assert!(emotion_seen.lock().unwrap().is_some());
    assert!(!reply.turn.content.is_empty());
    assert!(!reply.turn.content.contains("<think>"));
    assert!(!reply.turn.content.contains("[emotion:"));
    eprintln!(
        "REAL METAL REPLY: tag={} intensity={:.2} text={}",
        reply.emotion.tag, reply.emotion.intensity, reply.turn.content
    );
    manager.unload_model().await.expect("unload real Metal GGUF");
    assert!(manager.active_model().is_none());
}
