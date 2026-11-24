use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use shared_types::{SoulError, SoulResult, VoiceMode};
use tracing::{debug, warn};

/// Target mono sample rate expected by Whisper ASR.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Default VAD frame duration in samples at 16 kHz (20 ms).
pub const VAD_FRAME_SAMPLES: usize = 320;

/// Default resampler input chunk size in frames.
const RESAMPLER_CHUNK_FRAMES: usize = 1024;

/// Downmix interleaved multi-channel `f32` audio into mono `f32` by averaging channels.
pub fn downmix_to_mono_f32(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let inv = 1.0 / channels as f32;
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() * inv)
        .collect()
}

/// Convert interleaved arbitrary `cpal::SizedSample` input into mono `f32` samples.
pub fn cpal_to_mono_f32<T: cpal::SizedSample>(interleaved: &[T], channels: usize) -> Vec<f32>
where
    f32: cpal::FromSample<T>,
{
    if channels <= 1 {
        return interleaved.iter().map(|s| s.to_sample::<f32>()).collect();
    }
    let inv = 1.0 / channels as f32;
    interleaved
        .chunks_exact(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum();
            sum * inv
        })
        .collect()
}

/// Compute root-mean-square energy for a slice of mono `f32` samples.
pub fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Resampler converting native mono `f32` audio streams to 16,000 Hz mono PCM via `rubato`.
pub struct MonoResampler {
    input_sample_rate: u32,
    inner: Option<Async<f32>>,
    pending: Vec<f32>,
    out_scratch: Vec<f32>,
}

impl MonoResampler {
    /// Construct a new resampler targeting `TARGET_SAMPLE_RATE` (16,000 Hz).
    pub fn new(input_sample_rate: u32) -> SoulResult<Self> {
        if input_sample_rate == 0 {
            return Err(SoulError::InvalidInput(
                "Input sample rate must be greater than zero".into(),
            ));
        }
        if input_sample_rate == TARGET_SAMPLE_RATE {
            return Ok(Self {
                input_sample_rate,
                inner: None,
                pending: Vec::new(),
                out_scratch: Vec::new(),
            });
        }

        let ratio = f64::from(TARGET_SAMPLE_RATE) / f64::from(input_sample_rate);
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: None,
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };
        let chunk_size = RESAMPLER_CHUNK_FRAMES;
        let resampler = Async::<f32>::new_sinc(
            ratio,
            1.1,
            &params,
            chunk_size,
            1,
            FixedAsync::Input,
        )
        .map_err(|e| SoulError::Voice(format!("Failed to create rubato resampler: {e}")))?;

        let out_max = resampler.output_frames_max();
        Ok(Self {
            input_sample_rate,
            inner: Some(resampler),
            pending: Vec::with_capacity(chunk_size * 2),
            out_scratch: vec![0.0; out_max.max(chunk_size)],
        })
    }

    pub fn input_sample_rate(&self) -> u32 {
        self.input_sample_rate
    }

    /// Push a chunk of mono `f32` audio at `input_sample_rate` and return any newly produced
    /// 16,000 Hz mono samples.
    pub fn push(&mut self, mono_samples: &[f32]) -> SoulResult<Vec<f32>> {
        if mono_samples.is_empty() {
            return Ok(Vec::new());
        }
        let Some(resampler) = self.inner.as_mut() else {
            return Ok(mono_samples.to_vec());
        };

        self.pending.extend_from_slice(mono_samples);
        let mut output = Vec::new();

        loop {
            let needed = resampler.input_frames_next();
            if self.pending.len() < needed {
                break;
            }
            let max_out = resampler.output_frames_next();
            if self.out_scratch.len() < max_out {
                self.out_scratch.resize(max_out, 0.0);
            }

            let adapter_in = InterleavedSlice::new(&self.pending[..needed], 1, needed)
                .map_err(|e| SoulError::Voice(format!("Resampler input adapter error: {e}")))?;
            let mut adapter_out =
                InterleavedSlice::new_mut(&mut self.out_scratch[..max_out], 1, max_out).map_err(
                    |e| SoulError::Voice(format!("Resampler output adapter error: {e}")),
                )?;

            let (consumed, produced) = resampler
                .process_into_buffer(&adapter_in, &mut adapter_out, None)
                .map_err(|e| SoulError::Voice(format!("Resampler process error: {e}")))?;

            output.extend_from_slice(&self.out_scratch[..produced]);
            self.pending.drain(..consumed);
        }

        Ok(output)
    }

    /// Flush any trailing buffered frames through the resampler at end-of-stream.
    pub fn flush(&mut self) -> SoulResult<Vec<f32>> {
        let Some(resampler) = self.inner.as_mut() else {
            return Ok(std::mem::take(&mut self.pending));
        };
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }

        let remaining = self.pending.len();
        let needed = resampler.input_frames_next();
        let pad_len = needed.max(remaining);
        self.pending.resize(pad_len, 0.0);

        let max_out = resampler.output_frames_next();
        if self.out_scratch.len() < max_out {
            self.out_scratch.resize(max_out, 0.0);
        }

        let indexing = Indexing::new().partial_len(remaining);
        let adapter_in = InterleavedSlice::new(&self.pending[..pad_len], 1, pad_len)
            .map_err(|e| SoulError::Voice(format!("Resampler flush input error: {e}")))?;
        let mut adapter_out =
            InterleavedSlice::new_mut(&mut self.out_scratch[..max_out], 1, max_out)
                .map_err(|e| SoulError::Voice(format!("Resampler flush output error: {e}")))?;

        let (_, produced) = resampler
            .process_into_buffer(&adapter_in, &mut adapter_out, Some(&indexing))
            .map_err(|e| SoulError::Voice(format!("Resampler flush process error: {e}")))?;

        self.pending.clear();
        let expected_valid =
            ((remaining as f64) * resampler.resample_ratio()).round() as usize;
        let take_len = produced.min(expected_valid.max(1));
        Ok(self.out_scratch[..take_len].to_vec())
    }

    /// Reset internal filter and buffer state.
    pub fn reset(&mut self) {
        self.pending.clear();
        if let Some(resampler) = self.inner.as_mut() {
            resampler.reset();
        }
    }

    /// Resample an entire in-memory mono clip from `input_sample_rate` to 16,000 Hz cleanly,
    /// trimming startup delay.
    pub fn resample_clip(input_sample_rate: u32, mono_samples: &[f32]) -> SoulResult<Vec<f32>> {
        if mono_samples.is_empty() {
            return Ok(Vec::new());
        }
        if input_sample_rate == TARGET_SAMPLE_RATE {
            return Ok(mono_samples.to_vec());
        }
        let mut resampler = Self::new(input_sample_rate)?;
        let inner = resampler
            .inner
            .as_mut()
            .expect("inner resampler present when rate != 16k");
        let input_adapter = InterleavedSlice::new(mono_samples, 1, mono_samples.len())
            .map_err(|e| SoulError::Voice(format!("Resampler clip adapter error: {e}")))?;
        let out = inner
            .process_all(&input_adapter, mono_samples.len(), None)
            .map_err(|e| SoulError::Voice(format!("Resampler process_all error: {e}")))?;
        Ok(out.take_data())
    }
}

/// Configuration parameters for [`AdaptiveVad`].
#[derive(Debug, Clone)]
pub struct VadConfig {
    pub frame_samples: usize,
    pub initial_noise_floor: f32,
    pub min_noise_floor: f32,
    pub max_noise_floor: f32,
    pub noise_ema_alpha: f32,
    pub speech_multiplier: f32,
    pub min_speech_rms: f32,
    pub hangover_ratio: f32,
    pub onset_frames: usize,
    pub hangover_silence_frames: usize,
    pub pre_roll_frames: usize,
    pub min_utterance_samples: usize,
    pub max_utterance_samples: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_samples: VAD_FRAME_SAMPLES,
            initial_noise_floor: 0.004,
            min_noise_floor: 0.001,
            max_noise_floor: 0.06,
            noise_ema_alpha: 0.08,
            speech_multiplier: 3.0,
            min_speech_rms: 0.015,
            hangover_ratio: 0.65,
            onset_frames: 3,
            hangover_silence_frames: 20,
            pre_roll_frames: 8,
            min_utterance_samples: 3_200,
            max_utterance_samples: 480_000,
        }
    }
}

/// Result of processing a single 16 kHz mono frame through [`AdaptiveVad`].
#[derive(Debug, Clone, PartialEq)]
pub enum VadDecision {
    /// Audio discarded because TTS is speaking (`is_speaking == true`) or mode is `Muted`.
    Suppressed,
    /// Background noise / silence frame (used to track adaptive noise floor).
    Silence,
    /// Active speech frame currently accumulating into an utterance buffer.
    SpeechActive,
    /// Speech utterance completed and ready for ASR transcription.
    UtteranceReady(Vec<f32>),
}

/// Adaptive noise-floor Voice Activity Detector with echo-suppression gating.
#[derive(Debug, Clone)]
pub struct AdaptiveVad {
    config: VadConfig,
    noise_floor: f32,
    in_speech: bool,
    onset_count: usize,
    silence_count: usize,
    pre_roll: VecDeque<Vec<f32>>,
    speech_buf: Vec<f32>,
    frame_accum: Vec<f32>,
}

impl Default for AdaptiveVad {
    fn default() -> Self {
        Self::new(VadConfig::default())
    }
}

impl AdaptiveVad {
    pub fn new(config: VadConfig) -> Self {
        let pre_roll_cap = config.pre_roll_frames.max(config.onset_frames);
        let initial_floor = config
            .initial_noise_floor
            .clamp(config.min_noise_floor, config.max_noise_floor);
        Self {
            config,
            noise_floor: initial_floor,
            in_speech: false,
            onset_count: 0,
            silence_count: 0,
            pre_roll: VecDeque::with_capacity(pre_roll_cap + 1),
            speech_buf: Vec::new(),
            frame_accum: Vec::with_capacity(VAD_FRAME_SAMPLES * 2),
        }
    }

    pub fn noise_floor(&self) -> f32 {
        self.noise_floor
    }

    pub fn is_speech_active(&self) -> bool {
        self.in_speech || !self.speech_buf.is_empty()
    }

    pub fn current_speech_threshold(&self) -> f32 {
        (self.noise_floor * self.config.speech_multiplier).max(self.config.min_speech_rms)
    }

    /// Clear buffered speech state while preserving the learned ambient noise floor.
    pub fn reset_utterance(&mut self) {
        self.in_speech = false;
        self.onset_count = 0;
        self.silence_count = 0;
        self.pre_roll.clear();
        self.speech_buf.clear();
        self.frame_accum.clear();
    }

    /// Process a single mono 16 kHz frame (typically `config.frame_samples` = 320 samples).
    ///
    /// When `is_speaking` is `true` (TTS active) or `mode == VoiceMode::Muted`, frames are
    /// immediately suppressed and discarded so the companion never hears herself and ambient
    /// noise estimation is never corrupted.
    pub fn process_frame(
        &mut self,
        frame: &[f32],
        mode: VoiceMode,
        is_speaking: bool,
    ) -> VadDecision {
        if frame.is_empty() {
            return VadDecision::Silence;
        }

        if is_speaking || mode == VoiceMode::Muted {
            self.in_speech = false;
            self.onset_count = 0;
            self.silence_count = 0;
            self.pre_roll.clear();
            self.speech_buf.clear();
            return VadDecision::Suppressed;
        }

        if mode == VoiceMode::PushToTalk {
            self.in_speech = true;
            self.speech_buf.extend_from_slice(frame);
            if self.speech_buf.len() >= self.config.max_utterance_samples {
                let utt = std::mem::take(&mut self.speech_buf);
                self.in_speech = false;
                return VadDecision::UtteranceReady(utt);
            }
            return VadDecision::SpeechActive;
        }

        // HandsFree adaptive energy VAD
        let rms = frame_rms(frame);
        let speech_threshold = self.current_speech_threshold();
        let continue_threshold =
            (speech_threshold * self.config.hangover_ratio).max(self.config.min_speech_rms * 0.65);

        if !self.in_speech {
            let cap = self.config.pre_roll_frames.max(self.config.onset_frames);
            if cap > 0 {
                if self.pre_roll.len() >= cap {
                    self.pre_roll.pop_front();
                }
                self.pre_roll.push_back(frame.to_vec());
            }

            if rms >= speech_threshold {
                self.onset_count += 1;
                if self.onset_count >= self.config.onset_frames.max(1) {
                    self.in_speech = true;
                    self.silence_count = 0;
                    self.speech_buf.clear();
                    for pre in self.pre_roll.drain(..) {
                        self.speech_buf.extend_from_slice(&pre);
                    }
                    if self.config.pre_roll_frames == 0 && self.config.onset_frames == 0 {
                        self.speech_buf.extend_from_slice(frame);
                    }
                    return VadDecision::SpeechActive;
                }
            } else {
                self.onset_count = 0;
                let alpha = self.config.noise_ema_alpha.clamp(0.001, 0.5);
                self.noise_floor = ((1.0 - alpha) * self.noise_floor + alpha * rms)
                    .clamp(self.config.min_noise_floor, self.config.max_noise_floor);
            }
            return VadDecision::Silence;
        }

        self.speech_buf.extend_from_slice(frame);
        if rms >= continue_threshold {
            self.silence_count = 0;
        } else {
            self.silence_count += 1;
        }

        if self.silence_count >= self.config.hangover_silence_frames
            || self.speech_buf.len() >= self.config.max_utterance_samples
        {
            let utt = std::mem::take(&mut self.speech_buf);
            self.in_speech = false;
            self.onset_count = 0;
            self.silence_count = 0;
            self.pre_roll.clear();
            if utt.len() >= self.config.min_utterance_samples {
                return VadDecision::UtteranceReady(utt);
            }
            return VadDecision::Silence;
        }

        VadDecision::SpeechActive
    }

    /// Push arbitrary-length 16 kHz mono samples, chunking into `frame_samples` frames and
    /// returning any finalized utterances.
    pub fn push_samples(
        &mut self,
        samples: &[f32],
        mode: VoiceMode,
        is_speaking: bool,
    ) -> Vec<Vec<f32>> {
        if is_speaking || mode == VoiceMode::Muted {
            self.reset_utterance();
            return Vec::new();
        }
        self.frame_accum.extend_from_slice(samples);
        let frame_len = self.config.frame_samples.max(1);
        let mut ready = Vec::new();
        while self.frame_accum.len() >= frame_len {
            let frame: Vec<f32> = self.frame_accum.drain(..frame_len).collect();
            if let VadDecision::UtteranceReady(utt) =
                self.process_frame(&frame, mode, is_speaking)
            {
                ready.push(utt);
            }
        }
        ready
    }

    /// Flush buffered audio at the end of a `PushToTalk` capture or manual stop.
    pub fn flush_utterance(&mut self) -> Option<Vec<f32>> {
        if !self.frame_accum.is_empty() && self.in_speech {
            self.speech_buf.extend_from_slice(&self.frame_accum);
        }
        self.frame_accum.clear();
        self.in_speech = false;
        self.onset_count = 0;
        self.silence_count = 0;
        self.pre_roll.clear();

        let utt = std::mem::take(&mut self.speech_buf);
        if utt.len() >= self.config.min_utterance_samples {
            Some(utt)
        } else {
            None
        }
    }
}

/// Encode `VoiceMode` into `u8` for lock-free atomic reads on audio threads.
pub fn encode_voice_mode(mode: VoiceMode) -> u8 {
    match mode {
        VoiceMode::PushToTalk => 0,
        VoiceMode::HandsFree => 1,
        VoiceMode::Muted => 2,
    }
}

/// Decode `VoiceMode` from `u8`.
pub fn decode_voice_mode(raw: u8) -> VoiceMode {
    match raw {
        1 => VoiceMode::HandsFree,
        2 => VoiceMode::Muted,
        _ => VoiceMode::PushToTalk,
    }
}

/// Active `cpal` capture handle + worker thread.
pub struct MicCaptureHandle {
    stop_flag: Arc<AtomicBool>,
    stream_thread: Mutex<Option<JoinHandle<()>>>,
    worker_thread: Mutex<Option<JoinHandle<()>>>,
}

impl MicCaptureHandle {
    /// Start capturing microphone input using `cpal`, downmixing to mono `f32`, resampling to
    /// 16 kHz via `rubato`, gating echo with `is_speaking`, and passing finalized utterances from
    /// `AdaptiveVad` to `on_utterance`.
    ///
    /// Returns a clean `SoulError::Voice` if no input device is available or permission is denied,
    /// without panicking.
    pub fn start<F>(
        mode_atomic: Arc<AtomicU8>,
        is_speaking: Arc<AtomicBool>,
        mut on_utterance: F,
    ) -> SoulResult<Self>
    where
        F: FnMut(Vec<f32>) + Send + 'static,
    {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or_else(|| {
            SoulError::Voice("No default microphone input device available".into())
        })?;

        let supported_config = device.default_input_config().map_err(|e| {
            SoulError::Voice(format!("Unable to query default mic input config: {e}"))
        })?;
        let sample_format = supported_config.sample_format();
        let stream_config: cpal::StreamConfig = supported_config.into();
        let input_rate = stream_config.sample_rate;
        let channels = usize::from(stream_config.channels.max(1));

        // Pre-validate resampler before spinning up threads.
        let mut resampler = MonoResampler::new(input_rate)?;

        let (audio_tx, audio_rx) = mpsc::sync_channel::<Vec<f32>>(64);
        let (init_tx, init_rx) = mpsc::channel::<SoulResult<()>>();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_stream = Arc::clone(&stop_flag);
        let stop_worker = Arc::clone(&stop_flag);

        let stream_thread = std::thread::Builder::new()
            .name("voice-io-cpal-in".into())
            .spawn(move || {
                let stream_res =
                    build_input_stream(&device, &stream_config, sample_format, channels, audio_tx);
                let stream = match stream_res {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };
                if let Err(e) = stream.play() {
                    let _ = init_tx.send(Err(SoulError::Voice(format!(
                        "Failed to start mic input stream: {e}"
                    ))));
                    return;
                }
                let _ = init_tx.send(Ok(()));

                while !stop_stream.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(25));
                }
                drop(stream);
            })
            .map_err(|e| SoulError::Voice(format!("Failed to spawn mic capture thread: {e}")))?;

        match init_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                stop_flag.store(true, Ordering::Release);
                let _ = stream_thread.join();
                return Err(e);
            }
            Err(_) => {
                stop_flag.store(true, Ordering::Release);
                let _ = stream_thread.join();
                return Err(SoulError::Voice(
                    "Microphone capture thread terminated during initialization".into(),
                ));
            }
        }

        let worker_thread = std::thread::Builder::new()
            .name("voice-io-vad-worker".into())
            .spawn(move || {
                let mut vad = AdaptiveVad::default();
                loop {
                    match audio_rx.recv_timeout(Duration::from_millis(30)) {
                        Ok(mono_native) => {
                            let speaking = is_speaking.load(Ordering::Acquire);
                            let mode = decode_voice_mode(mode_atomic.load(Ordering::Acquire));

                            // Still keep resampler fed or reset on mute so phase stays clean.
                            match resampler.push(&mono_native) {
                                Ok(pcm_16k) if !pcm_16k.is_empty() => {
                                    for utt in vad.push_samples(&pcm_16k, mode, speaking) {
                                        on_utterance(utt);
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => warn!("Resampler error: {e}"),
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if stop_worker.load(Ordering::Acquire) {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }

                // Drain remaining resampled samples and flush PushToTalk buffer on stop.
                let mode = decode_voice_mode(mode_atomic.load(Ordering::Acquire));
                let speaking = is_speaking.load(Ordering::Acquire);
                if !speaking && mode != VoiceMode::Muted {
                    if let Ok(tail) = resampler.flush() {
                        if !tail.is_empty() {
                            for utt in vad.push_samples(&tail, mode, false) {
                                on_utterance(utt);
                            }
                        }
                    }
                    if mode == VoiceMode::PushToTalk {
                        if let Some(utt) = vad.flush_utterance() {
                            on_utterance(utt);
                        }
                    }
                }
                debug!("Mic capture VAD worker stopped cleanly");
            })
            .map_err(|e| SoulError::Voice(format!("Failed to spawn VAD worker thread: {e}")))?;

        Ok(Self {
            stop_flag,
            stream_thread: Mutex::new(Some(stream_thread)),
            worker_thread: Mutex::new(Some(worker_thread)),
        })
    }

    /// Stop microphone capture cleanly and wait for any pending PushToTalk flush to complete.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Ok(mut guard) = self.stream_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
        if let Ok(mut guard) = self.worker_thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for MicCaptureHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    channels: usize,
    tx: mpsc::SyncSender<Vec<f32>>,
) -> SoulResult<cpal::Stream> {
    match format {
        cpal::SampleFormat::F32 => build_typed_input_stream::<f32>(device, config, channels, tx),
        cpal::SampleFormat::I16 => build_typed_input_stream::<i16>(device, config, channels, tx),
        cpal::SampleFormat::U16 => build_typed_input_stream::<u16>(device, config, channels, tx),
        cpal::SampleFormat::I32 => build_typed_input_stream::<i32>(device, config, channels, tx),
        cpal::SampleFormat::F64 => build_typed_input_stream::<f64>(device, config, channels, tx),
        other => Err(SoulError::Voice(format!(
            "Unsupported microphone sample format: {other}"
        ))),
    }
}

fn build_typed_input_stream<T: cpal::SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    tx: mpsc::SyncSender<Vec<f32>>,
) -> SoulResult<cpal::Stream>
where
    f32: cpal::FromSample<T>,
{
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
                let mono = cpal_to_mono_f32(data, channels);
                if !mono.is_empty() {
                    let _ = tx.try_send(mono);
                }
            },
            |err| {
                warn!("Microphone input stream error: {err}");
            },
            None,
        )
        .map_err(|e| SoulError::Voice(format!("Failed to build microphone input stream: {e}")))
}
