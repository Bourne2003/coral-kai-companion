use std::f32::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use shared_types::{
    EmotionTag, LanguagePref, ModelInfo, ModelKind, ModelSource, RamSafetyTier, SoulError,
    VisemeFrame, VoiceEngine, VoiceMode,
};

use crate::{
    AdaptiveVad, MonoResampler, PcmVisemePlayer, TARGET_SAMPLE_RATE, VAD_FRAME_SAMPLES, VadConfig,
    VadDecision, VoiceDescriptor, VoiceLangTarget, VoicePipeline, detect_thai_script,
    downmix_to_mono_f32, extract_pcm_viseme_frames, frame_rms, generate_phonetic_visemes,
    route_voice_language, select_preferred_voice,
};

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "soulmate2d-voice-io-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn assert_viseme_bounds(frames: &[VisemeFrame]) {
    assert!(!frames.is_empty(), "expected non-empty viseme frames");
    let mut prev_ts = i64::MIN;
    for f in frames {
        assert!(f.mouth_open_y.is_finite(), "mouth_open_y must be finite");
        assert!(f.mouth_form.is_finite(), "mouth_form must be finite");
        assert!(f.energy.is_finite(), "energy must be finite");
        assert!(
            (0.0..=1.0).contains(&f.mouth_open_y),
            "mouth_open_y out of bounds: {}",
            f.mouth_open_y
        );
        assert!(
            (-1.0..=1.0).contains(&f.mouth_form),
            "mouth_form out of bounds: {}",
            f.mouth_form
        );
        assert!(
            (0.0..=1.0).contains(&f.energy),
            "energy out of bounds: {}",
            f.energy
        );
        assert!(
            f.timestamp_ms >= prev_ts,
            "viseme timestamps must be non-decreasing: {} < {}",
            f.timestamp_ms,
            prev_ts
        );
        prev_ts = f.timestamp_ms;
    }
}

#[test]
fn resampler_48k_to_16k_mono_accuracy() {
    let sample_rate = 48_000u32;
    let freq_hz = 440.0f32;
    let duration_secs = 1.0f32;
    let n_frames = (sample_rate as f32 * duration_secs) as usize;

    // Interleaved stereo: L = 0.7 * sin, R = 0.3 * sin -> mono average = 0.5 * sin.
    let mut stereo = Vec::with_capacity(n_frames * 2);
    for i in 0..n_frames {
        let s = (TAU * freq_hz * (i as f32) / sample_rate as f32).sin();
        stereo.push(0.7 * s);
        stereo.push(0.3 * s);
    }

    let mono = downmix_to_mono_f32(&stereo, 2);
    assert_eq!(mono.len(), n_frames);

    // 1) Offline clip resample accuracy
    let resampled_clip =
        MonoResampler::resample_clip(sample_rate, &mono).expect("clip resample 48k -> 16k");
    assert_eq!(resampled_clip.len(), TARGET_SAMPLE_RATE as usize);

    // Measure both waveform correlation and phase-invariant spectral purity at 440 Hz vs 1000 Hz.
    let start = 300usize;
    let end = resampled_clip.len() - 300;
    let slice = &resampled_clip[start..end];
    let mut dot_440 = 0.0f64;
    let mut quad_440 = 0.0f64;
    let mut dot_1k = 0.0f64;
    let mut quad_1k = 0.0f64;
    let mut norm_out = 0.0f64;
    let mut norm_ref = 0.0f64;
    for (idx, &sample) in slice.iter().enumerate() {
        let i = start + idx;
        let angle_440 = TAU * freq_hz * (i as f32) / TARGET_SAMPLE_RATE as f32;
        let angle_1k = TAU * 1_000.0 * (i as f32) / TARGET_SAMPLE_RATE as f32;
        let ref_s = 0.5 * angle_440.sin();
        let s = f64::from(sample);
        dot_440 += s * f64::from(angle_440.sin());
        quad_440 += s * f64::from(angle_440.cos());
        dot_1k += s * f64::from(angle_1k.sin());
        quad_1k += s * f64::from(angle_1k.cos());
        norm_out += s * s;
        norm_ref += f64::from(ref_s) * f64::from(ref_s);
    }
    let cosine_sim = (0.5 * dot_440) / (norm_out.sqrt() * norm_ref.sqrt());
    assert!(
        cosine_sim > 0.99,
        "48kHz -> 16kHz sine phase/frequency correlation too low: {cosine_sim}"
    );

    let n = slice.len() as f64;
    let tone_energy_440 = 2.0 * (dot_440 * dot_440 + quad_440 * quad_440) / (n * n);
    let tone_energy_1k = 2.0 * (dot_1k * dot_1k + quad_1k * quad_1k) / (n * n);
    let mean_power = norm_out / n;
    let purity = tone_energy_440 / mean_power;
    assert!(
        purity > 0.995,
        "48kHz -> 16kHz 440 Hz spectral purity too low: {purity}"
    );
    assert!(
        tone_energy_1k < 1e-4,
        "unexpected off-frequency aliasing energy at 1kHz: {tone_energy_1k}"
    );

    let rms = frame_rms(slice);
    let expected_rms = 0.5 / std::f32::consts::SQRT_2;
    assert!(
        (rms - expected_rms).abs() < 0.02,
        "resampled RMS {rms} drifted from expected {expected_rms}"
    );

    // 2) Streaming chunked resampler accuracy (simulating cpal callbacks)
    let mut streaming = MonoResampler::new(sample_rate).expect("streaming resampler");
    let mut streamed_out = Vec::new();
    for chunk in mono.chunks(512) {
        streamed_out.extend(streaming.push(chunk).expect("streaming push"));
    }
    streamed_out.extend(streaming.flush().expect("streaming flush"));
    assert!(
        (streamed_out.len() as isize - TARGET_SAMPLE_RATE as isize).abs() <= 64,
        "unexpected streamed output length: {}",
        streamed_out.len()
    );

    // 3) Also validate 44.1 kHz -> 16 kHz non-integer ratio resampling
    let mono_44k: Vec<f32> = (0..44_100)
        .map(|i| 0.4 * (TAU * 440.0 * i as f32 / 44_100.0).sin())
        .collect();
    let out_44k =
        MonoResampler::resample_clip(44_100, &mono_44k).expect("clip resample 44.1k -> 16k");
    assert_eq!(out_44k.len(), TARGET_SAMPLE_RATE as usize);
    let rms_44k = frame_rms(&out_44k[300..out_44k.len() - 300]);
    assert!(
        (rms_44k - (0.4 / std::f32::consts::SQRT_2)).abs() < 0.02,
        "44.1kHz -> 16kHz RMS drifted: {rms_44k}"
    );
}

#[test]
fn vad_speech_onset_offset_and_adaptive_noise_floor() {
    let config = VadConfig {
        onset_frames: 3,
        hangover_silence_frames: 5,
        pre_roll_frames: 4,
        min_utterance_samples: VAD_FRAME_SAMPLES * 4,
        ..VadConfig::default()
    };
    let mut vad = AdaptiveVad::new(config);
    let initial_floor = vad.noise_floor();

    // Feed quiet ambient noise slightly above initial_noise_floor -> floor adapts smoothly upward.
    let quiet_noise = vec![0.006f32; VAD_FRAME_SAMPLES];
    for _ in 0..25 {
        assert_eq!(
            vad.process_frame(&quiet_noise, VoiceMode::HandsFree, false),
            VadDecision::Silence
        );
    }
    assert!(
        vad.noise_floor() > initial_floor,
        "noise floor should adapt upward toward ambient 0.006 (got {})",
        vad.noise_floor()
    );

    // Feed loud speech frames -> triggers SpeechActive after onset_frames.
    let speech_frame: Vec<f32> = (0..VAD_FRAME_SAMPLES)
        .map(|i| 0.20 * (TAU * 220.0 * i as f32 / 16_000.0).sin())
        .collect();

    assert_eq!(
        vad.process_frame(&speech_frame, VoiceMode::HandsFree, false),
        VadDecision::Silence
    );
    assert_eq!(
        vad.process_frame(&speech_frame, VoiceMode::HandsFree, false),
        VadDecision::Silence
    );
    assert_eq!(
        vad.process_frame(&speech_frame, VoiceMode::HandsFree, false),
        VadDecision::SpeechActive
    );
    for _ in 0..6 {
        assert_eq!(
            vad.process_frame(&speech_frame, VoiceMode::HandsFree, false),
            VadDecision::SpeechActive
        );
    }

    // Feed silence until hangover completes -> UtteranceReady!
    let mut emitted = None;
    for _ in 0..6 {
        if let VadDecision::UtteranceReady(utt) =
            vad.process_frame(&quiet_noise, VoiceMode::HandsFree, false)
        {
            emitted = Some(utt);
            break;
        }
    }
    let utt = emitted.expect("expected finalized speech utterance after hangover");
    assert!(
        utt.len() >= VAD_FRAME_SAMPLES * 10,
        "expected pre-roll + speech + hangover samples, got {}",
        utt.len()
    );
}

#[test]
fn vad_echo_suppression_mutes_while_is_speaking() {
    let mut vad = AdaptiveVad::default();
    let baseline_floor = vad.noise_floor();
    let loud_tts_echo = vec![0.35f32; VAD_FRAME_SAMPLES];

    // While is_speaking == true, all mic audio must be discarded (HandsFree & PushToTalk).
    for _ in 0..20 {
        assert_eq!(
            vad.process_frame(&loud_tts_echo, VoiceMode::HandsFree, true),
            VadDecision::Suppressed
        );
        assert_eq!(
            vad.process_frame(&loud_tts_echo, VoiceMode::PushToTalk, true),
            VadDecision::Suppressed
        );
    }
    assert!(
        (vad.noise_floor() - baseline_floor).abs() < 1e-6,
        "echo frames while speaking must not corrupt noise floor"
    );
    assert!(!vad.is_speech_active());
    assert!(vad.flush_utterance().is_none());

    // VoiceMode::Muted also suppresses all audio even when is_speaking == false.
    assert_eq!(
        vad.process_frame(&loud_tts_echo, VoiceMode::Muted, false),
        VadDecision::Suppressed
    );

    // PushToTalk works immediately once is_speaking == false.
    for _ in 0..12 {
        assert_eq!(
            vad.process_frame(&loud_tts_echo, VoiceMode::PushToTalk, false),
            VadDecision::SpeechActive
        );
    }
    let flushed = vad
        .flush_utterance()
        .expect("PushToTalk should flush captured speech when not speaking");
    assert_eq!(flushed.len(), VAD_FRAME_SAMPLES * 12);

    // If TTS starts mid-utterance, active buffer is discarded cleanly.
    for _ in 0..5 {
        vad.process_frame(&loud_tts_echo, VoiceMode::PushToTalk, false);
    }
    assert!(vad.is_speech_active());
    assert_eq!(
        vad.process_frame(&loud_tts_echo, VoiceMode::PushToTalk, true),
        VadDecision::Suppressed
    );
    assert!(!vad.is_speech_active());
    assert!(vad.flush_utterance().is_none());
}

#[test]
fn thai_and_english_voice_routing_and_selection() {
    assert!(detect_thai_script("สวัสดีค่ะ มีนา"));
    assert!(detect_thai_script("Hello มีนา"));
    assert!(!detect_thai_script("Hello Mina, how are you?"));

    assert_eq!(
        route_voice_language("สวัสดีครับ", LanguagePref::Auto),
        VoiceLangTarget::Thai
    );
    assert_eq!(
        route_voice_language("Good morning!", LanguagePref::Auto),
        VoiceLangTarget::English
    );
    assert_eq!(
        route_voice_language("สวัสดี", LanguagePref::English),
        VoiceLangTarget::Thai
    );

    let voices = vec![
        VoiceDescriptor {
            id: "com.apple.speech.synthesis.voice.daniel".into(),
            name: "Daniel".into(),
            language: "en-GB".into(),
        },
        VoiceDescriptor {
            id: "com.apple.voice.compact.th-TH.Kanya".into(),
            name: "Kanya".into(),
            language: "th_TH".into(),
        },
        VoiceDescriptor {
            id: "com.apple.voice.enhanced.th-TH.Narisa".into(),
            name: "Narisa (Enhanced)".into(),
            language: "th-TH".into(),
        },
        VoiceDescriptor {
            id: "com.apple.voice.compact.en-US.Samantha".into(),
            name: "Samantha".into(),
            language: "en_US".into(),
        },
        VoiceDescriptor {
            id: "com.apple.voice.enhanced.en-US.Ava".into(),
            name: "Ava (Enhanced)".into(),
            language: "en-US".into(),
        },
    ];

    let th_idx = select_preferred_voice(&voices, VoiceLangTarget::Thai).expect("Thai voice");
    assert!(
        voices[th_idx].name.contains("Narisa") || voices[th_idx].name.contains("Kanya"),
        "unexpected Thai voice: {:?}",
        voices[th_idx]
    );

    let en_idx = select_preferred_voice(&voices, VoiceLangTarget::English).expect("English voice");
    assert!(
        voices[en_idx].name.contains("Ava") || voices[en_idx].name.contains("Samantha"),
        "unexpected English voice: {:?}",
        voices[en_idx]
    );

    // Fallback check when only Kanya & Samantha exist.
    let minimal = vec![voices[1].clone(), voices[3].clone()];
    assert_eq!(
        minimal[select_preferred_voice(&minimal, VoiceLangTarget::Thai).unwrap()].name,
        "Kanya"
    );
    assert_eq!(
        minimal[select_preferred_voice(&minimal, VoiceLangTarget::English).unwrap()].name,
        "Samantha"
    );
}

#[test]
fn phonetic_viseme_bounds_cadence_and_smoothing() {
    let emotions = [
        EmotionTag::Joy,
        EmotionTag::Sadness,
        EmotionTag::Shy,
        EmotionTag::Surprised,
        EmotionTag::Thinking,
        EmotionTag::Empathetic,
        EmotionTag::Neutral,
    ];

    for emotion in emotions {
        let th_frames = generate_phonetic_visemes("สวัสดีค่ะ วันนี้มีความสุขจังเลย!", emotion, 1_000);
        assert_viseme_bounds(&th_frames);
        let last = th_frames.last().unwrap();
        assert_eq!(last.mouth_open_y, 0.0);
        assert_eq!(last.energy, 0.0);

        let en_frames = generate_phonetic_visemes("Hello Mina, nice to meet you!", emotion, 2_000);
        assert_viseme_bounds(&en_frames);
        // Verify ~60 Hz cadence (~16.67 ms per step).
        if en_frames.len() >= 10 {
            let span = en_frames[9].timestamp_ms - en_frames[0].timestamp_ms;
            assert!(
                (140..=165).contains(&span),
                "expected ~150 ms across 9 intervals at 60 FPS, got {span} ms"
            );
        }
    }

    // Damped spring should avoid abrupt 0 -> 0.9 single-frame discontinuities.
    let open_frames = generate_phonetic_visemes("aaaaaa", EmotionTag::Neutral, 0);
    for w in open_frames.windows(2) {
        let delta = (w[1].mouth_open_y - w[0].mouth_open_y).abs();
        assert!(
            delta < 0.35,
            "viseme step delta too discontinuous ({delta}) between {:?} and {:?}",
            w[0],
            w[1]
        );
    }

    // Rounded vowel ('โ'/'อ'/'u') should produce negative mouth_form vs spread ('ี'/'i').
    let round_frames = generate_phonetic_visemes("อูููโอ", EmotionTag::Neutral, 0);
    let spread_frames = generate_phonetic_visemes("อีีีอี", EmotionTag::Neutral, 0);
    let avg_round: f32 =
        round_frames.iter().map(|f| f.mouth_form).sum::<f32>() / round_frames.len() as f32;
    let avg_spread: f32 =
        spread_frames.iter().map(|f| f.mouth_form).sum::<f32>() / spread_frames.len() as f32;
    assert!(
        avg_round < 0.0 && avg_spread > 0.0,
        "expected negative mouth_form for rounded ({avg_round}) and positive for spread ({avg_spread})"
    );
}

#[test]
fn pcm_rms_zcr_envelope_extraction() {
    let sample_rate = 16_000u32;
    let total_samples = sample_rate as usize; // 1.0s
    let mut pcm = vec![0.0f32; total_samples];

    // [0..0.3s]: silence
    // [0.3..0.65s]: low-frequency vowel-like 180 Hz tone
    for (i, sample) in pcm[4_800..10_400].iter_mut().enumerate() {
        *sample = 0.55 * (TAU * 180.0 * i as f32 / sample_rate as f32).sin();
    }
    // [0.65..1.0s]: high-frequency sibilant-like 4500 Hz tone
    for (i, sample) in pcm[10_400..16_000].iter_mut().enumerate() {
        *sample = 0.35 * (TAU * 4_500.0 * i as f32 / sample_rate as f32).sin();
    }

    let frames = extract_pcm_viseme_frames(&pcm, sample_rate, 5_000, EmotionTag::Neutral);
    assert_viseme_bounds(&frames);
    // 1.0s at 60 FPS -> ~60 frames + 1 closing frame
    assert!(
        (58..=63).contains(&frames.len()),
        "expected ~61 frames for 1s @ 60 FPS, got {}",
        frames.len()
    );

    let silence_avg_energy: f32 =
        frames[2..14].iter().map(|f| f.energy).sum::<f32>() / 12.0;
    let vowel_avg_energy: f32 =
        frames[22..36].iter().map(|f| f.energy).sum::<f32>() / 14.0;
    let vowel_avg_form: f32 =
        frames[22..36].iter().map(|f| f.mouth_form).sum::<f32>() / 14.0;
    let sibilant_avg_form: f32 =
        frames[44..56].iter().map(|f| f.mouth_form).sum::<f32>() / 12.0;

    assert!(
        silence_avg_energy < 0.02,
        "silence energy should stay near zero, got {silence_avg_energy}"
    );
    assert!(
        vowel_avg_energy > 0.5,
        "vowel energy should be strong, got {vowel_avg_energy}"
    );
    assert!(
        sibilant_avg_form > vowel_avg_form + 0.3,
        "high-ZCR sibilant mouth_form ({sibilant_avg_form}) should exceed low-ZCR vowel ({vowel_avg_form})"
    );
}

#[tokio::test]
async fn asr_model_scanner_and_magic_validation() {
    let dir = unique_temp_dir("asr-scan");
    let valid_bin = dir.join("ggml-custom-test.bin");
    let invalid_bin = dir.join("not-a-model.bin");

    // 0x67676d6c LE = b"lmgg"
    let mut valid_bytes = b"lmgg".to_vec();
    valid_bytes.extend_from_slice(&[0u8; 4096]);
    std::fs::write(&valid_bin, valid_bytes).expect("write valid magic bin");
    std::fs::write(&invalid_bin, b"<html>404</html>").expect("write invalid bin");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{
            "id": "whisper-ggml-custom-test",
            "name": "Whisper Test Base",
            "filename": "ggml-custom-test.bin",
            "estimatedRamMb": 384,
            "safetyTier": "safe8Gb"
        }"#,
    )
    .expect("write manifest");

    let pipeline = VoicePipeline::with_asr_dirs(Some(dir.clone()), None);
    let models = pipeline.scan_asr_models().expect("scan_asr_models");
    assert_eq!(models.len(), 2);

    let valid_model = models
        .iter()
        .find(|m| m.id == "whisper-ggml-custom-test")
        .expect("manifest model discovered");
    assert!(valid_model.valid);
    assert_eq!(valid_model.safety_tier, RamSafetyTier::Safe8Gb);
    assert_eq!(valid_model.source, ModelSource::Bundled);

    let corrupt_model = models
        .iter()
        .find(|m| m.id == "not-a-model")
        .expect("corrupt bin discovered");
    assert!(!corrupt_model.valid);

    // Reject invalid magic on custom registration & load
    assert!(matches!(
        pipeline.register_custom_asr_model(&invalid_bin, None),
        Err(SoulError::InvalidInput(_))
    ));
    let reg = pipeline
        .register_custom_asr_model(&valid_bin, Some("Custom Base"))
        .expect("register valid bin");
    assert!(reg.valid);

    let wrong_kind = ModelInfo {
        kind: ModelKind::Llm,
        ..valid_model.clone()
    };
    assert!(matches!(
        pipeline.load_asr_model(&wrong_kind).await,
        Err(SoulError::InvalidInput(_))
    ));
    assert!(matches!(
        pipeline.load_asr_model(corrupt_model).await,
        Err(SoulError::Voice(_))
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn voice_pipeline_headless_guards_and_cancellation() {
    let pipeline = VoicePipeline::new();
    assert_eq!(pipeline.voice_mode(), VoiceMode::PushToTalk);
    assert_eq!(pipeline.language_pref(), LanguagePref::Auto);
    assert!(!pipeline.is_speaking());

    pipeline
        .set_language_pref(LanguagePref::Thai)
        .await
        .expect("set language pref");
    assert_eq!(pipeline.language_pref(), LanguagePref::Thai);

    // Muted mode cleanly rejects start_listening without touching hardware or panicking.
    pipeline
        .set_voice_mode(VoiceMode::Muted)
        .await
        .expect("set muted");
    let muted_err = pipeline
        .start_listening(Box::new(|_| {}))
        .await
        .expect_err("muted mode should refuse listening");
    assert!(matches!(muted_err, SoulError::Voice(_)));

    // Calling stop_listening / stop_speaking when idle is a clean no-op.
    pipeline.stop_listening().await.expect("idle stop_listening");
    pipeline.stop_speaking().await.expect("idle stop_speaking");

    // Speak + cancel via stop_speaking (safe even if AVFoundation has no audio device in CI).
    let frames = Arc::new(Mutex::new(Vec::<VisemeFrame>::new()));
    let frames_cb = Arc::clone(&frames);
    let pipeline_arc = Arc::new(pipeline);
    let stopper = Arc::clone(&pipeline_arc);

    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(75)).await;
        let _ = stopper.stop_speaking().await;
    });

    pipeline_arc
        .speak(
            "Hello there, this is a longer sentence that will be cancelled smoothly.",
            EmotionTag::Joy,
            Box::new(move |f| {
                frames_cb.lock().unwrap().push(f);
            }),
        )
        .await
        .expect("speak completes cleanly on cancel");
    cancel_task.await.expect("cancel task");

    assert!(
        !pipeline_arc.is_speaking(),
        "is_speaking must reset to false after stop_speaking"
    );
    let collected = frames.lock().unwrap();
    assert_viseme_bounds(&collected);
    assert_eq!(collected.last().unwrap().mouth_open_y, 0.0);

    // PcmVisemePlayer playback + cancellation resets is_speaking cleanly.
    let pcm_player = PcmVisemePlayer::from_speaking_flag(pipeline_arc.is_speaking_flag());
    let tone: Vec<f32> = (0..16_000)
        .map(|i| 0.25 * (TAU * 300.0 * i as f32 / 16_000.0).sin())
        .collect();
    let pcm_frames = Arc::new(Mutex::new(Vec::<VisemeFrame>::new()));
    let pcm_cb = Arc::clone(&pcm_frames);
    let cancel_pcm = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(75)).await;
        let _ = pipeline_arc.stop_speaking().await;
    });
    pcm_player
        .play_pcm(
            &tone,
            16_000,
            1,
            EmotionTag::Empathetic,
            Box::new(move |f| pcm_cb.lock().unwrap().push(f)),
        )
        .await
        .expect("pcm playback completes cleanly");
    cancel_pcm.await.expect("pcm cancel task");
    assert!(!pcm_player.is_speaking());
    assert_viseme_bounds(&pcm_frames.lock().unwrap());
}

#[tokio::test]
async fn metal_whisper_real_model_smoke_if_present() {
    let model_path = Path::new(
        "/Users/goshi/.gemini/antigravity/scratch/soulmate2d/src-tauri/resources/models/asr/ggml-base.bin",
    );
    if !model_path.is_file() {
        // Provisioned binary is gitignored; skip when absent in isolated CI.
        return;
    }

    let asr_dir = model_path.parent().unwrap().to_path_buf();
    let pipeline = VoicePipeline::with_asr_dirs(Some(asr_dir), None);
    let scanned = pipeline.scan_asr_models().expect("scan real asr dir");
    let base = scanned
        .iter()
        .find(|m| m.path == model_path.to_string_lossy())
        .expect("ggml-base.bin discovered");
    assert!(base.valid);

    pipeline
        .load_asr_model(base)
        .await
        .expect("load ggml-base.bin with Metal");
    assert_eq!(
        pipeline.active_asr_model().map(|m| m.id),
        Some(base.id.clone())
    );

    // Hot-swap onto same model to verify drop-before-load works without Metal errors.
    pipeline
        .switch_asr_model(&base.id)
        .await
        .expect("hot-swap Whisper Metal model");

    // Transcribe 1s of quiet audio cleanly (should succeed without errors).
    let quiet_16k = vec![0.0f32; 16_000];
    let res = pipeline
        .transcribe_pcm(quiet_16k)
        .await
        .expect("Metal Whisper inference succeeds");
    if let Some(evt) = res {
        assert!(evt.is_final);
    }

    pipeline.unload_asr_model().await.expect("unload ASR");
    assert!(pipeline.active_asr_model().is_none());
}
