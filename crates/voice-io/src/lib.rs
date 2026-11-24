use std::sync::Mutex;

use async_trait::async_trait;
use shared_types::{
    EmotionTag, LanguagePref, ModelInfo, SoulError, SoulResult, TranscriptCallback, VisemeCallback,
    VoiceEngine, VoiceMode,
};

/// Local ASR, VAD, TTS, and viseme pipeline stub (implemented in Phase 2).
#[derive(Debug, Default)]
pub struct VoicePipeline {
    active_asr: Mutex<Option<ModelInfo>>,
    voice_mode: Mutex<VoiceMode>,
    language_pref: Mutex<LanguagePref>,
}

impl VoicePipeline {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl VoiceEngine for VoicePipeline {
    async fn load_asr_model(&self, model: &ModelInfo) -> SoulResult<()> {
        let mut guard = self
            .active_asr
            .lock()
            .map_err(|_| SoulError::Internal("active_asr mutex poisoned".into()))?;
        *guard = Some(model.clone());
        Err(SoulError::NotImplemented(
            "VoicePipeline::load_asr_model backend is not implemented yet".into(),
        ))
    }

    fn active_asr_model(&self) -> Option<ModelInfo> {
        self.active_asr.lock().ok().and_then(|g| g.clone())
    }

    async fn set_voice_mode(&self, mode: VoiceMode) -> SoulResult<()> {
        let mut guard = self
            .voice_mode
            .lock()
            .map_err(|_| SoulError::Internal("voice_mode mutex poisoned".into()))?;
        *guard = mode;
        Ok(())
    }

    fn voice_mode(&self) -> VoiceMode {
        self.voice_mode
            .lock()
            .map(|g| *g)
            .unwrap_or_default()
    }

    async fn set_language_pref(&self, pref: LanguagePref) -> SoulResult<()> {
        let mut guard = self
            .language_pref
            .lock()
            .map_err(|_| SoulError::Internal("language_pref mutex poisoned".into()))?;
        *guard = pref;
        Ok(())
    }

    fn language_pref(&self) -> LanguagePref {
        self.language_pref
            .lock()
            .map(|g| *g)
            .unwrap_or_default()
    }

    async fn start_listening(&self, _on_transcript: TranscriptCallback) -> SoulResult<()> {
        Err(SoulError::NotImplemented(
            "VoicePipeline::start_listening is not implemented yet".into(),
        ))
    }

    async fn stop_listening(&self) -> SoulResult<()> {
        Ok(())
    }

    async fn speak(
        &self,
        _text: &str,
        _emotion: EmotionTag,
        _on_viseme: VisemeCallback,
    ) -> SoulResult<()> {
        Err(SoulError::NotImplemented(
            "VoicePipeline::speak is not implemented yet".into(),
        ))
    }

    async fn stop_speaking(&self) -> SoulResult<()> {
        Ok(())
    }
}
