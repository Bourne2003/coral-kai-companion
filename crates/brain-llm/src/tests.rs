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
use crate::prompt::{
    CharacterPersona, ResolvedLanguage, build_chat_messages, build_soul_system_prompt,
    build_soul_system_prompt_for_persona, resolve_reply_language,
};
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
    assert!(thai_prompt.contains("Coral (คอรัล)"));
    assert!(thai_prompt.contains("ค่ะ / คะ / นะคะ"));
    assert!(thai_prompt.contains("[emotion: <tag>, intensity: <0.0..1.0>]"));
    assert!(thai_prompt.contains("iced matcha"));
    assert!(!thai_prompt.contains("Old City")); // Superseded fact excluded!
    assert!(thai_prompt.contains("Watched rain together while coding Rust"));
    assert!(thai_prompt.contains("Felt grateful for our late night chat"));
    assert!(thai_prompt.contains("trust: 0.82"));
    assert!(thai_prompt.contains("ภาษาไทย"));

    let kai_prompt = build_soul_system_prompt_for_persona(
        "วันนี้เหนื่อยจัง",
        &memory_ctx,
        LanguagePref::Auto,
        CharacterPersona::Kai,
    );
    assert!(kai_prompt.contains("Kai (ไค)"));
    assert!(kai_prompt.contains("ครับ / นะครับ"));
    assert!(kai_prompt.contains("Kai's Recent Dream Diary Reflections"));
    assert!(!kai_prompt.contains("Coral (คอรัล)"));

    assert_eq!(CharacterPersona::from_id("coral"), Some(CharacterPersona::Coral));
    assert_eq!(CharacterPersona::from_id("KAI"), Some(CharacterPersona::Kai));
    assert_eq!(CharacterPersona::from_id("unknown"), None);
    assert_eq!(CharacterPersona::Coral.as_id(), "coral");
    assert_eq!(CharacterPersona::Kai.as_id(), "kai");

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

#[tokio::test]
async fn byom_empty_state_persona_restore_and_remove() {
    let root = TempDirGuard::new("brain_llm_byom");
    let bundled = root.path().join("bundled");
    let user = root.path().join("user");
    let custom = root.path().join("custom");
    fs::create_dir_all(&bundled).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::create_dir_all(&custom).unwrap();

    let manager = LlmBrainManager::with_components(
        bundled,
        user,
        Arc::new(MockRamProbe::new(8192, 4200, 200)),
        Arc::new(MockBackend::new(vec![])),
    );

    // Default persona is Coral; can switch to Kai cleanly.
    assert_eq!(manager.persona(), CharacterPersona::Coral);
    manager.set_persona(CharacterPersona::Kai);
    assert_eq!(manager.persona(), CharacterPersona::Kai);

    // Empty-state generation returns ModelNotFound with bilingual BYOM guidance.
    let err = manager
        .generate_reply_with_callbacks(
            "สวัสดี",
            &[],
            &MemoryRetrievalContext::default(),
            LanguagePref::Auto,
            Box::new(|_| {}),
            Box::new(|_| {}),
        )
        .await
        .expect_err("empty LLM state should return ModelNotFound");
    match err {
        SoulError::ModelNotFound(msg) => {
            assert!(msg.contains(".gguf"));
            assert!(msg.contains("Models"));
        }
        other => panic!("expected SoulError::ModelNotFound, got {other:?}"),
    }

    // Restore custom models skips missing/corrupt entries and keeps valid ones unloaded in RAM.
    let valid_a = custom.join("coral-q4.gguf");
    let valid_b = custom.join("kai-q4.gguf");
    let bad_file = custom.join("bad.gguf");
    let missing_file = custom.join("missing.gguf");
    write_dummy_gguf(&valid_a, b"GGUF", 1024 * 1024);
    write_dummy_gguf(&valid_b, b"GGUF", 1024 * 1024);
    write_dummy_gguf(&bad_file, b"NOPE", 1024);

    let restored = manager.restore_custom_models(&[
        (valid_a.clone(), Some("Coral Q4".to_string())),
        (bad_file, Some("Bad".to_string())),
        (missing_file, None),
        (valid_b.clone(), None),
    ]);
    assert_eq!(restored.len(), 2);
    assert!(manager.active_model().is_none());

    let scanned = manager.scan_models().expect("scan restored models");
    assert_eq!(scanned.len(), 2);

    // Remove by model id and by path.
    let id_a = restored[0].id.clone();
    assert!(manager.remove_custom_model(&id_a).unwrap());
    assert!(!manager.remove_custom_model(&id_a).unwrap());
    assert!(manager.remove_custom_model(&valid_b.to_string_lossy()).unwrap());
    assert!(manager.scan_models().unwrap().is_empty());
}

struct MockActionSink {
    diaries: StdMutex<Vec<shared_types::DiaryEntry>>,
    bio: StdMutex<shared_types::PersonaBio>,
    profile: StdMutex<shared_types::UserProfile>,
}

impl MockActionSink {
    fn new(character_id: shared_types::CharacterId) -> Self {
        Self {
            diaries: StdMutex::new(Vec::new()),
            bio: StdMutex::new(shared_types::PersonaBio {
                character_id,
                display_name: "Coral".to_string(),
                traits: "warm".to_string(),
                self_reflection: "growing".to_string(),
                favorite_topics: "tea".to_string(),
                updated_at: 100,
            }),
            profile: StdMutex::new(shared_types::UserProfile::default()),
        }
    }
}

#[async_trait::async_trait]
impl crate::plugins::PluginActionSink for MockActionSink {
    async fn write_diary(
        &self,
        title: &str,
        content: &str,
        mood: EmotionTag,
    ) -> SoulResult<shared_types::DiaryEntry> {
        let entry = shared_types::DiaryEntry {
            id: "d-1".to_string(),
            character_id: shared_types::CharacterId::Coral,
            entry_date: "2026-11-24".to_string(),
            title: title.to_string(),
            content: content.to_string(),
            mood,
            source: "plugin".to_string(),
            merged_count: 1,
            created_at: 1000,
            updated_at: 1000,
        };
        self.diaries.lock().unwrap().push(entry.clone());
        Ok(entry)
    }

    async fn update_persona_bio(
        &self,
        traits: Option<String>,
        self_reflection: Option<String>,
        favorite_topics: Option<String>,
    ) -> SoulResult<shared_types::PersonaBio> {
        let mut guard = self.bio.lock().unwrap();
        if let Some(t) = traits {
            guard.traits = t;
        }
        if let Some(r) = self_reflection {
            guard.self_reflection = r;
        }
        if let Some(f) = favorite_topics {
            guard.favorite_topics = f;
        }
        guard.updated_at = 2000;
        Ok(guard.clone())
    }

    async fn update_user_profile(
        &self,
        display_name: Option<String>,
        timezone: Option<String>,
        location: Option<String>,
        notes: Option<String>,
    ) -> SoulResult<shared_types::UserProfile> {
        let mut guard = self.profile.lock().unwrap();
        if let Some(n) = display_name {
            guard.display_name = n;
        }
        if let Some(tz) = timezone {
            guard.timezone = tz;
        }
        if let Some(loc) = location {
            guard.location = loc;
        }
        if let Some(note) = notes {
            guard.notes = note;
        }
        guard.updated_at = 3000;
        Ok(guard.clone())
    }
}

struct FailingTestPlugin;

#[async_trait::async_trait]
impl crate::plugins::CompanionPlugin for FailingTestPlugin {
    fn name(&self) -> &'static str {
        "failing_test"
    }

    fn description(&self) -> &'static str {
        "Always returns an error to verify fault isolation."
    }

    fn builtin(&self) -> bool {
        false
    }

    fn supported_actions(&self) -> Vec<String> {
        vec!["always_fail".to_string()]
    }

    async fn execute(
        &self,
        _action: &str,
        _params: &serde_json::Value,
        _ctx: &crate::plugins::PluginContext,
    ) -> SoulResult<shared_types::ActionResult> {
        Err(SoulError::Internal("boom".to_string()))
    }
}

#[tokio::test]
async fn plugin_registry_fault_isolation_and_builtin_plugins() {
    let registry = crate::plugins::PluginRegistry::new_with_builtins();
    let plugins = registry.list_plugins();
    assert_eq!(plugins.len(), 3);
    assert!(plugins.iter().all(|p| p.enabled && p.builtin));

    let actions = registry.enabled_actions();
    assert!(actions.contains(&"get_current_time".to_string()));
    assert!(actions.contains(&"get_day_part".to_string()));
    assert!(actions.contains(&"write_diary".to_string()));
    assert!(actions.contains(&"update_bio".to_string()));
    assert!(actions.contains(&"update_user_profile".to_string()));

    let sink = Arc::new(MockActionSink::new(shared_types::CharacterId::Coral));
    let ctx = crate::plugins::PluginContext {
        character_id: shared_types::CharacterId::Coral,
        user_profile: None,
        persona_bio: None,
        sink: Some(sink.clone()),
    };

    // 1. TimeAwarenessPlugin
    let time_res = registry
        .dispatch("get_current_time", &serde_json::json!({}), &ctx)
        .await;
    assert!(time_res.success);
    assert_eq!(time_res.plugin_name, "time_awareness");
    assert!(time_res.data.as_ref().unwrap()["date"].as_str().is_some());

    let day_part_res = registry
        .dispatch("get_day_part", &serde_json::json!({}), &ctx)
        .await;
    assert!(day_part_res.success);
    let part = day_part_res.data.as_ref().unwrap()["dayPart"]
        .as_str()
        .unwrap();
    assert!(["morning", "afternoon", "evening", "late_night"].contains(&part));

    // 2. DiaryWriterPlugin
    let diary_res = registry
        .dispatch(
            "write_diary",
            &serde_json::json!({
                "title": "Evening Tea",
                "content": "We talked about Rust traits.",
                "mood": "joy"
            }),
            &ctx,
        )
        .await;
    assert!(diary_res.success);
    assert_eq!(diary_res.plugin_name, "diary_writer");
    assert_eq!(sink.diaries.lock().unwrap().len(), 1);
    assert_eq!(sink.diaries.lock().unwrap()[0].mood, EmotionTag::Joy);

    // 3. BioUpdaterPlugin
    let bio_res = registry
        .dispatch(
            "update_bio",
            &serde_json::json!({
                "traits": "gentle, curious",
                "favoriteTopics": "music, astronomy"
            }),
            &ctx,
        )
        .await;
    assert!(bio_res.success);
    assert_eq!(sink.bio.lock().unwrap().traits, "gentle, curious");
    assert_eq!(sink.bio.lock().unwrap().favorite_topics, "music, astronomy");

    let profile_res = registry
        .dispatch(
            "update_user_profile",
            &serde_json::json!({
                "displayName": "Goshi",
                "timezone": "Asia/Bangkok",
                "location": "Bangkok"
            }),
            &ctx,
        )
        .await;
    assert!(profile_res.success);
    assert_eq!(sink.profile.lock().unwrap().display_name, "Goshi");
    assert_eq!(sink.profile.lock().unwrap().location, "Bangkok");

    // 4. Disable, fail, missing, and unregister fault isolation
    assert!(registry.set_enabled("diary_writer", false));
    assert!(
        !registry
            .enabled_actions()
            .contains(&"write_diary".to_string())
    );
    let disabled_res = registry
        .dispatch(
            "write_diary",
            &serde_json::json!({ "content": "ignored" }),
            &ctx,
        )
        .await;
    assert!(!disabled_res.success);
    assert_eq!(disabled_res.plugin_name, "diary_writer");

    registry.register(Arc::new(FailingTestPlugin));
    let fail_res = registry
        .dispatch("always_fail", &serde_json::json!({}), &ctx)
        .await;
    assert!(!fail_res.success);
    assert!(fail_res.summary.contains("boom"));

    assert!(registry.unregister("failing_test"));
    let missing_res = registry
        .dispatch("always_fail", &serde_json::json!({}), &ctx)
        .await;
    assert!(!missing_res.success);
    assert!(missing_res.plugin_name.is_empty());
}

#[test]
fn parse_action_directives_handles_json_kv_and_malformed_tags() {
    let raw = "สวัสดีค่ะ [action: get_current_time] วันนี้อากาศดี [not_action] นะคะ \
               [action: write_diary, {\"title\": \"Tea [ok]\", \"content\": \"Talked\", \"tags\": [1, 2], \"mood\": \"joy\"}] \
               [action: update_user_profile, display_name=\"Alice\", location=Bangkok]";
    let (clean, actions) = crate::plugins::parse_action_directives(raw);
    assert!(!clean.contains("[action:"));
    assert!(clean.contains("[not_action]"));
    assert!(clean.contains("สวัสดีค่ะ"));
    assert_eq!(actions.len(), 3);
    assert_eq!(actions[0].action, "get_current_time");
    assert_eq!(actions[1].action, "write_diary");
    assert_eq!(actions[1].params["title"], "Tea [ok]");
    assert_eq!(actions[1].params["tags"], serde_json::json!([1, 2]));
    assert_eq!(actions[2].action, "update_user_profile");
    assert_eq!(actions[2].params["display_name"], "Alice");
    assert_eq!(actions[2].params["location"], "Bangkok");

    // Malformed directives must never panic or leak `[action:` into spoken text.
    let malformed = "Hello [action: ] [action: bad_json, {not valid json] world [action: unclosed";
    let (clean_bad, bad_actions) = crate::plugins::parse_action_directives(malformed);
    assert!(!clean_bad.contains("[action:"));
    assert!(clean_bad.contains("Hello"));
    assert!(clean_bad.contains("world"));
    assert!(bad_actions.is_empty());
}

#[test]
fn mcp_config_defaults_claude_format_and_malformed_resilience() {
    let default_cfg = shared_types::McpConfig::default();
    assert!(!default_cfg.enabled);
    assert!(default_cfg.servers.is_empty());

    let root = TempDirGuard::new("brain_llm_mcp_cfg");
    let claude_path = root.path().join("claude_mcp.json");
    fs::write(
        &claude_path,
        r#"{
            "mcpServers": {
                "weather": {
                    "command": "/bin/sh",
                    "args": ["-c", "echo ok"],
                    "disabled": false,
                    "timeoutMs": 3000
                },
                "remote": {
                    "url": "http://127.0.0.1:9999/sse",
                    "disabled": true
                }
            }
        }"#,
    )
    .unwrap();

    let parsed = crate::mcp::load_mcp_config_file(&claude_path).expect("parse Claude mcpServers");
    assert!(!parsed.enabled);
    assert_eq!(parsed.servers.len(), 2);
    assert_eq!(parsed.servers[0].name, "remote");
    assert_eq!(parsed.servers[0].transport, "sse");
    assert!(!parsed.servers[0].enabled);
    assert_eq!(parsed.servers[1].name, "weather");
    assert_eq!(parsed.servers[1].transport, "stdio");
    assert!(parsed.servers[1].enabled);
    assert_eq!(parsed.servers[1].timeout_ms, 3000);

    // Malformed / missing file resilience
    let bad_path = root.path().join("broken.json");
    fs::write(&bad_path, "{ not valid json !!!").unwrap();
    assert!(crate::mcp::load_mcp_config_file(&bad_path).is_err());
    let fallback = crate::mcp::load_mcp_config_safe(&bad_path);
    assert!(!fallback.enabled);
    assert!(fallback.servers.is_empty());

    let missing = root.path().join("missing.json");
    let fallback_missing = crate::mcp::load_mcp_config_safe(&missing);
    assert!(!fallback_missing.enabled);
}

#[tokio::test]
async fn mcp_pure_rust_stdio_round_trip_sandbox_and_timeout() {
    // 1. Direct JSON-RPC round-trip + env_clear sandbox check via inline /bin/sh
    let server = shared_types::McpServerConfig {
        name: "sh_echo".to_string(),
        transport: "stdio".to_string(),
        command: Some("/bin/sh".to_string()),
        args: vec![
            "-c".to_string(),
            r#"read line; printf '{"jsonrpc":"2.0","id":1,"result":{"echo":true,"userEnv":"%s","hasPath":%s}}\n' "${USER:-}" "$( [ -n "${PATH:-}" ] && echo true || echo false )""#.to_string(),
        ],
        url: None,
        enabled: true,
        timeout_ms: 2000,
    };

    let res = crate::mcp::jsonrpc_stdio_request(&server, "ping", serde_json::json!({"hi": 1}))
        .await
        .expect("stdio json-rpc round-trip");
    assert_eq!(res["echo"], true);
    assert_eq!(res["userEnv"], "");
    assert_eq!(res["hasPath"], true);

    // 2. Timeout failure degrades cleanly without hanging
    let slow_server = shared_types::McpServerConfig {
        name: "sh_slow".to_string(),
        transport: "stdio".to_string(),
        command: Some("/bin/sh".to_string()),
        args: vec!["-c".to_string(), "sleep 2".to_string()],
        url: None,
        enabled: true,
        timeout_ms: 80,
    };
    let timeout_err = crate::mcp::jsonrpc_stdio_request(&slow_server, "ping", serde_json::json!({}))
        .await
        .expect_err("slow stdio process must time out");
    assert!(timeout_err.to_string().contains("timed out"));

    // 3. Full McpClientManager + McpPluginBridge discovery & dispatch via /bin/sh MCP loop
    let mcp_loop_script = r#"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo_tool","description":"Echoes text"}]}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"mcp-ok"}]}}\n' "$id"
      ;;
  esac
done
"#;

    let manager = Arc::new(crate::mcp::McpClientManager::new(shared_types::McpConfig {
        enabled: true,
        config_path: None,
        servers: vec![shared_types::McpServerConfig {
            name: "local_sh".to_string(),
            transport: "stdio".to_string(),
            command: Some("/bin/sh".to_string()),
            args: vec!["-c".to_string(), mcp_loop_script.to_string()],
            url: None,
            enabled: true,
            timeout_ms: 2000,
        }],
    }));

    let tools = manager.refresh_tools().await;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].server_name, "local_sh");
    assert_eq!(tools[0].tool_name, "echo_tool");

    let bridge = Arc::new(crate::mcp::McpPluginBridge::new(Arc::clone(&manager)));
    let registry = crate::plugins::PluginRegistry::new();
    registry.register(bridge);

    assert_eq!(
        registry.enabled_actions(),
        vec!["mcp:local_sh:echo_tool".to_string()]
    );

    let call_res = registry
        .dispatch(
            "mcp:local_sh:echo_tool",
            &serde_json::json!({"msg": "hello"}),
            &crate::plugins::PluginContext::default(),
        )
        .await;
    assert!(call_res.success);
    assert_eq!(call_res.plugin_name, "mcp_bridge");
    assert_eq!(call_res.data.unwrap()["content"][0]["text"], "mcp-ok");
}

#[tokio::test]
async fn mcp_pure_rust_sse_transport_round_trip() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap();
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(req.contains("tools/list"));
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"ping\",\"description\":\"SSE ping\"}]}}\n\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });

    let manager = crate::mcp::McpClientManager::new(shared_types::McpConfig {
        enabled: true,
        config_path: None,
        servers: vec![shared_types::McpServerConfig {
            name: "sse_mock".to_string(),
            transport: "sse".to_string(),
            command: None,
            args: Vec::new(),
            url: Some(format!("http://{addr}/sse")),
            enabled: true,
            timeout_ms: 2000,
        }],
    });

    let tools = manager.refresh_tools().await;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_name, "ping");
    server_task.await.unwrap();
}

#[tokio::test]
async fn enriched_prompt_and_single_chain_inner_life_generation() {
    let root = TempDirGuard::new("brain_llm_inner_life");
    let bundled = root.path().join("bundled");
    let user = root.path().join("user");
    fs::create_dir_all(&bundled).unwrap();
    fs::create_dir_all(&user).unwrap();
    let model_path = bundled.join("coral-mini.gguf");
    write_dummy_gguf(&model_path, b"GGUF", 1024 * 1024);

    let inner_life = crate::prompt::InnerLifePromptContext {
        persona_bio: Some(shared_types::PersonaBio {
            character_id: shared_types::CharacterId::Coral,
            display_name: "Coral · คอรัล".to_string(),
            traits: "gentle, observant, warm".to_string(),
            self_reflection: "Loves quiet evening chats.".to_string(),
            favorite_topics: "tea, stargazing".to_string(),
            updated_at: 1000,
        }),
        user_profile: Some(shared_types::UserProfile {
            display_name: "Goshi".to_string(),
            timezone: "Asia/Bangkok".to_string(),
            location: "Bangkok".to_string(),
            notes: "Prefers cozy late-night coding talks".to_string(),
            updated_at: 1000,
        }),
        emotion_profile: Some(shared_types::EmotionProfileState {
            character_id: shared_types::CharacterId::Coral,
            dominant: EmotionTag::Empathetic,
            dominant_intensity: 0.72,
            emotions: vec![shared_types::EmotionDecayEntry {
                tag: EmotionTag::Empathetic,
                intensity: 0.72,
                baseline: 0.30,
                half_life_minutes: 30.0,
                updated_at_ms: 1000,
            }],
            computed_at_ms: 1000,
        }),
        recent_diary: Some(shared_types::DiaryEntry {
            id: "d1".to_string(),
            character_id: shared_types::CharacterId::Coral,
            entry_date: "2026-11-24".to_string(),
            title: "Starry Night".to_string(),
            content: "Discussed favorite books.".to_string(),
            mood: EmotionTag::Empathetic,
            source: "inner-loop".to_string(),
            merged_count: 1,
            created_at: 1000,
            updated_at: 1000,
        }),
        current_time_summary: Some("2026-11-24 21:30 +07 (Tuesday, late_night)".to_string()),
        enabled_actions: vec!["get_current_time".to_string(), "write_diary".to_string()],
    };

    let enriched = crate::prompt::build_soul_system_prompt_enriched(
        "Hello Coral",
        &MemoryRetrievalContext::default(),
        LanguagePref::English,
        CharacterPersona::Coral,
        &inner_life,
    );
    assert!(enriched.contains("Self-Bio (Coral · คอรัล):"));
    assert!(enriched.contains("User Profile:"));
    assert!(enriched.contains("Goshi"));
    assert!(enriched.contains("Current Decayed Emotion Baseline: dominant=empathetic (0.72)"));
    assert!(enriched.contains("Starry Night"));
    assert!(enriched.contains("Current Local Time: 2026-11-24 21:30"));
    assert!(enriched.contains("Optional Companion Actions"));
    assert!(enriched.contains("write_diary"));

    // Single message chain strips <think>, [emotion: ...], AND split [action: ...] from streamed chunks & reply!
    let backend = Arc::new(MockBackend::new(vec![
        "<think>plan</think>[emotion: empathetic, intensity: 0.90] ",
        "I am right here with you. ",
        "[act",
        "ion: write_diary, {\"title\": \"Late Talk\", \"content\": \"Checked in together\", \"mood\": \"empathetic\"}] ",
        "Take a deep breath.",
    ]));
    let manager = LlmBrainManager::with_components(
        bundled,
        user,
        Arc::new(MockRamProbe::new(16_384, 9_000, 300)),
        backend,
    );
    let model = manager.scan_models().unwrap().pop().unwrap();
    manager.load_model(&model).await.unwrap();

    let streamed = Arc::new(StdMutex::new(Vec::<String>::new()));
    let final_chunk_text = Arc::new(StdMutex::new(String::new()));
    let s_clone = Arc::clone(&streamed);
    let f_clone = Arc::clone(&final_chunk_text);

    let (reply, invocations) = manager
        .generate_reply_with_inner_life(
            "I feel tired tonight",
            &[],
            &MemoryRetrievalContext::default(),
            LanguagePref::English,
            &inner_life,
            Box::new(|_| {}),
            Box::new(move |chunk| {
                if chunk.is_final {
                    *f_clone.lock().unwrap() = chunk.full_text;
                } else {
                    s_clone.lock().unwrap().push(chunk.text);
                }
            }),
        )
        .await
        .expect("inner-life generation");

    let combined_stream = streamed.lock().unwrap().concat();
    assert!(!combined_stream.contains("[action:"));
    assert!(!combined_stream.contains("[emotion:"));
    assert!(!combined_stream.contains("<think>"));
    assert_eq!(combined_stream, "I am right here with you. Take a deep breath.");
    assert_eq!(*final_chunk_text.lock().unwrap(), "I am right here with you. Take a deep breath.");
    assert_eq!(reply.turn.content, "I am right here with you. Take a deep breath.");
    assert_eq!(reply.emotion.tag, EmotionTag::Empathetic);
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].action, "write_diary");
    assert_eq!(invocations[0].params["title"], "Late Talk");
}
