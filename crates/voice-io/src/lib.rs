pub mod asr;
pub mod audio;
pub mod tts_viseme;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use shared_types::{
    EmotionTag, LanguagePref, ModelInfo, SoulError, SoulResult, TranscriptCallback,
    TranscriptEvent, VisemeCallback, VoiceEngine, VoiceMode,
};
use tracing::warn;

pub use asr::{AsrManager, classify_ram_safety, estimate_asr_ram_mb, has_valid_ggml_magic};
pub use audio::{
    AdaptiveVad, MicCaptureHandle, MonoResampler, TARGET_SAMPLE_RATE, VAD_FRAME_SAMPLES,
    VadConfig, VadDecision, cpal_to_mono_f32, decode_voice_mode, downmix_to_mono_f32,
    encode_voice_mode, frame_rms,
};
pub use tts_viseme::{
    PcmVisemePlayer, SpeakingGate, TtsSpeechCoordinator, VISEME_FPS, VISEME_FRAME_INTERVAL,
    VisemeSpring, VoiceDescriptor, VoiceLangTarget, detect_thai_script, extract_pcm_viseme_frames,
    generate_phonetic_visemes, route_voice_language, select_preferred_voice, zero_crossing_rate,
};

fn encode_language_pref(pref: LanguagePref) -> u8 {
    match pref {
        LanguagePref::Auto => 0,
        LanguagePref::Thai => 1,
        LanguagePref::English => 2,
    }
}

fn decode_language_pref(raw: u8) -> LanguagePref {
    match raw {
        1 => LanguagePref::Thai,
        2 => LanguagePref::English,
        _ => LanguagePref::Auto,
    }
}

/// Complete local Voice I/O pipeline combining `cpal` mic capture, `rubato` 16 kHz resampling,
/// adaptive noise-floor VAD with strict echo-suppression gating, Metal Whisper ASR, offline
/// macOS `AVFoundation` TTS, 60 FPS phonetic-prosodic lip-sync visemes, and `PcmVisemePlayer`.
pub struct VoicePipeline {
    asr: Arc<AsrManager>,
    voice_mode: Arc<AtomicU8>,
    language_pref: Arc<AtomicU8>,
    speaking_gate: SpeakingGate,
    tts: TtsSpeechCoordinator,
    pcm_player: PcmVisemePlayer,
    mic_handle: Mutex<Option<MicCaptureHandle>>,
}

impl Default for VoicePipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for VoicePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoicePipeline")
            .field("active_asr", &self.active_asr_model())
            .field("voice_mode", &self.voice_mode())
            .field("language_pref", &self.language_pref())
            .field("is_speaking", &self.is_speaking())
            .field("is_listening", &self.is_listening())
            .finish_non_exhaustive()
    }
}

impl VoicePipeline {
    /// Construct a new `VoicePipeline` with auto-discovered bundled and user ASR directories.
    pub fn new() -> Self {
        Self::with_asr_dirs(None, None)
    }

    /// Construct a `VoicePipeline` with explicit bundled and user ASR model directories.
    pub fn with_asr_dirs(bundled_dir: Option<PathBuf>, user_dir: Option<PathBuf>) -> Self {
        let is_speaking = Arc::new(AtomicBool::new(false));
        let speaking_gate = SpeakingGate::new(is_speaking);
        let tts = TtsSpeechCoordinator::new(speaking_gate.clone());
        let pcm_player = PcmVisemePlayer::new(speaking_gate.clone());
        Self {
            asr: Arc::new(AsrManager::new(bundled_dir, user_dir)),
            voice_mode: Arc::new(AtomicU8::new(encode_voice_mode(VoiceMode::default()))),
            language_pref: Arc::new(AtomicU8::new(encode_language_pref(LanguagePref::default()))),
            speaking_gate,
            tts,
            pcm_player,
            mic_handle: Mutex::new(None),
        }
    }

    /// Override bundled and user ASR scan directories after construction.
    pub fn set_asr_dirs(&self, bundled_dir: Option<PathBuf>, user_dir: Option<PathBuf>) {
        self.asr.set_directories(bundled_dir, user_dir);
    }

    /// Shared echo-suppression gate flag (`true` while TTS or PCM audio playback is active).
    pub fn is_speaking_flag(&self) -> Arc<AtomicBool> {
        self.speaking_gate.flag()
    }

    /// Whether TTS or PCM voice output is currently speaking.
    pub fn is_speaking(&self) -> bool {
        self.speaking_gate.is_speaking()
    }

    /// Whether microphone capture is currently active.
    pub fn is_listening(&self) -> bool {
        self.mic_handle
            .lock()
            .ok()
            .is_some_and(|guard| guard.is_some())
    }

    /// Discover and validate bundled, user, and custom Whisper GGML `.bin` ASR models.
    pub fn scan_asr_models(&self) -> SoulResult<Vec<ModelInfo>> {
        self.asr.scan_asr_models()
    }

    /// Validate and register a custom Whisper GGML `.bin` model file.
    pub fn register_custom_asr_model(
        &self,
        path: &Path,
        name: Option<&str>,
    ) -> SoulResult<ModelInfo> {
        self.asr.register_custom_model(path, name)
    }

    /// Scan available ASR models, find `model_id`, and hot-swap to it (dropping any previous
    /// Whisper Metal context first).
    pub async fn switch_asr_model(&self, model_id: &str) -> SoulResult<ModelInfo> {
        let models = self.scan_asr_models()?;
        let target = models
            .into_iter()
            .find(|m| m.id == model_id)
            .ok_or_else(|| SoulError::ModelNotFound(format!("ASR model not found: {model_id}")))?;
        self.load_asr_model(&target).await?;
        self.active_asr_model().ok_or_else(|| {
            SoulError::Internal("ASR model reported loaded but active_asr_model is None".into())
        })
    }

    /// Unload the active Whisper ASR model and free Metal buffers.
    pub async fn unload_asr_model(&self) -> SoulResult<()> {
        self.asr.unload_asr_model().await
    }

    /// Run Whisper ASR directly on an in-memory 16,000 Hz mono `f32` audio buffer.
    pub async fn transcribe_pcm(&self, pcm_16k: Vec<f32>) -> SoulResult<Option<TranscriptEvent>> {
        self.asr.transcribe_pcm(pcm_16k, self.language_pref()).await
    }

    /// Play a local PCM buffer (e.g., synthesized by a local `.onnx` voice asset) while emitting
    /// real-time RMS/zero-crossing 60 FPS visemes and enforcing the `is_speaking` echo gate.
    pub async fn play_pcm(
        &self,
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
        emotion: EmotionTag,
        on_viseme: VisemeCallback,
    ) -> SoulResult<()> {
        self.tts.stop_speaking();
        self.pcm_player
            .play_pcm(samples, sample_rate, channels, emotion, on_viseme)
            .await
    }

    /// Access the secondary offline [`PcmVisemePlayer`].
    pub fn pcm_player(&self) -> &PcmVisemePlayer {
        &self.pcm_player
    }
}

#[async_trait]
impl VoiceEngine for VoicePipeline {
    async fn load_asr_model(&self, model: &ModelInfo) -> SoulResult<()> {
        self.asr.load_asr_model(model).await
    }

    fn active_asr_model(&self) -> Option<ModelInfo> {
        self.asr.active_model()
    }

    async fn set_voice_mode(&self, mode: VoiceMode) -> SoulResult<()> {
        self.voice_mode
            .store(encode_voice_mode(mode), Ordering::Release);
        Ok(())
    }

    fn voice_mode(&self) -> VoiceMode {
        decode_voice_mode(self.voice_mode.load(Ordering::Acquire))
    }

    async fn set_language_pref(&self, pref: LanguagePref) -> SoulResult<()> {
        self.language_pref
            .store(encode_language_pref(pref), Ordering::Release);
        Ok(())
    }

    fn language_pref(&self) -> LanguagePref {
        decode_language_pref(self.language_pref.load(Ordering::Acquire))
    }

    async fn start_listening(&self, mut on_transcript: TranscriptCallback) -> SoulResult<()> {
        self.stop_listening().await?;

        if self.voice_mode() == VoiceMode::Muted {
            return Err(SoulError::Voice(
                "Microphone is muted; switch voice mode before listening".into(),
            ));
        }

        let asr = Arc::clone(&self.asr);
        let lang_atomic = Arc::clone(&self.language_pref);
        let mode_atomic = Arc::clone(&self.voice_mode);
        let is_speaking = self.is_speaking_flag();

        let handle = MicCaptureHandle::start(mode_atomic, is_speaking, move |utterance| {
            let pref = decode_language_pref(lang_atomic.load(Ordering::Acquire));
            match asr.transcribe_pcm_blocking(&utterance, pref) {
                Ok(Some(event)) => on_transcript(event),
                Ok(None) => {}
                Err(err) => {
                    warn!("ASR utterance transcription skipped: {err}");
                }
            }
        })?;

        let mut guard = self
            .mic_handle
            .lock()
            .map_err(|_| SoulError::Internal("mic_handle mutex poisoned".into()))?;
        *guard = Some(handle);
        Ok(())
    }

    async fn stop_listening(&self) -> SoulResult<()> {
        let handle = {
            let mut guard = self
                .mic_handle
                .lock()
                .map_err(|_| SoulError::Internal("mic_handle mutex poisoned".into()))?;
            guard.take()
        };

        if let Some(handle) = handle {
            tokio::task::spawn_blocking(move || handle.stop())
                .await
                .map_err(|e| SoulError::Internal(format!("Mic capture stop join failed: {e}")))?;
        }
        Ok(())
    }

    async fn speak(
        &self,
        text: &str,
        emotion: EmotionTag,
        on_viseme: VisemeCallback,
    ) -> SoulResult<()> {
        self.pcm_player.stop();
        self.tts
            .speak(text, emotion, self.language_pref(), on_viseme)
            .await
    }

    async fn stop_speaking(&self) -> SoulResult<()> {
        self.tts.stop_speaking();
        self.pcm_player.stop();
        Ok(())
    }
}
