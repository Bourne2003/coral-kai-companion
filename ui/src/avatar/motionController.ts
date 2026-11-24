import type {
  AvatarLogicalState,
  AvatarSnapshot,
  EmotionState,
  EmotionTag,
  VisemeFrame,
} from "../types/ipc";
import { createDefaultCubismParams, type CubismParams } from "./types";

interface SpringState {
  value: number;
  velocity: number;
}

interface EmotionExpressionPose {
  eyeOpenBase: number;
  eyeSmile: number;
  browLY: number;
  browRY: number;
  browLAngle: number;
  browRAngle: number;
  mouthForm: number;
  mouthRestOpen: number;
  cheek: number;
  headTiltZ: number;
  headPitchY: number;
  headYawX: number;
  gazeBiasX: number;
  gazeBiasY: number;
  tearSheen: number;
  ahogeLift: number;
}

const EMOTION_POSES: Record<EmotionTag, EmotionExpressionPose> = {
  neutral: {
    eyeOpenBase: 1.0,
    eyeSmile: 0.08,
    browLY: 0.04,
    browRY: 0.04,
    browLAngle: 0.04,
    browRAngle: 0.04,
    mouthForm: 0.18,
    mouthRestOpen: 0.0,
    cheek: 0.14,
    headTiltZ: 0,
    headPitchY: 0,
    headYawX: 0,
    gazeBiasX: 0,
    gazeBiasY: 0,
    tearSheen: 0,
    ahogeLift: 0,
  },
  joy: {
    eyeOpenBase: 0.96,
    eyeSmile: 0.92,
    browLY: 0.36,
    browRY: 0.36,
    browLAngle: 0.18,
    browRAngle: 0.18,
    mouthForm: 0.85,
    mouthRestOpen: 0.12,
    cheek: 0.48,
    headTiltZ: 5.5,
    headPitchY: 3.5,
    headYawX: 2.0,
    gazeBiasX: 0,
    gazeBiasY: 0.04,
    tearSheen: 0,
    ahogeLift: 0.55,
  },
  sadness: {
    eyeOpenBase: 0.78,
    eyeSmile: 0,
    browLY: -0.22,
    browRY: -0.22,
    browLAngle: -0.72,
    browRAngle: -0.72,
    mouthForm: -0.62,
    mouthRestOpen: 0.02,
    cheek: 0.16,
    headTiltZ: -6.5,
    headPitchY: -7.5,
    headYawX: -3.0,
    gazeBiasX: -0.1,
    gazeBiasY: -0.26,
    tearSheen: 0.88,
    ahogeLift: -0.7,
  },
  shy: {
    eyeOpenBase: 0.9,
    eyeSmile: 0.32,
    browLY: 0.14,
    browRY: 0.14,
    browLAngle: -0.28,
    browRAngle: -0.28,
    mouthForm: 0.44,
    mouthRestOpen: 0.02,
    cheek: 0.96,
    headTiltZ: -8.0,
    headPitchY: -4.0,
    headYawX: -8.5,
    gazeBiasX: -0.45,
    gazeBiasY: -0.22,
    tearSheen: 0,
    ahogeLift: 0.25,
  },
  surprised: {
    eyeOpenBase: 1.24,
    eyeSmile: 0,
    browLY: 0.82,
    browRY: 0.82,
    browLAngle: 0.35,
    browRAngle: 0.35,
    mouthForm: -0.45,
    mouthRestOpen: 0.42,
    cheek: 0.28,
    headTiltZ: 2.0,
    headPitchY: 5.5,
    headYawX: 0,
    gazeBiasX: 0,
    gazeBiasY: 0.08,
    tearSheen: 0,
    ahogeLift: 1.0,
  },
  thinking: {
    eyeOpenBase: 0.92,
    eyeSmile: 0.05,
    browLY: 0.38,
    browRY: -0.18,
    browLAngle: 0.22,
    browRAngle: -0.35,
    mouthForm: -0.18,
    mouthRestOpen: 0.0,
    cheek: 0.12,
    headTiltZ: 9.5,
    headPitchY: 3.0,
    headYawX: 7.5,
    gazeBiasX: 0.48,
    gazeBiasY: 0.32,
    tearSheen: 0,
    ahogeLift: 0.35,
  },
  empathetic: {
    eyeOpenBase: 0.89,
    eyeSmile: 0.38,
    browLY: 0.18,
    browRY: 0.18,
    browLAngle: -0.32,
    browRAngle: -0.32,
    mouthForm: 0.54,
    mouthRestOpen: 0.03,
    cheek: 0.62,
    headTiltZ: 4.5,
    headPitchY: -2.5,
    headYawX: 2.5,
    gazeBiasX: 0,
    gazeBiasY: -0.04,
    tearSheen: 0.2,
    ahogeLift: 0.1,
  },
};

function clamp(v: number, min: number, max: number): number {
  return v < min ? min : v > max ? max : v;
}

function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

/**
 * Evaluates cubic-bezier(p1x, p1y, p2x, p2y) with Newton-Raphson refinement
 * for smooth non-linear emotional morphing.
 */
function cubicBezierEase(t: number, p1x = 0.22, p1y = 1, p2x = 0.36, p2y = 1): number {
  const x = clamp(t, 0, 1);
  if (x <= 0 || x >= 1) return x;

  const sampleX = (u: number) =>
    ((1 - 3 * p2x + 3 * p1x) * u * u * u) +
    ((3 * p2x - 6 * p1x) * u * u) +
    (3 * p1x * u);
  const sampleDX = (u: number) =>
    3 * (1 - 3 * p2x + 3 * p1x) * u * u +
    2 * (3 * p2x - 6 * p1x) * u +
    3 * p1x;
  const sampleY = (u: number) =>
    ((1 - 3 * p2y + 3 * p1y) * u * u * u) +
    ((3 * p2y - 6 * p1y) * u * u) +
    (3 * p1y * u);

  let u = x;
  for (let i = 0; i < 4; i += 1) {
    const err = sampleX(u) - x;
    const dx = sampleDX(u);
    if (Math.abs(err) < 1e-4 || Math.abs(dx) < 1e-5) break;
    u = clamp(u - err / dx, 0, 1);
  }
  return sampleY(u);
}

function stepSpring(
  spring: SpringState,
  target: number,
  stiffness: number,
  damping: number,
  dtSec: number
): number {
  const force = (target - spring.value) * stiffness;
  const accel = force - spring.velocity * damping;
  spring.velocity += accel * dtSec;
  spring.value += spring.velocity * dtSec;
  return spring.value;
}

export class AvatarMotionController {
  readonly params: CubismParams = createDefaultCubismParams();

  private currentEmotion: EmotionTag = "neutral";
  private currentIntensity = 0.55;
  private logicalState: AvatarLogicalState = "idle";
  private snapshotEyesClosed = false;
  private autoBlinkEnabled = true;
  private gazeTrackingEnabled = true;
  private fromPose: EmotionExpressionPose = { ...EMOTION_POSES.neutral };
  private toPose: EmotionExpressionPose = { ...EMOTION_POSES.neutral };
  private blendedPose: EmotionExpressionPose = { ...EMOTION_POSES.neutral };
  private morphProgress = 1;
  private readonly morphDurationSec = 0.34;

  // Blink timeline (2.5 - 6.0s natural interval + occasional double blink)
  private nextBlinkAtMs = 2800;
  private blinkElapsedSec = -1;
  private doubleBlinkPending = false;
  private readonly blinkDurationSec = 0.175;

  // Gaze & saccades
  private pointerNormX = 0;
  private pointerNormY = 0;
  private pointerActiveUntilMs = 0;
  private saccadeX = 0;
  private saccadeY = 0;
  private nextSaccadeAtMs = 1500;
  private smoothGazeX = 0;
  private smoothGazeY = 0;

  // Viseme lip-sync buffer
  private targetMouthOpenY = 0;
  private targetMouthForm = 0.16;
  private targetSpeechEnergy = 0;
  private lastVisemeAtMs = 0;
  private smoothMouthOpenY = 0;
  private smoothMouthForm = 0.16;
  private smoothSpeechEnergy = 0;

  // Damped springs
  private springHeadX: SpringState = { value: 0, velocity: 0 };
  private springHeadY: SpringState = { value: 0, velocity: 0 };
  private springHeadZ: SpringState = { value: 0, velocity: 0 };
  private springHairFront: SpringState = { value: 0, velocity: 0 };
  private springHairSide: SpringState = { value: 0, velocity: 0 };
  private springHairBack: SpringState = { value: 0, velocity: 0 };
  private springAccessory: SpringState = { value: 0, velocity: 0 };
  private springTorsoX: SpringState = { value: 0, velocity: 0 };
  private springAhoge: SpringState = { value: 0, velocity: 0 };

  applyAvatarSnapshot(snapshot: AvatarSnapshot): void {
    this.logicalState = snapshot.logicalState ?? "idle";
    this.snapshotEyesClosed = Boolean(snapshot.eyesClosed);
    this.autoBlinkEnabled = Boolean(snapshot.autoBlinkEnabled);
    this.gazeTrackingEnabled = Boolean(snapshot.gazeTrackingEnabled);
    this.setEmotion(snapshot.emotion);

    if (this.logicalState === "sleep") {
      this.clearViseme();
    } else if (
      snapshot.viseme &&
      (snapshot.viseme.mouthOpenY > 0.02 || snapshot.viseme.energy > 0.02)
    ) {
      this.pushViseme(snapshot.viseme, performance.now());
    }
  }

  getLogicalState(): AvatarLogicalState {
    return this.logicalState;
  }

  isOcularMotionPaused(): boolean {
    return (
      this.snapshotEyesClosed ||
      !this.autoBlinkEnabled ||
      this.logicalState === "sleep" ||
      this.blendedPose.eyeSmile > 0.65
    );
  }

  setEmotion(state: EmotionState): void {
    const tag = state.tag in EMOTION_POSES ? state.tag : "neutral";
    const intensity = clamp(state.intensity, 0.1, 1);

    this.fromPose = { ...this.blendedPose };
    const base = EMOTION_POSES.neutral;
    const raw = EMOTION_POSES[tag];

    this.toPose = {
      eyeOpenBase: lerp(base.eyeOpenBase, raw.eyeOpenBase, intensity),
      eyeSmile: lerp(base.eyeSmile, raw.eyeSmile, intensity),
      browLY: lerp(base.browLY, raw.browLY, intensity),
      browRY: lerp(base.browRY, raw.browRY, intensity),
      browLAngle: lerp(base.browLAngle, raw.browLAngle, intensity),
      browRAngle: lerp(base.browRAngle, raw.browRAngle, intensity),
      mouthForm: lerp(base.mouthForm, raw.mouthForm, intensity),
      mouthRestOpen: lerp(base.mouthRestOpen, raw.mouthRestOpen, intensity),
      cheek: lerp(base.cheek, raw.cheek, intensity),
      headTiltZ: lerp(base.headTiltZ, raw.headTiltZ, intensity),
      headPitchY: lerp(base.headPitchY, raw.headPitchY, intensity),
      headYawX: lerp(base.headYawX, raw.headYawX, intensity),
      gazeBiasX: lerp(base.gazeBiasX, raw.gazeBiasX, intensity),
      gazeBiasY: lerp(base.gazeBiasY, raw.gazeBiasY, intensity),
      tearSheen: lerp(base.tearSheen, raw.tearSheen, intensity),
      ahogeLift: lerp(base.ahogeLift, raw.ahogeLift, intensity),
    };

    if (tag !== this.currentEmotion || Math.abs(intensity - this.currentIntensity) > 0.15) {
      this.morphProgress = 0;
      if (tag === "surprised") {
        this.springHeadY.velocity += 55;
        this.springAhoge.velocity += 10;
      } else if (tag === "joy") {
        this.springHeadZ.velocity += 22;
        this.springAhoge.velocity += 6;
      }
    }

    this.currentEmotion = tag;
    this.currentIntensity = intensity;
  }

  getEmotion(): { tag: EmotionTag; intensity: number } {
    return { tag: this.currentEmotion, intensity: this.currentIntensity };
  }

  setPointerLook(normX: number, normY: number, nowMs: number): void {
    this.pointerNormX = clamp(normX, -1, 1);
    this.pointerNormY = clamp(normY, -1, 1);
    if (!this.isOcularMotionPaused() && this.gazeTrackingEnabled) {
      this.pointerActiveUntilMs = nowMs + 4200;
    }
  }

  pushViseme(frame: VisemeFrame, nowMs: number): void {
    this.targetMouthOpenY = clamp(frame.mouthOpenY, 0, 1);
    this.targetMouthForm = clamp(frame.mouthForm, -1, 1);
    this.targetSpeechEnergy = clamp(frame.energy, 0, 1);
    this.lastVisemeAtMs = nowMs;
  }

  clearViseme(): void {
    this.targetMouthOpenY = 0;
    this.targetSpeechEnergy = 0;
    this.lastVisemeAtMs = 0;
  }

  update(dtMs: number, nowMs: number): CubismParams {
    const dtSec = clamp(dtMs / 1000, 0.001, 0.05);
    const tSec = nowMs / 1000;

    // 1. Cubic-bezier emotion morphing
    if (this.morphProgress < 1) {
      this.morphProgress = Math.min(1, this.morphProgress + dtSec / this.morphDurationSec);
    }
    const e = cubicBezierEase(this.morphProgress);
    const fp = this.fromPose;
    const tp = this.toPose;
    const bp = this.blendedPose;

    bp.eyeOpenBase = lerp(fp.eyeOpenBase, tp.eyeOpenBase, e);
    bp.eyeSmile = lerp(fp.eyeSmile, tp.eyeSmile, e);
    bp.browLY = lerp(fp.browLY, tp.browLY, e);
    bp.browRY = lerp(fp.browRY, tp.browRY, e);
    bp.browLAngle = lerp(fp.browLAngle, tp.browLAngle, e);
    bp.browRAngle = lerp(fp.browRAngle, tp.browRAngle, e);
    bp.mouthForm = lerp(fp.mouthForm, tp.mouthForm, e);
    bp.mouthRestOpen = lerp(fp.mouthRestOpen, tp.mouthRestOpen, e);
    bp.cheek = lerp(fp.cheek, tp.cheek, e);
    bp.headTiltZ = lerp(fp.headTiltZ, tp.headTiltZ, e);
    bp.headPitchY = lerp(fp.headPitchY, tp.headPitchY, e);
    bp.headYawX = lerp(fp.headYawX, tp.headYawX, e);
    bp.gazeBiasX = lerp(fp.gazeBiasX, tp.gazeBiasX, e);
    bp.gazeBiasY = lerp(fp.gazeBiasY, tp.gazeBiasY, e);
    bp.tearSheen = lerp(fp.tearSheen, tp.tearSheen, e);
    bp.ahogeLift = lerp(fp.ahogeLift, tp.ahogeLift, e);

    // Logical state pose offsets (think / write / speak / sleep)
    const isSleeping = this.logicalState === "sleep";
    const isThinking = this.logicalState === "think";
    const isWriting = this.logicalState === "write";
    const isSpeakingState = this.logicalState === "speak";

    let stateHeadYaw = 0;
    let stateHeadPitch = 0;
    let stateHeadTilt = 0;
    let stateGazeBiasX = 0;
    let stateGazeBiasY = 0;
    let stateBrowLY = 0;
    let stateBrowRY = 0;
    let stateAhogeLift = 0;

    if (isThinking) {
      stateHeadTilt = 6.2;
      stateHeadPitch = 2.6;
      stateHeadYaw = 4.4;
      stateGazeBiasX = 0.34;
      stateGazeBiasY = 0.28;
      stateBrowLY = 0.22;
      stateBrowRY = -0.1;
      stateAhogeLift = 0.28;
    } else if (isWriting) {
      const penNod = Math.sin(tSec * 4.4) * 1.45 + Math.sin(tSec * 8.8) * 0.45;
      stateHeadPitch = -6.2 + penNod;
      stateHeadTilt = -3.4 + Math.sin(tSec * 2.2) * 0.75;
      stateHeadYaw = 3.2;
      stateGazeBiasX = 0.12;
      stateGazeBiasY = -0.34;
      stateBrowLY = -0.08;
      stateBrowRY = -0.08;
    } else if (isSleeping) {
      stateHeadPitch = -4.6;
      stateHeadTilt = -5.2;
    }

    // HARD INVARIANT: Pause blinking AND pause gaze tracking + saccades whenever eyes are closed
    // (`eyesClosed === true`, `!autoBlinkEnabled`, `logicalState === "sleep"`, or `eyeSmile > 0.65`).
    const eyesForcedClosed = this.snapshotEyesClosed || isSleeping;
    const suppressBlinkAndGaze =
      eyesForcedClosed || !this.autoBlinkEnabled || bp.eyeSmile > 0.65;
    const pauseGaze = suppressBlinkAndGaze || !this.gazeTrackingEnabled;

    // 2. Natural blinking (paused immediately when suppressBlinkAndGaze is true)
    let blinkFactor = 1;
    if (suppressBlinkAndGaze) {
      // Hold countdown in place without advancing/triggering blink timers
      this.nextBlinkAtMs += dtMs;
      this.blinkElapsedSec = -1;
      this.doubleBlinkPending = false;
      blinkFactor = eyesForcedClosed ? 0 : 1;
    } else {
      if (this.blinkElapsedSec < 0 && nowMs >= this.nextBlinkAtMs) {
        this.blinkElapsedSec = 0;
        this.doubleBlinkPending = Math.random() < 0.2;
      }

      if (this.blinkElapsedSec >= 0) {
        this.blinkElapsedSec += dtSec;
        const p = this.blinkElapsedSec / this.blinkDurationSec;
        if (p >= 1) {
          if (this.doubleBlinkPending) {
            this.doubleBlinkPending = false;
            this.blinkElapsedSec = -0.075; // brief 75ms pause before second blink
            this.nextBlinkAtMs = nowMs + 75;
          } else {
            this.blinkElapsedSec = -1;
            this.nextBlinkAtMs = nowMs + 2500 + Math.random() * 3500;
          }
        } else {
          // Asymmetric close (fast 35%) -> open (smooth 65%)
          blinkFactor = p < 0.35 ? 1 - p / 0.35 : (p - 0.35) / 0.65;
          blinkFactor = clamp(blinkFactor, 0, 1);
        }
      }
    }

    // 3. Eye saccades + pointer gaze tracking (held cleanly at neutral when paused)
    if (pauseGaze) {
      this.nextSaccadeAtMs += dtMs;
      this.saccadeX = 0;
      this.saccadeY = 0;
      this.smoothGazeX = 0;
      this.smoothGazeY = 0;
    } else {
      if (nowMs >= this.nextSaccadeAtMs) {
        this.saccadeX = (Math.random() - 0.5) * 0.24;
        this.saccadeY = (Math.random() - 0.5) * 0.16;
        this.nextSaccadeAtMs = nowMs + 1400 + Math.random() * 2200;
      }

      const pointerWeight = nowMs < this.pointerActiveUntilMs ? 0.78 : 0.12;
      const desiredGazeX = clamp(
        this.pointerNormX * pointerWeight +
          this.saccadeX * (1 - pointerWeight * 0.6) +
          bp.gazeBiasX +
          stateGazeBiasX,
        -1,
        1
      );
      const desiredGazeY = clamp(
        this.pointerNormY * pointerWeight +
          this.saccadeY * (1 - pointerWeight * 0.6) +
          bp.gazeBiasY +
          stateGazeBiasY,
        -1,
        1
      );

      const gazeFollowRate = 1 - Math.exp(-dtSec * 14);
      this.smoothGazeX = lerp(this.smoothGazeX, desiredGazeX, gazeFollowRate);
      this.smoothGazeY = lerp(this.smoothGazeY, desiredGazeY, gazeFollowRate);
    }

    // 4. Sinusoidal breathing & idle sway (slower, deeper breathing in sleep)
    const breathPeriod = isSleeping ? 5.8 : 3.9;
    const breathPhase = Math.sin(tSec * ((Math.PI * 2) / breathPeriod));
    const breathNorm = 0.5 + 0.5 * breathPhase;
    const swayScale = isSleeping ? 0.25 : 1.0;
    const idleSwayX = Math.sin(tSec * 0.85) * 1.4 * swayScale;
    const idleSwayZ = Math.cos(tSec * 0.62) * 1.1 * swayScale;

    // 5. Lip-sync visemes with smooth attack/release
    const isSpeakingViseme = this.lastVisemeAtMs > 0 && nowMs - this.lastVisemeAtMs < 240;
    const targetOpen = isSleeping
      ? 0
      : isSpeakingViseme
      ? this.targetMouthOpenY
      : bp.mouthRestOpen;
    const targetForm = isSleeping
      ? 0.08
      : isSpeakingViseme
      ? lerp(bp.mouthForm, this.targetMouthForm, 0.75)
      : bp.mouthForm;
    const targetEnergy = isSleeping
      ? 0
      : isSpeakingViseme
      ? this.targetSpeechEnergy
      : isSpeakingState
      ? 0.24
      : 0;

    const mouthRate = 1 - Math.exp(-dtSec * (isSpeakingViseme ? 26 : 14));
    this.smoothMouthOpenY = lerp(this.smoothMouthOpenY, targetOpen, mouthRate);
    this.smoothMouthForm = lerp(this.smoothMouthForm, targetForm, mouthRate);
    this.smoothSpeechEnergy = lerp(this.smoothSpeechEnergy, targetEnergy, mouthRate);

    // 6. Head tracking + damped spring physics for hair/ribbon/torso
    const speechNod =
      this.smoothSpeechEnergy * Math.sin(tSec * 11.5) * 1.8 +
      this.smoothSpeechEnergy * 1.2;
    const targetHeadX = clamp(
      (isSleeping ? 0 : bp.headYawX) + stateHeadYaw + this.smoothGazeX * 7.5 + idleSwayX,
      -26,
      26
    );
    const targetHeadY = clamp(
      (isSleeping ? 0 : bp.headPitchY) +
        stateHeadPitch +
        this.smoothGazeY * 6.0 +
        breathPhase * (isSleeping ? 1.65 : 1.1) -
        speechNod * 0.35,
      -24,
      24
    );
    const targetHeadZ = clamp(
      (isSleeping ? 0 : bp.headTiltZ) + stateHeadTilt + this.smoothGazeX * 2.4 + idleSwayZ,
      -22,
      22
    );

    const headX = stepSpring(this.springHeadX, targetHeadX, 95, 14, dtSec);
    const headY = stepSpring(this.springHeadY, targetHeadY, 105, 14, dtSec);
    const headZ = stepSpring(this.springHeadZ, targetHeadZ, 88, 13, dtSec);

    const ambientBreeze =
      (Math.sin(tSec * 1.65) * 0.06 + Math.cos(tSec * 2.4) * 0.03) * (isSleeping ? 0.45 : 1);
    const hairFrontTarget = clamp(-headZ * 0.032 - headX * 0.014 + ambientBreeze, -1, 1);
    const hairSideTarget = clamp(-headZ * 0.045 - headX * 0.018 + ambientBreeze * 1.4, -1, 1);
    const hairBackTarget = clamp(-headZ * 0.055 - headX * 0.022 + ambientBreeze * 1.6, -1, 1);
    const accessoryTarget = clamp(
      -headZ * 0.048 + this.smoothSpeechEnergy * 0.14 + ambientBreeze * 1.5,
      -1,
      1
    );
    const torsoXTarget = clamp(headX * 0.32 + idleSwayX * 0.4, -10, 10);

    const hairFront = stepSpring(this.springHairFront, hairFrontTarget, 68, 7.2, dtSec);
    const hairSide = stepSpring(this.springHairSide, hairSideTarget, 58, 6.4, dtSec);
    const hairBack = stepSpring(this.springHairBack, hairBackTarget, 48, 5.8, dtSec);
    const accessory = stepSpring(this.springAccessory, accessoryTarget, 76, 6.5, dtSec);
    const torsoX = stepSpring(this.springTorsoX, torsoXTarget, 54, 9.5, dtSec);
    const ahoge = stepSpring(
      this.springAhoge,
      (isSleeping ? -0.25 : bp.ahogeLift) +
        stateAhogeLift +
        ambientBreeze * 1.8 +
        this.smoothSpeechEnergy * 0.2,
      82,
      5.5,
      dtSec
    );

    // Subtle shy blush shimmer
    const blushPulse =
      this.currentEmotion === "shy"
        ? Math.sin(tSec * 3.1) * 0.07
        : this.currentEmotion === "empathetic"
        ? Math.sin(tSec * 2.0) * 0.04
        : 0;

    // Write directly into preallocated CubismParams
    const p = this.params;
    p.ParamAngleX = headX;
    p.ParamAngleY = headY;
    p.ParamAngleZ = headZ;
    p.ParamEyeLOpen = eyesForcedClosed ? 0 : clamp(bp.eyeOpenBase * blinkFactor, 0, 1.3);
    p.ParamEyeROpen = eyesForcedClosed ? 0 : clamp(bp.eyeOpenBase * blinkFactor, 0, 1.3);
    p.ParamEyeLSmile = eyesForcedClosed ? 0 : bp.eyeSmile;
    p.ParamEyeRSmile = eyesForcedClosed ? 0 : bp.eyeSmile;
    p.ParamEyeBallX = this.smoothGazeX;
    p.ParamEyeBallY = this.smoothGazeY;
    p.ParamBrowLY = clamp(bp.browLY + stateBrowLY + this.smoothGazeY * 0.12, -1, 1);
    p.ParamBrowRY = clamp(bp.browRY + stateBrowRY + this.smoothGazeY * 0.12, -1, 1);
    p.ParamBrowLAngle = bp.browLAngle;
    p.ParamBrowRAngle = bp.browRAngle;
    p.ParamMouthOpenY = this.smoothMouthOpenY;
    p.ParamMouthForm = this.smoothMouthForm;
    p.ParamCheek = clamp(bp.cheek + blushPulse, 0, 1);
    p.ParamBreath = breathNorm;
    p.ParamBodyAngleX = torsoX;
    p.ParamBodyAngleY = breathPhase * (isSleeping ? 2.4 : 1.6);
    p.ParamBodyAngleZ = headZ * 0.25;
    p.ParamHairFront = hairFront;
    p.ParamHairSide = hairSide;
    p.ParamHairBack = hairBack;
    p.ParamAccessorySway = accessory;
    p.ParamTearSheen = isSleeping ? 0 : bp.tearSheen;
    p.speechEnergy = this.smoothSpeechEnergy;
    p.ahogeLift = ahoge;

    return p;
  }
}
