use std::sync::RwLock;

use shared_types::{
    AvatarLogicalState, AvatarSnapshot, CharacterId, EmotionProfileState, EmotionState, EmotionTag,
    VisemeFrame, events, now_timestamp_ms,
};
use tauri::{AppHandle, Emitter};

/// Joy emotion intensity threshold at or above which the avatar closes its eyes in a happy smile.
pub const JOY_EYES_CLOSED_THRESHOLD: f32 = 0.65;

/// Centralized Single Source of Truth ("Karada") for synchronized Live2D avatar state.
///
/// Enforces the hard ocular invariant on every state transition:
/// `eyes_closed = (logical_state == Sleep) || (emotion.tag == Joy && emotion.intensity >= 0.65)`,
/// `auto_blink_enabled = !eyes_closed`, and `gaze_tracking_enabled = !eyes_closed`.
#[derive(Debug)]
pub struct AvatarStateServer {
    snapshot: RwLock<AvatarSnapshot>,
}

impl AvatarStateServer {
    /// Create a new `AvatarStateServer` initialized from `character_id` and its decayed emotion profile.
    pub fn new(character_id: CharacterId, emotion_profile: EmotionProfileState) -> Self {
        let now = now_timestamp_ms();
        let initial_emotion = EmotionState {
            tag: emotion_profile.dominant,
            intensity: emotion_profile.dominant_intensity.clamp(0.0, 1.0),
            reason: "Restored character emotion baseline".to_string(),
            timestamp_ms: now,
        };
        let mut snap = AvatarSnapshot {
            character_id,
            logical_state: AvatarLogicalState::Idle,
            emotion: initial_emotion,
            emotion_profile,
            eyes_closed: false,
            auto_blink_enabled: true,
            gaze_tracking_enabled: true,
            viseme: VisemeFrame::default(),
            updated_at_ms: now,
        };
        Self::enforce_invariants(&mut snap);
        Self {
            snapshot: RwLock::new(snap),
        }
    }

    /// Enforce hard eyes-closed / blink-pause / gaze-pause invariants on `snapshot`.
    pub fn enforce_invariants(snapshot: &mut AvatarSnapshot) {
        let eyes_closed = (snapshot.logical_state == AvatarLogicalState::Sleep)
            || (snapshot.emotion.tag == EmotionTag::Joy
                && snapshot.emotion.intensity >= JOY_EYES_CLOSED_THRESHOLD);
        snapshot.eyes_closed = eyes_closed;
        snapshot.auto_blink_enabled = !eyes_closed;
        snapshot.gaze_tracking_enabled = !eyes_closed;
    }

    /// Return a cloned copy of the current [`AvatarSnapshot`].
    pub fn snapshot(&self) -> AvatarSnapshot {
        self.snapshot
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Transition the avatar logical state (e.g. `Idle`, `Think`, `Write`, `Speak`, `Sleep`)
    /// with optional emotion and decayed profile updates, returning the synchronized snapshot.
    pub fn set_logical_state(
        &self,
        logical_state: AvatarLogicalState,
        emotion: Option<EmotionState>,
        emotion_profile: Option<EmotionProfileState>,
    ) -> AvatarSnapshot {
        let now = now_timestamp_ms();
        let mut guard = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.logical_state = logical_state;
        if let Some(emo) = emotion {
            guard.emotion = emo;
        }
        if let Some(profile) = emotion_profile {
            guard.emotion_profile = profile;
        }
        if logical_state == AvatarLogicalState::Sleep {
            guard.viseme = VisemeFrame {
                mouth_open_y: 0.0,
                mouth_form: 0.0,
                energy: 0.0,
                timestamp_ms: now,
            };
        }
        guard.updated_at_ms = now;
        Self::enforce_invariants(&mut guard);
        guard.clone()
    }

    /// Update the instantaneous emotion state (and optional decayed profile) while enforcing invariants.
    pub fn set_emotion(
        &self,
        emotion: EmotionState,
        emotion_profile: Option<EmotionProfileState>,
    ) -> AvatarSnapshot {
        let now = now_timestamp_ms();
        let mut guard = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.emotion = emotion;
        if let Some(profile) = emotion_profile {
            guard.emotion_profile = profile;
        }
        guard.updated_at_ms = now;
        Self::enforce_invariants(&mut guard);
        guard.clone()
    }

    /// Refresh only the decayed emotion profile (if matching the active character) and enforce invariants.
    pub fn update_emotion_profile(&self, emotion_profile: EmotionProfileState) -> AvatarSnapshot {
        let now = now_timestamp_ms();
        let mut guard = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.character_id == emotion_profile.character_id {
            guard.emotion_profile = emotion_profile;
            guard.updated_at_ms = now;
            Self::enforce_invariants(&mut guard);
        }
        guard.clone()
    }

    /// Update the latest lip-sync viseme frame in the snapshot.
    pub fn set_viseme(&self, viseme: VisemeFrame) -> AvatarSnapshot {
        let mut guard = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.viseme = viseme;
        guard.updated_at_ms = now_timestamp_ms();
        guard.clone()
    }

    /// Switch active companion character and load their isolated emotion profile, resetting to `Idle`.
    pub fn switch_character(
        &self,
        character_id: CharacterId,
        emotion_profile: EmotionProfileState,
    ) -> AvatarSnapshot {
        let now = now_timestamp_ms();
        let mut guard = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.character_id = character_id;
        guard.logical_state = AvatarLogicalState::Idle;
        guard.emotion = EmotionState {
            tag: emotion_profile.dominant,
            intensity: emotion_profile.dominant_intensity.clamp(0.0, 1.0),
            reason: format!("Switched active character to {character_id}"),
            timestamp_ms: now,
        };
        guard.emotion_profile = emotion_profile;
        guard.viseme = VisemeFrame {
            mouth_open_y: 0.0,
            mouth_form: 0.0,
            energy: 0.0,
            timestamp_ms: now,
        };
        guard.updated_at_ms = now;
        Self::enforce_invariants(&mut guard);
        guard.clone()
    }

    /// Emit `avatar://state` (`events::AVATAR_STATE`) to the Tauri webview.
    pub fn emit_snapshot(app: &AppHandle, snapshot: &AvatarSnapshot) {
        let _ = app.emit(events::AVATAR_STATE, snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline_profile(character_id: CharacterId) -> EmotionProfileState {
        EmotionProfileState {
            character_id,
            dominant: EmotionTag::Neutral,
            dominant_intensity: 0.35,
            emotions: Vec::new(),
            computed_at_ms: 1_000,
        }
    }

    #[test]
    fn avatar_state_enforces_eyes_closed_on_sleep_and_high_joy() {
        let server = AvatarStateServer::new(CharacterId::Coral, baseline_profile(CharacterId::Coral));
        let initial = server.snapshot();
        assert_eq!(initial.logical_state, AvatarLogicalState::Idle);
        assert!(!initial.eyes_closed);
        assert!(initial.auto_blink_enabled);
        assert!(initial.gaze_tracking_enabled);

        // Sleep forces eyes_closed = true and disables auto-blink + gaze tracking.
        let sleeping = server.set_logical_state(AvatarLogicalState::Sleep, None, None);
        assert_eq!(sleeping.logical_state, AvatarLogicalState::Sleep);
        assert!(sleeping.eyes_closed);
        assert!(!sleeping.auto_blink_enabled);
        assert!(!sleeping.gaze_tracking_enabled);

        // Waking up to Idle restores auto-blink and gaze tracking when emotion is neutral.
        let awake = server.set_logical_state(AvatarLogicalState::Idle, None, None);
        assert!(!awake.eyes_closed);
        assert!(awake.auto_blink_enabled);
        assert!(awake.gaze_tracking_enabled);

        // High-intensity Joy (>= 0.65) closes eyes in a happy smile and pauses blink + gaze.
        let happy_smile = server.set_emotion(
            EmotionState {
                tag: EmotionTag::Joy,
                intensity: 0.65,
                reason: "Smiling warmly".into(),
                timestamp_ms: 2_000,
            },
            None,
        );
        assert!(happy_smile.eyes_closed);
        assert!(!happy_smile.auto_blink_enabled);
        assert!(!happy_smile.gaze_tracking_enabled);

        // Mild Joy (< 0.65) keeps eyes open with blink + gaze active.
        let mild_joy = server.set_emotion(
            EmotionState {
                tag: EmotionTag::Joy,
                intensity: 0.64,
                reason: "Gentle smile".into(),
                timestamp_ms: 3_000,
            },
            None,
        );
        assert!(!mild_joy.eyes_closed);
        assert!(mild_joy.auto_blink_enabled);
        assert!(mild_joy.gaze_tracking_enabled);
    }
}
