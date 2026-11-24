use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;
use rodio::{DeviceSinkBuilder, Player};
use shared_types::{
    EmotionTag, LanguagePref, SoulError, SoulResult, VisemeCallback, VisemeFrame, now_timestamp_ms,
};
use tokio::sync::Notify;
use tracing::{debug, warn};

use crate::audio::{downmix_to_mono_f32, frame_rms};

/// Target viseme frame cadence (~60 FPS = 16.667 ms).
pub const VISEME_FPS: f32 = 60.0;
pub const VISEME_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const VISEME_DT: f32 = 1.0 / VISEME_FPS;

/// Target spoken language for offline TTS voice routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceLangTarget {
    Thai,
    English,
}

/// Lightweight descriptor of an available OS/offline TTS voice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceDescriptor {
    pub id: String,
    pub name: String,
    pub language: String,
}

/// Returns `true` if `text` contains any Thai Unicode codepoints (`U+0E00..=U+0E7F`).
pub fn detect_thai_script(text: &str) -> bool {
    text.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c))
}

/// Select Thai vs English TTS voice routing based on detected script (with preference tie-break
/// for empty/neutral strings).
pub fn route_voice_language(text: &str, pref: LanguagePref) -> VoiceLangTarget {
    if detect_thai_script(text) {
        return VoiceLangTarget::Thai;
    }
    if text.trim().is_empty() && pref == LanguagePref::Thai {
        return VoiceLangTarget::Thai;
    }
    VoiceLangTarget::English
}

/// Pick the highest-priority voice matching `target` for the default feminine persona
/// (`Narisa`/`Kanya` for Thai, `Ava`/`Samantha` for English).
pub fn select_preferred_voice(
    voices: &[VoiceDescriptor],
    target: VoiceLangTarget,
) -> Option<usize> {
    select_preferred_voice_for_persona(voices, target, false)
}

/// Pick the highest-priority voice matching `target` and speaker gender preference.
///
/// For Coral (`is_male = false`): prefers `Narisa`/`Kanya` (Thai) and `Ava`/`Samantha` (English).
/// For Kai (`is_male = true`): prefers masculine Thai/English voices if available, falling back
/// gracefully to the best available language-matched voice (with deeper pitch applied during TTS).
pub fn select_preferred_voice_for_persona(
    voices: &[VoiceDescriptor],
    target: VoiceLangTarget,
    is_male: bool,
) -> Option<usize> {
    let mut best: Option<(usize, i32)> = None;
    for (idx, voice) in voices.iter().enumerate() {
        let score = score_voice_for_persona(voice, target, is_male);
        if score > 0 && best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((idx, score));
        }
    }
    best.map(|(idx, _)| idx)
}

fn score_voice_for_persona(
    voice: &VoiceDescriptor,
    target: VoiceLangTarget,
    is_male: bool,
) -> i32 {
    let name = voice.name.to_ascii_lowercase();
    let id = voice.id.to_ascii_lowercase();
    let lang = voice.language.to_ascii_lowercase().replace('_', "-");

    match target {
        VoiceLangTarget::Thai => {
            let mut score = 0;
            if lang == "th-th" {
                score += 60;
            } else if lang.starts_with("th") {
                score += 40;
            } else if id.contains("th-th") || id.contains("th_th") {
                score += 35;
            }
            if score == 0 {
                return 0;
            }
            if is_male {
                if name.contains("pattara")
                    || id.contains("pattara")
                    || name.contains("narin")
                    || id.contains("narin")
                    || name.contains("male")
                    || id.contains("male")
                {
                    score += 120;
                } else if name.contains("narisa") || id.contains("narisa") {
                    score += 80;
                } else if name.contains("kanya") || id.contains("kanya") {
                    score += 75;
                }
            } else if name.contains("narisa") || id.contains("narisa") {
                score += 110;
            } else if name.contains("kanya") || id.contains("kanya") {
                score += 105;
            }
            if name.contains("neural") || name.contains("enhanced") || name.contains("premium") {
                score += 15;
            }
            score
        }
        VoiceLangTarget::English => {
            let mut score = 0;
            if lang == "en-us" {
                score += 60;
            } else if lang.starts_with("en") {
                score += 40;
            } else if id.contains("en-us") || id.contains("en_us") {
                score += 35;
            }
            if score == 0 {
                return 0;
            }
            if is_male {
                const MALE_EN_VOICES: [&str; 9] = [
                    "daniel", "alex", "aaron", "arthur", "tom", "oliver", "rishi", "fred", "gordon",
                ];
                if MALE_EN_VOICES
                    .iter()
                    .any(|m| name.contains(m) || id.contains(m))
                {
                    score += 120;
                } else if name.contains("ava") || id.contains("ava") {
                    score += 70;
                } else if name.contains("samantha") || id.contains("samantha") {
                    score += 65;
                }
            } else if name.contains("ava") || id.contains("ava") {
                score += 110;
            } else if name.contains("samantha") || id.contains("samantha") {
                score += 105;
            }
            if name.contains("neural") || name.contains("enhanced") || name.contains("premium") {
                score += 15;
            }
            score
        }
    }
}

enum TtsCommand {
    Speak {
        text: String,
        target: VoiceLangTarget,
        is_male: bool,
        rate_scale: f32,
        pitch_scale: f32,
        reply: mpsc::Sender<bool>,
    },
    Stop {
        reply: mpsc::Sender<()>,
    },
    IsSpeaking {
        reply: mpsc::Sender<bool>,
    },
}

struct TtsActor {
    tx: mpsc::Sender<TtsCommand>,
}

static TTS_ACTOR: OnceLock<TtsActor> = OnceLock::new();

impl TtsActor {
    fn global() -> &'static Self {
        TTS_ACTOR.get_or_init(Self::spawn)
    }

    fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<TtsCommand>();
        let _ = std::thread::Builder::new()
            .name("voice-io-tts-actor".into())
            .spawn(move || run_tts_actor_loop(rx));
        Self { tx }
    }

    fn speak(
        &self,
        text: String,
        target: VoiceLangTarget,
        is_male: bool,
        rate_scale: f32,
        pitch_scale: f32,
    ) -> bool {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .tx
            .send(TtsCommand::Speak {
                text,
                target,
                is_male,
                rate_scale,
                pitch_scale,
                reply: reply_tx,
            })
            .is_err()
        {
            return false;
        }
        reply_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false)
    }

    fn stop(&self) {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self.tx.send(TtsCommand::Stop { reply: reply_tx }).is_ok() {
            let _ = reply_rx.recv_timeout(Duration::from_secs(1));
        }
    }

    fn is_speaking(&self) -> bool {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .tx
            .send(TtsCommand::IsSpeaking { reply: reply_tx })
            .is_err()
        {
            return false;
        }
        reply_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap_or(false)
    }
}

fn init_platform_tts() -> Option<(tts::Tts, Vec<VoiceDescriptor>, Vec<tts::Voice>)> {
    let res = std::panic::catch_unwind(|| {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let engine = tts::Tts::new(tts::Backends::AvFoundation)?;
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        let engine = tts::Tts::default()?;
        Ok::<tts::Tts, tts::Error>(engine)
    });

    let engine = match res {
        Ok(Ok(e)) => e,
        Ok(Err(err)) => {
            warn!("TTS backend unavailable (running headless fallback): {err}");
            return None;
        }
        Err(_) => {
            warn!("TTS backend panicked during init (running headless fallback)");
            return None;
        }
    };

    let voices = engine.voices().unwrap_or_default();
    let descriptors = voices
        .iter()
        .map(|v| VoiceDescriptor {
            id: v.id(),
            name: v.name(),
            language: v.language().to_string(),
        })
        .collect();
    debug!("Initialized offline TTS backend with {} voices", voices.len());
    Some((engine, descriptors, voices))
}

fn run_tts_actor_loop(rx: mpsc::Receiver<TtsCommand>) {
    let mut state = init_platform_tts();
    while let Ok(cmd) = rx.recv() {
        match cmd {
            TtsCommand::Speak {
                text,
                target,
                is_male,
                rate_scale,
                pitch_scale,
                reply,
            } => {
                let started = if let Some((engine, descriptors, voices)) = state.as_mut() {
                    if let Some(idx) =
                        select_preferred_voice_for_persona(descriptors, target, is_male)
                    {
                        if let Some(voice) = voices.get(idx) {
                            let _ = engine.set_voice(voice);
                        }
                    }
                    let normal_rate = engine.normal_rate();
                    let min_rate = engine.min_rate();
                    let max_rate = engine.max_rate();
                    let desired_rate = (normal_rate * rate_scale).clamp(min_rate, max_rate);
                    let _ = engine.set_rate(desired_rate);

                    let persona_pitch = if is_male { 0.92 } else { 1.0 };
                    let normal_pitch = engine.normal_pitch();
                    let min_pitch = engine.min_pitch();
                    let max_pitch = engine.max_pitch();
                    let desired_pitch =
                        (normal_pitch * pitch_scale * persona_pitch).clamp(min_pitch, max_pitch);
                    let _ = engine.set_pitch(desired_pitch);

                    match engine.speak(&text, true) {
                        Ok(_) => true,
                        Err(e) => {
                            warn!("OS TTS speak failed (falling back to visemes): {e}");
                            false
                        }
                    }
                } else {
                    false
                };
                let _ = reply.send(started);
            }
            TtsCommand::Stop { reply } => {
                if let Some((engine, _, _)) = state.as_mut() {
                    let _ = engine.stop();
                }
                let _ = reply.send(());
            }
            TtsCommand::IsSpeaking { reply } => {
                let speaking = state
                    .as_ref()
                    .and_then(|(engine, _, _)| engine.is_speaking().ok())
                    .unwrap_or(false);
                let _ = reply.send(speaking);
            }
        }
    }
}

/// Damped second-order spring smoother for natural 60 FPS mouth kinematics.
#[derive(Debug, Clone)]
pub struct VisemeSpring {
    omega: f32,
    zeta: f32,
    open_pos: f32,
    open_vel: f32,
    form_pos: f32,
    form_vel: f32,
    energy_pos: f32,
    energy_vel: f32,
}

impl Default for VisemeSpring {
    fn default() -> Self {
        Self::new(30.0, 0.85)
    }
}

impl VisemeSpring {
    pub fn new(omega: f32, zeta: f32) -> Self {
        Self {
            omega: omega.max(4.0),
            zeta: zeta.clamp(0.3, 1.5),
            open_pos: 0.0,
            open_vel: 0.0,
            form_pos: 0.0,
            form_vel: 0.0,
            energy_pos: 0.0,
            energy_vel: 0.0,
        }
    }

    pub fn step(&mut self, target_open: f32, target_form: f32, target_energy: f32, dt: f32) -> (f32, f32, f32) {
        let dt = dt.clamp(0.001, 0.05);
        let sub_steps = 2;
        let h = dt / sub_steps as f32;

        let t_open = target_open.clamp(0.0, 1.0);
        let t_form = target_form.clamp(-1.0, 1.0);
        let t_energy = target_energy.clamp(0.0, 1.0);

        for _ in 0..sub_steps {
            integrate_axis(&mut self.open_pos, &mut self.open_vel, t_open, self.omega, self.zeta, h);
            integrate_axis(&mut self.form_pos, &mut self.form_vel, t_form, self.omega, self.zeta, h);
            integrate_axis(&mut self.energy_pos, &mut self.energy_vel, t_energy, self.omega, self.zeta, h);
        }

        (
            self.open_pos.clamp(0.0, 1.0),
            self.form_pos.clamp(-1.0, 1.0),
            self.energy_pos.clamp(0.0, 1.0),
        )
    }
}

fn integrate_axis(pos: &mut f32, vel: &mut f32, target: f32, omega: f32, zeta: f32, dt: f32) {
    let accel = omega * omega * (target - *pos) - 2.0 * zeta * omega * (*vel);
    *vel += accel * dt;
    *pos += *vel * dt;
    if !pos.is_finite() {
        *pos = target;
        *vel = 0.0;
    }
}

#[derive(Debug, Clone, Copy)]
struct PhonemeSegment {
    duration_ms: f32,
    open_y: f32,
    mouth_form: f32,
    energy: f32,
}

/// Build a phonetic-prosodic target contour from Thai/English text and companion emotion.
fn build_phonetic_contour(text: &str, emotion: EmotionTag) -> Vec<PhonemeSegment> {
    let (form_bias, open_scale, energy_scale, tempo_scale) = emotion_prosody(emotion);
    let mut segments: Vec<PhonemeSegment> = Vec::new();

    for ch in text.chars() {
        if is_thai_diacritic_or_tone(ch) {
            if let Some(last) = segments.last_mut() {
                last.duration_ms = (last.duration_ms + 14.0).min(150.0);
                last.energy = (last.energy * 1.06).clamp(0.0, 1.0);
            }
            continue;
        }

        let (dur, open, form, energy) = classify_char_phoneme(ch);
        if dur <= 0.0 {
            continue;
        }
        segments.push(PhonemeSegment {
            duration_ms: dur * tempo_scale,
            open_y: (open * open_scale).clamp(0.0, 1.0),
            mouth_form: (form + form_bias).clamp(-1.0, 1.0),
            energy: (energy * energy_scale).clamp(0.0, 1.0),
        });
    }

    if segments.is_empty() {
        segments.push(PhonemeSegment {
            duration_ms: 120.0,
            open_y: 0.0,
            mouth_form: form_bias.clamp(-1.0, 1.0),
            energy: 0.0,
        });
    }

    // Smooth release tail to close lips cleanly at utterance end.
    segments.push(PhonemeSegment {
        duration_ms: 90.0,
        open_y: 0.0,
        mouth_form: 0.0,
        energy: 0.0,
    });
    segments
}

fn emotion_prosody(emotion: EmotionTag) -> (f32, f32, f32, f32) {
    match emotion {
        EmotionTag::Joy => (0.22, 1.05, 1.08, 0.94),
        EmotionTag::Sadness => (-0.18, 0.78, 0.80, 1.15),
        EmotionTag::Shy => (0.08, 0.72, 0.84, 1.06),
        EmotionTag::Surprised => (-0.12, 1.16, 1.12, 0.90),
        EmotionTag::Thinking => (-0.04, 0.82, 0.88, 1.08),
        EmotionTag::Empathetic => (0.14, 0.90, 0.92, 1.04),
        EmotionTag::Neutral => (0.0, 1.0, 1.0, 1.0),
    }
}

fn is_thai_diacritic_or_tone(ch: char) -> bool {
    matches!(
        ch,
        '\u{0E47}'..='\u{0E4E}' | '\u{0E3A}'
    )
}

fn classify_char_phoneme(ch: char) -> (f32, f32, f32, f32) {
    if ch.is_whitespace() {
        return (75.0, 0.05, 0.0, 0.06);
    }
    if matches!(ch, ',' | ';' | ':' | '—' | '-') {
        return (110.0, 0.04, 0.0, 0.05);
    }
    if matches!(ch, '.' | '!' | '?' | 'ๆ' | 'ฯ') {
        return (150.0, 0.02, 0.0, 0.03);
    }

    match ch {
        // Thai open / diphthongs
        'า' | 'ะ' | 'ำ' | '\u{0E31}' | 'แ' | 'ไ' | 'ใ' => {
            let form = if matches!(ch, 'แ' | 'ไ' | 'ใ') { 0.25 } else { 0.05 };
            (92.0, 0.88, form, 0.88)
        }
        // Thai front spread vowels
        '\u{0E34}' | '\u{0E35}' | 'เ' => (82.0, 0.44, 0.68, 0.72),
        // Thai rounded back vowels
        '\u{0E38}' | '\u{0E39}' | 'โ' | 'อ' | 'ว' => (88.0, 0.54, -0.68, 0.76),
        // Thai central vowels
        '\u{0E36}' | '\u{0E37}' => (80.0, 0.40, -0.10, 0.66),
        // Thai bilabial stops/nasals
        'บ' | 'ป' | 'พ' | 'ฟ' | 'ภ' | 'ม' => (64.0, 0.08, -0.12, 0.42),
        // Remaining Thai consonants
        '\u{0E01}'..='\u{0E2E}' => (68.0, 0.36, 0.05, 0.62),

        // English vowels
        'a' | 'A' => (88.0, 0.86, 0.12, 0.86),
        'e' | 'E' => (80.0, 0.54, 0.56, 0.76),
        'i' | 'I' | 'y' | 'Y' => (78.0, 0.38, 0.76, 0.72),
        'o' | 'O' => (88.0, 0.68, -0.66, 0.82),
        'u' | 'U' | 'w' | 'W' => (84.0, 0.38, -0.82, 0.72),

        // English bilabials & labiodentals
        'b' | 'B' | 'p' | 'P' | 'm' | 'M' => (58.0, 0.06, -0.10, 0.40),
        'f' | 'F' | 'v' | 'V' => (62.0, 0.16, -0.22, 0.46),

        // Other alphabetic / numeric characters
        c if c.is_alphanumeric() => (62.0, 0.32, 0.04, 0.58),
        _ => (0.0, 0.0, 0.0, 0.0),
    }
}

/// Generate a smoothed 60 FPS phonetic-prosodic viseme sequence for `text` and `emotion`.
pub fn generate_phonetic_visemes(
    text: &str,
    emotion: EmotionTag,
    start_timestamp_ms: i64,
) -> Vec<VisemeFrame> {
    let contour = build_phonetic_contour(text, emotion);
    let total_ms: f32 = contour.iter().map(|s| s.duration_ms).sum();
    let frame_step_ms = 1000.0 / VISEME_FPS;
    let total_frames = ((total_ms / frame_step_ms).ceil() as usize).max(1);

    let mut spring = VisemeSpring::default();
    let mut frames = Vec::with_capacity(total_frames + 1);

    let mut seg_idx = 0usize;
    let mut seg_elapsed_ms = 0.0f32;

    for frame_idx in 0..total_frames {
        let elapsed_ms = frame_idx as f32 * frame_step_ms;
        while seg_idx + 1 < contour.len() && elapsed_ms >= seg_elapsed_ms + contour[seg_idx].duration_ms {
            seg_elapsed_ms += contour[seg_idx].duration_ms;
            seg_idx += 1;
        }
        let seg = contour[seg_idx];
        let local_t = ((elapsed_ms - seg_elapsed_ms) / seg.duration_ms.max(1.0)).clamp(0.0, 1.0);
        // Bell-shaped prosodic syllable arc so consecutive vowels articulate distinctly.
        let arc = if seg.open_y > 0.15 {
            0.78 + 0.22 * (local_t * std::f32::consts::PI).sin()
        } else {
            1.0
        };
        let (open_y, mouth_form, energy) = spring.step(
            (seg.open_y * arc).clamp(0.0, 1.0),
            seg.mouth_form,
            (seg.energy * arc).clamp(0.0, 1.0),
            VISEME_DT,
        );

        frames.push(VisemeFrame {
            mouth_open_y: open_y,
            mouth_form,
            energy,
            timestamp_ms: start_timestamp_ms + elapsed_ms.round() as i64,
        });
    }

    frames.push(VisemeFrame {
        mouth_open_y: 0.0,
        mouth_form: 0.0,
        energy: 0.0,
        timestamp_ms: start_timestamp_ms + total_ms.round() as i64,
    });
    frames
}

/// Extract 60 FPS [`VisemeFrame`]s from an arbitrary PCM `f32` buffer using RMS energy and
/// spectral zero-crossing rate (ZCR), passed through [`VisemeSpring`].
pub fn extract_pcm_viseme_frames(
    mono_samples: &[f32],
    sample_rate: u32,
    start_timestamp_ms: i64,
    emotion: EmotionTag,
) -> Vec<VisemeFrame> {
    if mono_samples.is_empty() || sample_rate == 0 {
        return vec![VisemeFrame {
            mouth_open_y: 0.0,
            mouth_form: 0.0,
            energy: 0.0,
            timestamp_ms: start_timestamp_ms,
        }];
    }

    let window_len = ((sample_rate as f32) / VISEME_FPS).round().max(1.0) as usize;
    let frame_step_ms = 1000.0 / VISEME_FPS;
    let (form_bias, open_scale, energy_scale, _) = emotion_prosody(emotion);
    let mut spring = VisemeSpring::default();
    let mut frames = Vec::with_capacity(mono_samples.len().div_ceil(window_len) + 1);

    for (idx, window) in mono_samples.chunks(window_len).enumerate() {
        let rms = frame_rms(window);
        let zcr = zero_crossing_rate(window);

        let energy = ((rms * 4.2).powf(0.75) * energy_scale).clamp(0.0, 1.0);
        // Normalize ZCR against typical speech range (0..0.45 crossings/sample).
        let zcr_norm = (zcr / 0.45).clamp(0.0, 1.0);

        // Unvoiced high-frequency fricatives have higher ZCR -> narrower mouth opening & wider form.
        let fricative_damp = 1.0 - 0.35 * ((zcr_norm - 0.5).max(0.0) * 2.0);
        let target_open = if energy < 0.03 {
            0.0
        } else {
            (energy * 1.15 * fricative_damp * open_scale).clamp(0.0, 1.0)
        };

        let raw_form = if energy < 0.03 {
            0.0
        } else {
            // Low ZCR (warm voiced vowels) leans rounded (-0.55), high ZCR leans spread (+0.75).
            (zcr_norm * 1.3 - 0.55) + form_bias * 0.6
        };

        let (open_y, mouth_form, smooth_energy) =
            spring.step(target_open, raw_form.clamp(-1.0, 1.0), energy, VISEME_DT);

        frames.push(VisemeFrame {
            mouth_open_y: open_y,
            mouth_form,
            energy: smooth_energy,
            timestamp_ms: start_timestamp_ms + (idx as f32 * frame_step_ms).round() as i64,
        });
    }

    let tail_ts = start_timestamp_ms + (frames.len() as f32 * frame_step_ms).round() as i64;
    frames.push(VisemeFrame {
        mouth_open_y: 0.0,
        mouth_form: 0.0,
        energy: 0.0,
        timestamp_ms: tail_ts,
    });
    frames
}

/// Compute zero-crossing rate (`[0.0, 1.0]`) for a mono audio window.
pub fn zero_crossing_rate(samples: &[f32]) -> f32 {
    if samples.len() < 2 {
        return 0.0;
    }
    let crossings = samples
        .windows(2)
        .filter(|w| (w[0] >= 0.0 && w[1] < 0.0) || (w[0] < 0.0 && w[1] >= 0.0))
        .count();
    crossings as f32 / (samples.len() - 1) as f32
}

/// Generation-guarded speaking state so a cancelling utterance can never clear a newer one.
#[derive(Clone)]
pub struct SpeakingGate {
    is_speaking: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}

impl SpeakingGate {
    pub fn new(is_speaking: Arc<AtomicBool>) -> Self {
        Self {
            is_speaking,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.is_speaking)
    }

    pub fn is_speaking(&self) -> bool {
        self.is_speaking.load(Ordering::Acquire)
    }

    pub fn acquire(&self) -> SpeakingFlagGuard {
        let my_gen = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.is_speaking.store(true, Ordering::Release);
        SpeakingFlagGuard {
            is_speaking: Arc::clone(&self.is_speaking),
            generation: Arc::clone(&self.generation),
            my_gen,
        }
    }

    pub fn force_clear(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.is_speaking.store(false, Ordering::Release);
    }
}

/// Guard ensuring `is_speaking` resets only if this utterance is still the active generation.
pub struct SpeakingFlagGuard {
    is_speaking: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    my_gen: u64,
}

impl Drop for SpeakingFlagGuard {
    fn drop(&mut self) {
        if self.generation.load(Ordering::Acquire) == self.my_gen {
            self.is_speaking.store(false, Ordering::Release);
        }
    }
}

#[derive(Clone)]
struct SpeechCancelHandle {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl SpeechCancelHandle {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

async fn stream_viseme_frames(
    frames: &[VisemeFrame],
    cancel: &SpeechCancelHandle,
    on_viseme: &mut VisemeCallback,
) {
    let start_instant = Instant::now();
    for (idx, frame) in frames.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        if idx > 0 {
            let target_elapsed = VISEME_FRAME_INTERVAL.saturating_mul(idx as u32);
            if let Some(wait) = target_elapsed.checked_sub(start_instant.elapsed()) {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = cancel.notify.notified() => break,
                }
            }
        }
        if cancel.is_cancelled() {
            break;
        }
        on_viseme(*frame);
    }
}

fn emit_neutral_viseme(on_viseme: &mut VisemeCallback) {
    on_viseme(VisemeFrame {
        mouth_open_y: 0.0,
        mouth_form: 0.0,
        energy: 0.0,
        timestamp_ms: now_timestamp_ms(),
    });
}

fn clear_cancel_handle(
    slot: &Mutex<Option<SpeechCancelHandle>>,
    handle: &SpeechCancelHandle,
) {
    if let Ok(mut guard) = slot.lock() {
        if guard
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(&c.cancelled, &handle.cancelled))
        {
            *guard = None;
        }
    }
}

/// Coordinator for offline `AVFoundation` TTS playback + 60 FPS phonetic-prosodic visemes.
pub struct TtsSpeechCoordinator {
    gate: SpeakingGate,
    active_cancel: Mutex<Option<SpeechCancelHandle>>,
    is_male: AtomicBool,
}

impl TtsSpeechCoordinator {
    pub fn new(gate: SpeakingGate) -> Self {
        Self {
            gate,
            active_cancel: Mutex::new(None),
            is_male: AtomicBool::new(false),
        }
    }

    /// Set whether TTS should prefer masculine voice profiles / deeper pitch (`Kai`)
    /// vs feminine voice profiles (`Coral`, default).
    pub fn set_voice_persona(&self, is_male: bool) {
        self.is_male.store(is_male, Ordering::Release);
    }

    /// Returns `true` when the masculine (`Kai`) TTS persona is active.
    pub fn is_male_persona(&self) -> bool {
        self.is_male.load(Ordering::Acquire)
    }

    pub async fn speak(
        &self,
        text: &str,
        emotion: EmotionTag,
        pref: LanguagePref,
        mut on_viseme: VisemeCallback,
    ) -> SoulResult<()> {
        self.stop_speaking();

        let trimmed = text.trim();
        if trimmed.is_empty() {
            emit_neutral_viseme(&mut on_viseme);
            return Ok(());
        }

        let cancel = SpeechCancelHandle::new();
        {
            let mut guard = self
                .active_cancel
                .lock()
                .map_err(|_| SoulError::Internal("active_cancel mutex poisoned".into()))?;
            *guard = Some(cancel.clone());
        }

        let _speaking_guard = self.gate.acquire();

        let target = route_voice_language(trimmed, pref);
        let is_male = self.is_male_persona();
        let (rate_scale, pitch_scale) = match emotion {
            EmotionTag::Joy => (1.05, 1.06),
            EmotionTag::Sadness => (0.90, 0.94),
            EmotionTag::Shy => (0.96, 1.03),
            EmotionTag::Surprised => (1.08, 1.10),
            _ => (1.0, 1.0),
        };

        let actor = TtsActor::global();
        let text_owned = trimmed.to_string();
        let os_tts_active = tokio::task::spawn_blocking(move || {
            actor.speak(text_owned, target, is_male, rate_scale, pitch_scale)
        })
        .await
        .unwrap_or(false);

        let frames = generate_phonetic_visemes(trimmed, emotion, now_timestamp_ms());
        stream_viseme_frames(&frames, &cancel, &mut on_viseme).await;

        // If the OS synthesizer is still articulating slightly past the phonetic timeline,
        // emit gentle settled frames until it finishes (with a strict safety timeout).
        if os_tts_active && !cancel.is_cancelled() {
            let max_tail = Instant::now() + Duration::from_secs(4);
            let mut tail_spring = VisemeSpring::default();
            let mut phase = 0.0f32;
            while Instant::now() < max_tail && !cancel.is_cancelled() {
                let still_speaking = tokio::task::spawn_blocking(|| TtsActor::global().is_speaking())
                    .await
                    .unwrap_or(false);
                if !still_speaking {
                    break;
                }
                phase += 0.35;
                let target_open = (0.28 + 0.16 * phase.sin()).clamp(0.0, 1.0);
                let (open_y, mouth_form, energy) = tail_spring.step(target_open, 0.05, 0.35, VISEME_DT);
                on_viseme(VisemeFrame {
                    mouth_open_y: open_y,
                    mouth_form,
                    energy,
                    timestamp_ms: now_timestamp_ms(),
                });
                tokio::select! {
                    _ = tokio::time::sleep(VISEME_FRAME_INTERVAL) => {}
                    _ = cancel.notify.notified() => break,
                }
            }
        }

        if cancel.is_cancelled() {
            let _ = tokio::task::spawn_blocking(|| TtsActor::global().stop()).await;
            emit_neutral_viseme(&mut on_viseme);
        }

        clear_cancel_handle(&self.active_cancel, &cancel);
        Ok(())
    }

    pub fn stop_speaking(&self) {
        if let Ok(mut guard) = self.active_cancel.lock() {
            if let Some(cancel) = guard.take() {
                cancel.cancel();
            }
        }
        TtsActor::global().stop();
        self.gate.force_clear();
    }
}

/// Offline PCM audio player with real-time RMS & spectral zero-crossing 60 FPS viseme extraction.
///
/// Designed for playing local synthesized/recorded PCM buffers (e.g., from local `.onnx` voice
/// assets) without any network calls or `sherpa-rs`.
pub struct PcmVisemePlayer {
    gate: SpeakingGate,
    active_cancel: Mutex<Option<SpeechCancelHandle>>,
}

impl Default for PcmVisemePlayer {
    fn default() -> Self {
        Self::new(SpeakingGate::new(Arc::new(AtomicBool::new(false))))
    }
}

impl PcmVisemePlayer {
    pub fn new(gate: SpeakingGate) -> Self {
        Self {
            gate,
            active_cancel: Mutex::new(None),
        }
    }

    pub fn from_speaking_flag(is_speaking: Arc<AtomicBool>) -> Self {
        Self::new(SpeakingGate::new(is_speaking))
    }

    pub fn is_speaking(&self) -> bool {
        self.gate.is_speaking()
    }

    /// Analyze a mono PCM buffer without audio playback and return its 60 FPS viseme frames.
    pub fn analyze_pcm(
        &self,
        mono_samples: &[f32],
        sample_rate: u32,
        emotion: EmotionTag,
    ) -> SoulResult<Vec<VisemeFrame>> {
        if sample_rate == 0 {
            return Err(SoulError::InvalidInput(
                "Sample rate must be greater than zero".into(),
            ));
        }
        Ok(extract_pcm_viseme_frames(
            mono_samples,
            sample_rate,
            now_timestamp_ms(),
            emotion,
        ))
    }

    /// Play a local interleaved PCM `f32` buffer via `rodio` (falling back gracefully in headless
    /// environments without an audio output device) while streaming synchronized 60 FPS
    /// [`VisemeFrame`]s and maintaining the `is_speaking` echo-suppression gate.
    pub async fn play_pcm(
        &self,
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
        emotion: EmotionTag,
        mut on_viseme: VisemeCallback,
    ) -> SoulResult<()> {
        let nz_channels = NonZero::new(channels).ok_or_else(|| {
            SoulError::InvalidInput("PCM channel count must be greater than zero".into())
        })?;
        let nz_rate = NonZero::new(sample_rate).ok_or_else(|| {
            SoulError::InvalidInput("PCM sample rate must be greater than zero".into())
        })?;

        self.stop();

        if samples.is_empty() {
            emit_neutral_viseme(&mut on_viseme);
            return Ok(());
        }

        let cancel = SpeechCancelHandle::new();
        {
            let mut guard = self
                .active_cancel
                .lock()
                .map_err(|_| SoulError::Internal("PcmVisemePlayer cancel lock poisoned".into()))?;
            *guard = Some(cancel.clone());
        }

        let _speaking_guard = self.gate.acquire();

        let mono = downmix_to_mono_f32(samples, usize::from(channels));
        let frames = extract_pcm_viseme_frames(&mono, sample_rate, now_timestamp_ms(), emotion);

        let audio_samples = samples.to_vec();
        let output_handle = tokio::task::spawn_blocking(move || {
            match DeviceSinkBuilder::open_default_sink() {
                Ok(mut sink) => {
                    sink.log_on_drop(false);
                    let player = Player::connect_new(sink.mixer());
                    let source = SamplesBuffer::new(nz_channels, nz_rate, audio_samples);
                    player.append(source);
                    Some((sink, player))
                }
                Err(err) => {
                    warn!("Audio output unavailable (running headless viseme playback): {err}");
                    None
                }
            }
        })
        .await
        .unwrap_or(None);

        stream_viseme_frames(&frames, &cancel, &mut on_viseme).await;

        if let Some((_sink, player)) = output_handle {
            if cancel.is_cancelled() {
                player.stop();
                emit_neutral_viseme(&mut on_viseme);
            } else if !player.empty() {
                let wait_cancel = cancel.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    while !player.empty() && !wait_cancel.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if wait_cancel.is_cancelled() {
                        player.stop();
                    }
                })
                .await;
            }
        } else if cancel.is_cancelled() {
            emit_neutral_viseme(&mut on_viseme);
        }

        clear_cancel_handle(&self.active_cancel, &cancel);
        Ok(())
    }

    pub fn stop(&self) {
        if let Ok(mut guard) = self.active_cancel.lock() {
            if let Some(cancel) = guard.take() {
                cancel.cancel();
            }
        }
        self.gate.force_clear();
    }
}
