import type { AvatarLogicalState, CharacterId, EmotionTag } from "../types/ipc";
import { EMOTION_META, type CubismParams } from "./types";

const INK = "#503F3C";
const SKIN_LIGHT = "#FFF4EE";
const SKIN_BASE = "#FDECE4";
const SKIN_SHADE = "#EFC4BA";
const HAIR_BASE = "#315A70";
const HAIR_UNDERSIDE = "#234354";
const HAIR_GLOSS = "#41738D";
const SHIRT_WHITE = "#FFFFFF";
const SHIRT_FOLD = "#E6EAF0";
const BLUSH_PINK = "249, 197, 195"; // #F9C5C3

function clamp(v: number, min: number, max: number): number {
  return v < min ? min : v > max ? max : v;
}

function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

/**
 * Dual-Character Layered Expressive 60 FPS Vector Engine for Coral (คอรัล) & Kai (ไค).
 * Renders high-DPI Canvas 2D illustrations with 2.5D head/body parallax, spring-animated
 * hair/bow/necktie, cursor gaze tracking, blinking/double-blinks, 7 emotions, and viseme lip-sync.
 */
export class CompanionProceduralRenderer {
  private character: CharacterId = "coral";

  setCharacter(character: CharacterId): void {
    this.character = character;
  }

  getCharacter(): CharacterId {
    return this.character;
  }

  render(
    ctx: CanvasRenderingContext2D,
    width: number,
    height: number,
    params: CubismParams,
    emotion: EmotionTag,
    nowMs: number,
    petMode: boolean,
    characterOverride?: CharacterId,
    logicalState: AvatarLogicalState = "idle"
  ): void {
    const activeChar = characterOverride ?? this.character;
    ctx.clearRect(0, 0, width, height);

    const tSec = nowMs / 1000;
    const scale = Math.min(width / 540, height / 710);
    const centerX = width * 0.5;
    const centerY = height * (petMode ? 0.53 : 0.515);

    ctx.save();
    ctx.translate(centerX, centerY);
    ctx.scale(scale, scale);

    this.drawAmbientAura(ctx, emotion, params, tSec, petMode, logicalState);

    const breathY = (params.ParamBreath - 0.5) * (logicalState === "sleep" ? -7.2 : -5.2);
    const torsoShiftX = params.ParamBodyAngleX * 1.05;
    const torsoTilt = (params.ParamBodyAngleZ * Math.PI) / 180;

    // 1. Back / nape hair behind torso & head
    if (activeChar === "coral") {
      this.drawCoralBackHair(ctx, params, breathY, tSec);
    } else {
      this.drawKaiNapeHair(ctx, params, breathY);
    }

    // 2. Torso & outfit (white collared shirt + bow for Coral / necktie for Kai)
    ctx.save();
    ctx.translate(torsoShiftX, breathY);
    ctx.rotate(torsoTilt * 0.42);
    if (activeChar === "coral") {
      this.drawCoralTorso(ctx, params);
    } else {
      this.drawKaiTorso(ctx, params);
    }
    ctx.restore();

    // 3. Head group (2.5D parallax + tilt + speech bounce)
    const headShiftX = params.ParamAngleX * 0.7 + torsoShiftX * 0.48;
    const headShiftY = -params.ParamAngleY * 0.6 + breathY * 1.12;
    const headTilt = (params.ParamAngleZ * Math.PI) / 180;

    ctx.save();
    ctx.translate(headShiftX, headShiftY);
    ctx.rotate(headTilt * 0.6);

    this.drawNeckAndShadow(ctx, params, activeChar);
    if (activeChar === "coral") {
      this.drawCoralSideLocks(ctx, params, tSec);
    }
    this.drawFaceAndEars(ctx, params, activeChar);
    this.drawBlush(ctx, params, emotion);
    this.drawNose(ctx, params);
    this.drawMouth(ctx, params);
    this.drawEyes(ctx, params, emotion, tSec, activeChar, logicalState);
    this.drawEyebrows(ctx, params, activeChar);

    if (activeChar === "coral") {
      this.drawCoralFrontHairAndAhoge(ctx, params, emotion);
      this.drawCoralSeashellClip(ctx, params);
    } else {
      this.drawKaiFrontHairAndAhoge(ctx, params, emotion);
    }

    this.drawEmotionOverlayAccents(ctx, emotion, params, tSec, logicalState);

    ctx.restore();
    ctx.restore();

    // Softly feather bottom edge so waist blends cleanly behind bottom dock
    const fadeStart = height * 0.66;
    const fadeEnd = height * 0.84;
    ctx.save();
    ctx.globalCompositeOperation = "destination-out";
    const fadeGrad = ctx.createLinearGradient(0, fadeStart, 0, fadeEnd);
    fadeGrad.addColorStop(0, "rgba(0,0,0,0)");
    fadeGrad.addColorStop(1, "rgba(0,0,0,1)");
    ctx.fillStyle = fadeGrad;
    ctx.fillRect(0, fadeStart, width, height - fadeStart);
    ctx.restore();
  }

  private drawAmbientAura(
    ctx: CanvasRenderingContext2D,
    emotion: EmotionTag,
    params: CubismParams,
    tSec: number,
    petMode: boolean,
    logicalState: AvatarLogicalState = "idle"
  ): void {
    const meta = EMOTION_META[emotion];
    const auraGrad = ctx.createRadialGradient(0, -35, 30, 0, -20, 260);
    auraGrad.addColorStop(0, logicalState === "sleep" ? "rgba(136, 174, 210, 0.24)" : meta.auraColor);
    auraGrad.addColorStop(0.65, petMode ? "rgba(255,248,230,0.03)" : "rgba(255,246,226,0.12)");
    auraGrad.addColorStop(1, "rgba(255,255,255,0)");

    ctx.save();
    ctx.globalAlpha = petMode ? 0.48 : logicalState === "sleep" ? 0.62 : 0.85;
    ctx.fillStyle = auraGrad;
    ctx.beginPath();
    ctx.arc(0, -25, 260, 0, Math.PI * 2);
    ctx.fill();

    // Subtle pastel studio sparkle motes
    const moteSpeed = logicalState === "sleep" ? 0.45 : 1.0;
    for (let i = 0; i < 5; i += 1) {
      const seed = i * 1.41;
      const angle = tSec * moteSpeed * (0.24 + (i % 3) * 0.07) + seed;
      const radius = 160 + (i % 2) * 34;
      const x = Math.cos(angle) * radius;
      const y = -65 + Math.sin(angle * 1.15 + i) * 70 - ((tSec * 12 * moteSpeed + i * 21) % 40);
      const alpha =
        (logicalState === "sleep" ? 0.14 : 0.22) +
        0.22 * Math.sin(tSec * 2.0 + i * 1.6) +
        params.speechEnergy * 0.14;

      ctx.globalAlpha = clamp(alpha, 0.05, 0.58);
      ctx.fillStyle = emotion === "neutral" ? "#FAF2DF" : meta.accentColor;

      if (emotion === "joy" || emotion === "surprised" || emotion === "neutral") {
        const r = 4.6 + (i % 2) * 2.2;
        ctx.beginPath();
        ctx.moveTo(x, y - r);
        ctx.quadraticCurveTo(x, y, x + r, y);
        ctx.quadraticCurveTo(x, y, x, y + r);
        ctx.quadraticCurveTo(x, y, x - r, y);
        ctx.quadraticCurveTo(x, y, x, y - r);
        ctx.fill();
      } else {
        ctx.beginPath();
        ctx.arc(x, y, 2.6 + (i % 3) * 1.0, 0, Math.PI * 2);
        ctx.fill();
      }
    }
    ctx.restore();
  }

  private drawCoralBackHair(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    breathY: number,
    tSec: number
  ): void {
    const px = -params.ParamAngleX * 0.52;
    const py = params.ParamAngleY * 0.32 + breathY * 0.65;
    const sway = params.ParamHairBack * 24 + Math.sin(tSec * 1.45) * 2.4;

    ctx.save();
    ctx.translate(px, py);
    ctx.lineJoin = "round";
    ctx.lineCap = "round";

    // Dark underside depth layer behind shoulders & between waves (matches reference #234354)
    ctx.fillStyle = HAIR_UNDERSIDE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;
    ctx.beginPath();
    ctx.moveTo(-112, -135);
    ctx.bezierCurveTo(-148, -30, -162 + sway * 0.4, 95, -148 + sway * 0.9, 245);
    ctx.quadraticCurveTo(-82 + sway * 0.7, 260, 0 + sway * 0.5, 255);
    ctx.quadraticCurveTo(82 + sway * 0.7, 260, 148 + sway * 0.9, 245);
    ctx.bezierCurveTo(162 + sway * 0.4, 95, 148, -30, 112, -135);
    ctx.bezierCurveTo(72, -222, -72, -222, -112, -135);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Flowing wavy outer back locks (primary slate-blue #315A70)
    ctx.fillStyle = HAIR_BASE;
    for (const side of [-1, 1] as const) {
      const s = side;
      const w = sway * (0.85 + side * 0.1);
      ctx.beginPath();
      ctx.moveTo(s * 96, -155);
      ctx.bezierCurveTo(
        s * 142,
        -90,
        s * 158 + w * 0.3,
        -5,
        s * 138 + w * 0.5,
        62
      );
      ctx.bezierCurveTo(
        s * 184 + w * 0.6,
        118,
        s * 182 + w * 0.8,
        182,
        s * 152 + w,
        244
      );
      ctx.quadraticCurveTo(s * 138 + w * 0.9, 212, s * 134 + w * 0.8, 178);
      ctx.bezierCurveTo(
        s * 144 + w * 0.6,
        136,
        s * 124 + w * 0.45,
        84,
        s * 102 + w * 0.3,
        36
      );
      ctx.quadraticCurveTo(s * 100, -45, s * 86, -125);
      ctx.closePath();
      ctx.fill();
      ctx.stroke();
    }

    ctx.restore();
  }

  private drawKaiNapeHair(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    breathY: number
  ): void {
    const px = -params.ParamAngleX * 0.45;
    const py = params.ParamAngleY * 0.25 + breathY * 0.5;
    const sway = params.ParamHairBack * 12;

    ctx.save();
    ctx.translate(px, py);
    ctx.fillStyle = HAIR_UNDERSIDE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;
    ctx.lineJoin = "round";

    // Short spiky tousled nape tufts peeking below Kai's ears
    ctx.beginPath();
    ctx.moveTo(-98, -75);
    ctx.lineTo(-108 + sway * 0.3, -4);
    ctx.lineTo(-92 + sway * 0.4, 2);
    ctx.lineTo(-102 + sway * 0.5, 36);
    ctx.lineTo(-74 + sway * 0.5, 28);
    ctx.lineTo(-76 + sway * 0.4, 52);
    ctx.lineTo(-42 + sway * 0.3, 44);
    ctx.lineTo(0 + sway * 0.2, 54);
    ctx.lineTo(42 + sway * 0.3, 44);
    ctx.lineTo(76 + sway * 0.4, 52);
    ctx.lineTo(74 + sway * 0.5, 28);
    ctx.lineTo(102 + sway * 0.5, 36);
    ctx.lineTo(92 + sway * 0.4, 2);
    ctx.lineTo(108 + sway * 0.3, -4);
    ctx.lineTo(98, -75);
    ctx.bezierCurveTo(64, -185, -64, -185, -98, -75);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();
    ctx.restore();
  }

  private drawCoralTorso(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    ctx.save();
    ctx.lineJoin = "round";
    ctx.lineCap = "round";

    // Crisp white blouse silhouette (softer feminine shoulders)
    ctx.fillStyle = SHIRT_WHITE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;
    ctx.beginPath();
    ctx.moveTo(-36, 70);
    ctx.bezierCurveTo(-84, 78, -118, 98, -134, 158);
    ctx.lineTo(-156, 285);
    ctx.lineTo(156, 285);
    ctx.lineTo(134, 158);
    ctx.bezierCurveTo(118, 98, 84, 78, 36, 70);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Subtle crisp shirt fold shadows (#E6EAF0)
    ctx.fillStyle = SHIRT_FOLD;
    ctx.beginPath();
    ctx.moveTo(-106, 158);
    ctx.quadraticCurveTo(-92, 212, -98, 283);
    ctx.lineTo(-114, 283);
    ctx.quadraticCurveTo(-108, 212, -106, 158);
    ctx.closePath();
    ctx.fill();

    ctx.beginPath();
    ctx.moveTo(106, 158);
    ctx.quadraticCurveTo(92, 212, 98, 283);
    ctx.lineTo(114, 283);
    ctx.quadraticCurveTo(108, 212, 106, 158);
    ctx.closePath();
    ctx.fill();

    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.4;
    ctx.beginPath();
    ctx.moveTo(-105, 168);
    ctx.quadraticCurveTo(-96, 224, -101, 275);
    ctx.moveTo(105, 168);
    ctx.quadraticCurveTo(96, 224, 101, 275);
    ctx.stroke();

    // Center placket hint
    ctx.strokeStyle = SHIRT_FOLD;
    ctx.lineWidth = 2.6;
    ctx.beginPath();
    ctx.moveTo(0, 125);
    ctx.lineTo(0, 282);
    ctx.stroke();

    // White pointed collar flaps
    ctx.fillStyle = SHIRT_WHITE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.9;
    for (const side of [-1, 1] as const) {
      ctx.beginPath();
      ctx.moveTo(side * 28, 66);
      ctx.lineTo(side * 52, 102);
      ctx.lineTo(side * 12, 118);
      ctx.lineTo(side * 6, 98);
      ctx.quadraticCurveTo(side * 18, 86, side * 28, 66);
      ctx.closePath();
      ctx.fill();
      ctx.stroke();
    }

    // Plump sea-teal ribbon bow (#74AEB0) with ParamAccessorySway spring
    const sway = params.ParamAccessorySway * 10;
    ctx.save();
    ctx.translate(0, 110);
    ctx.rotate(sway * 0.012);

    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.7;

    // Hanging ribbon tails
    ctx.fillStyle = "#68A1A3";
    ctx.beginPath();
    ctx.moveTo(-6, 8);
    ctx.quadraticCurveTo(-24 + sway * 0.5, 42, -32 + sway, 82);
    ctx.lineTo(-14 + sway * 0.75, 92);
    ctx.lineTo(-2 + sway * 0.35, 30);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.beginPath();
    ctx.moveTo(6, 8);
    ctx.quadraticCurveTo(24 + sway * 0.5, 42, 32 + sway, 82);
    ctx.lineTo(14 + sway * 0.75, 92);
    ctx.lineTo(2 + sway * 0.35, 30);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Plump left & right bow loops
    ctx.fillStyle = "#74AEB0";
    for (const side of [-1, 1] as const) {
      ctx.beginPath();
      ctx.moveTo(side * 7, -4);
      ctx.bezierCurveTo(side * 34, -22, side * 56, 8, side * 38, 28);
      ctx.bezierCurveTo(side * 26, 36, side * 12, 18, side * 7, 10);
      ctx.closePath();
      ctx.fill();
      ctx.stroke();

      // Inner bow fold line
      ctx.strokeStyle = "#4F7D7F";
      ctx.lineWidth = 2.0;
      ctx.beginPath();
      ctx.moveTo(side * 11, 3);
      ctx.quadraticCurveTo(side * 24, 5, side * 31, 11);
      ctx.stroke();
      ctx.strokeStyle = INK;
      ctx.lineWidth = 2.7;
    }

    // Center ribbon knot
    ctx.fillStyle = "#74AEB0";
    ctx.beginPath();
    ctx.roundRect(-9, -8, 18, 20, 6);
    ctx.fill();
    ctx.stroke();

    ctx.restore();
    ctx.restore();
  }

  private drawKaiTorso(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    ctx.save();
    ctx.lineJoin = "round";
    ctx.lineCap = "round";

    // Broader crisp white collared shirt (#FFFFFF)
    ctx.fillStyle = SHIRT_WHITE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;
    ctx.beginPath();
    ctx.moveTo(-40, 68);
    ctx.bezierCurveTo(-96, 78, -134, 98, -152, 162);
    ctx.lineTo(-174, 285);
    ctx.lineTo(174, 285);
    ctx.lineTo(152, 162);
    ctx.bezierCurveTo(134, 98, 96, 78, 40, 68);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Subtle cool-gray shirt sleeve & shoulder fold shadows (#E6EAF0)
    ctx.fillStyle = SHIRT_FOLD;
    ctx.beginPath();
    ctx.moveTo(-124, 152);
    ctx.quadraticCurveTo(-110, 215, -116, 283);
    ctx.lineTo(-134, 283);
    ctx.quadraticCurveTo(-128, 212, -124, 152);
    ctx.closePath();
    ctx.fill();

    ctx.beginPath();
    ctx.moveTo(124, 152);
    ctx.quadraticCurveTo(110, 215, 116, 283);
    ctx.lineTo(134, 283);
    ctx.quadraticCurveTo(128, 212, 124, 152);
    ctx.closePath();
    ctx.fill();

    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.4;
    ctx.beginPath();
    ctx.moveTo(-122, 166);
    ctx.quadraticCurveTo(-112, 224, -116, 275);
    ctx.moveTo(122, 166);
    ctx.quadraticCurveTo(112, 224, 116, 275);
    ctx.stroke();

    // Sea-teal necktie (#659EA5) with subtle pendulum sway
    const sway = params.ParamAccessorySway * 9;
    ctx.save();
    ctx.translate(0, 96);

    // Tie blade
    ctx.fillStyle = "#659EA5";
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.8;
    ctx.beginPath();
    ctx.moveTo(-11, 20);
    ctx.lineTo(11, 20);
    ctx.lineTo(22 + sway, 188);
    ctx.lineTo(-22 + sway, 188);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Subtle tie shading strip
    ctx.fillStyle = "#568C93";
    ctx.beginPath();
    ctx.moveTo(-10, 22);
    ctx.lineTo(-2, 22);
    ctx.lineTo(-10 + sway, 186);
    ctx.lineTo(-19 + sway, 186);
    ctx.closePath();
    ctx.fill();

    // Tie knot
    ctx.fillStyle = "#659EA5";
    ctx.beginPath();
    ctx.moveTo(-18, -2);
    ctx.quadraticCurveTo(0, -8, 18, -2);
    ctx.lineTo(13, 22);
    ctx.quadraticCurveTo(0, 26, -13, 22);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.restore();

    // Crisp pointed white collar flaps overlapping tie knot
    ctx.fillStyle = SHIRT_WHITE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.9;
    for (const side of [-1, 1] as const) {
      ctx.beginPath();
      ctx.moveTo(side * 34, 64);
      ctx.lineTo(side * 58, 106);
      ctx.lineTo(side * 16, 114);
      ctx.lineTo(side * 14, 92);
      ctx.quadraticCurveTo(side * 24, 80, side * 34, 64);
      ctx.closePath();
      ctx.fill();
      ctx.stroke();
    }

    ctx.restore();
  }

  private drawNeckAndShadow(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    character: CharacterId
  ): void {
    const shiftX = -params.ParamAngleX * 0.15;
    const halfW = character === "kai" ? 30 : 25;

    ctx.save();
    ctx.fillStyle = SKIN_BASE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.0;
    ctx.lineJoin = "round";

    ctx.beginPath();
    ctx.moveTo(-halfW + shiftX, 24);
    ctx.lineTo(-(halfW + 3) + shiftX, 82);
    ctx.quadraticCurveTo(0, 98, halfW + 3 + shiftX, 82);
    ctx.lineTo(halfW + shiftX, 24);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Chin cast shadow on neck (#EFC4BA)
    ctx.fillStyle = SKIN_SHADE;
    ctx.beginPath();
    ctx.moveTo(-halfW + 1 + shiftX, 26);
    ctx.quadraticCurveTo(0, 68, halfW - 1 + shiftX, 26);
    ctx.lineTo(halfW - 1 + shiftX, 46);
    ctx.quadraticCurveTo(0, 64, -halfW + 1 + shiftX, 46);
    ctx.closePath();
    ctx.fill();

    ctx.restore();
  }

  private drawCoralSideLocks(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    tSec: number
  ): void {
    const sway = params.ParamHairSide * 20 + Math.sin(tSec * 1.65) * 1.8;
    const parallax = params.ParamAngleX * 0.25;

    ctx.save();
    ctx.fillStyle = HAIR_BASE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.1;
    ctx.lineJoin = "round";

    // Wavy front shoulder locks draping gracefully over white blouse
    for (const side of [-1, 1] as const) {
      ctx.beginPath();
      ctx.moveTo(side * 96 + parallax * 0.35, -72);
      ctx.bezierCurveTo(
        side * 124,
        4,
        side * 128 + sway * 0.35,
        84,
        side * 100 + sway * 0.75,
        154
      );
      ctx.bezierCurveTo(
        side * 86 + sway * 0.9,
        186,
        side * 98 + sway,
        214,
        side * 80 + sway,
        232
      );
      ctx.bezierCurveTo(
        side * 72 + sway * 0.8,
        182,
        side * 86 + sway * 0.5,
        125,
        side * 74 + sway * 0.3,
        66
      );
      ctx.quadraticCurveTo(side * 78, -10, side * 78, -55);
      ctx.closePath();
      ctx.fill();
      ctx.stroke();
    }

    ctx.restore();
  }

  private drawFaceAndEars(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    character: CharacterId
  ): void {
    const yawOffset = params.ParamAngleX * 0.36;
    const pitchOffset = -params.ParamAngleY * 0.24;

    ctx.save();
    ctx.lineJoin = "round";

    // Rounded ears (both visible on Kai; slight right-side peek on Coral matching ref)
    const drawEar = (side: -1 | 1) => {
      const earX = side * 96 - yawOffset * 0.35;
      const earY = -26 + pitchOffset * 0.28;
      ctx.fillStyle = SKIN_BASE;
      ctx.strokeStyle = INK;
      ctx.lineWidth = 2.9;
      ctx.beginPath();
      ctx.ellipse(earX, earY, 13.5, 20, side * 0.15, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();

      ctx.strokeStyle = SKIN_SHADE;
      ctx.lineWidth = 2.1;
      ctx.beginPath();
      ctx.arc(earX - side * 1.5, earY + 1, 6.2, side < 0 ? 1.0 : 3.2, side < 0 ? 4.3 : 0.2);
      ctx.stroke();
    };

    if (character === "kai") {
      drawEar(-1);
      drawEar(1);
    } else {
      drawEar(1);
    }

    // Soft pastel warm cream/peach head contour
    const skinGrad = ctx.createRadialGradient(-16 + yawOffset, -68, 22, 0, -45, 145);
    skinGrad.addColorStop(0, SKIN_LIGHT);
    skinGrad.addColorStop(0.75, SKIN_BASE);
    skinGrad.addColorStop(1, "#F7D9CE");

    ctx.fillStyle = skinGrad;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;

    ctx.beginPath();
    ctx.moveTo(-94, -74);
    ctx.bezierCurveTo(-100, -158, -56, -196, 0, -196);
    ctx.bezierCurveTo(56, -196, 100, -158, 94, -74);
    ctx.bezierCurveTo(
      94,
      -10,
      68 + yawOffset * 0.4,
      26,
      15 + yawOffset * 0.65,
      43 + pitchOffset * 0.35
    );
    ctx.quadraticCurveTo(
      yawOffset * 0.7,
      47 + pitchOffset * 0.35,
      -15 + yawOffset * 0.65,
      43 + pitchOffset * 0.35
    );
    ctx.bezierCurveTo(-68 + yawOffset * 0.4, 26, -94, -10, -94, -74);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.restore();
  }

  private drawBlush(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    emotion: EmotionTag
  ): void {
    const baseCheek = Math.max(0.45, params.ParamCheek);
    const alpha = clamp(baseCheek, 0, 1);

    const fx = params.ParamAngleX * 0.54;
    const fy = -params.ParamAngleY * 0.34;

    ctx.save();
    for (const side of [-1, 1] as const) {
      const cx = side * 51 + fx;
      const cy = -12 + fy;

      // Soft pink oval cheek blush matching #F9C5C3 in both reference illustrations
      ctx.fillStyle = `rgba(${BLUSH_PINK}, ${(0.72 * alpha).toFixed(3)})`;
      ctx.beginPath();
      ctx.ellipse(cx, cy, 21, 10, 0, 0, Math.PI * 2);
      ctx.fill();

      if (emotion === "shy" || params.ParamCheek > 0.65) {
        ctx.strokeStyle = `rgba(232, 104, 126, ${(0.52 * alpha).toFixed(3)})`;
        ctx.lineWidth = 1.9;
        ctx.lineCap = "round";
        for (let k = -1; k <= 1; k += 1) {
          const sx = cx + k * 6.5;
          ctx.beginPath();
          ctx.moveTo(sx - 2.4, cy + 3.8);
          ctx.lineTo(sx + 2.4, cy - 3.8);
          ctx.stroke();
        }
      }
    }
    ctx.restore();
  }

  private drawNose(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const fx = params.ParamAngleX * 0.68;
    const fy = -params.ParamAngleY * 0.42;
    ctx.save();
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.0;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(fx - 1.0, -15 + fy);
    ctx.quadraticCurveTo(fx + 2.0, -11.5 + fy, fx - 0.5, -8.5 + fy);
    ctx.stroke();
    ctx.restore();
  }

  private drawMouth(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const fx = params.ParamAngleX * 0.64;
    const fy = -params.ParamAngleY * 0.38;
    const openY = clamp(params.ParamMouthOpenY, 0, 1);
    const form = clamp(params.ParamMouthForm, -1, 1);

    const mx = fx;
    const my = 15 + fy;

    ctx.save();
    ctx.lineCap = "round";
    ctx.lineJoin = "round";

    const halfWidth = lerp(12, 23, (form + 1) * 0.5);
    const cornerLift = form * 6.5;

    if (openY < 0.07) {
      ctx.strokeStyle = INK;
      ctx.lineWidth = 2.7;
      ctx.beginPath();
      ctx.moveTo(mx - halfWidth, my - cornerLift * 0.52);
      ctx.quadraticCurveTo(
        mx,
        my + cornerLift * 0.72 + (form < -0.2 ? -4 : 2),
        mx + halfWidth,
        my - cornerLift * 0.52
      );
      ctx.stroke();
      ctx.restore();
      return;
    }

    const aperture = 4 + openY * 18.0;
    const rx = halfWidth * lerp(0.55, 0.94, (form + 1) * 0.5);
    const upperY = my - aperture * 0.24 - cornerLift * 0.25;
    const lowerY = my + aperture * 0.9;

    ctx.save();
    ctx.beginPath();
    ctx.moveTo(mx - rx, my - cornerLift * 0.4);
    ctx.quadraticCurveTo(mx, upperY, mx + rx, my - cornerLift * 0.4);
    ctx.bezierCurveTo(
      mx + rx * 0.88,
      lowerY,
      mx - rx * 0.88,
      lowerY,
      mx - rx,
      my - cornerLift * 0.4
    );
    ctx.closePath();

    const cavGrad = ctx.createLinearGradient(mx, upperY, mx, lowerY);
    cavGrad.addColorStop(0, "#6E303D");
    cavGrad.addColorStop(1, "#A65163");
    ctx.fillStyle = cavGrad;
    ctx.fill();
    ctx.clip();

    if (form > -0.45 && openY > 0.16) {
      ctx.fillStyle = "#FFFDFB";
      ctx.beginPath();
      ctx.ellipse(mx, upperY + 2.4, rx * 0.72, 4.4, 0, 0, Math.PI * 2);
      ctx.fill();
    }

    ctx.fillStyle = "#EB8C96";
    ctx.beginPath();
    ctx.ellipse(mx, lowerY - 1.5, rx * 0.65, aperture * 0.45, 0, Math.PI, Math.PI * 2);
    ctx.fill();
    ctx.restore();

    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.5;
    ctx.beginPath();
    ctx.moveTo(mx - rx, my - cornerLift * 0.4);
    ctx.quadraticCurveTo(mx, upperY, mx + rx, my - cornerLift * 0.4);
    ctx.bezierCurveTo(
      mx + rx * 0.88,
      lowerY,
      mx - rx * 0.88,
      lowerY,
      mx - rx,
      my - cornerLift * 0.4
    );
    ctx.stroke();

    ctx.restore();
  }

  private drawEyes(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    emotion: EmotionTag,
    tSec: number,
    character: CharacterId,
    logicalState: AvatarLogicalState = "idle"
  ): void {
    const fx = params.ParamAngleX * 0.56;
    const fy = -params.ParamAngleY * 0.36;

    this.drawSingleEye(
      ctx,
      -1,
      43,
      fx,
      fy,
      params.ParamEyeLOpen,
      params.ParamEyeLSmile,
      params,
      emotion,
      tSec,
      character,
      logicalState
    );
    this.drawSingleEye(
      ctx,
      1,
      43,
      fx,
      fy,
      params.ParamEyeROpen,
      params.ParamEyeRSmile,
      params,
      emotion,
      tSec,
      character,
      logicalState
    );
  }

  private drawFeminineLashes(
    ctx: CanvasRenderingContext2D,
    side: -1 | 1,
    anchorX: number,
    anchorY: number
  ): void {
    ctx.save();
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.3;
    ctx.lineCap = "round";

    // Delicate 3-lash feminine outer eyelashes matching Coral reference
    const lashes: Array<[number, number, number, number]> = [
      [0, -1, 6.5, -5.5],
      [1, 1.5, 7.2, -1.0],
      [0.5, 4.0, 5.5, 3.0],
    ];
    for (const [dx0, dy0, dx1, dy1] of lashes) {
      ctx.beginPath();
      ctx.moveTo(anchorX + side * dx0, anchorY + dy0);
      ctx.lineTo(anchorX + side * dx1, anchorY + dy1);
      ctx.stroke();
    }
    ctx.restore();
  }

  private drawSingleEye(
    ctx: CanvasRenderingContext2D,
    side: -1 | 1,
    offsetX: number,
    fx: number,
    fy: number,
    eyeOpenRaw: number,
    eyeSmile: number,
    params: CubismParams,
    emotion: EmotionTag,
    tSec: number,
    character: CharacterId,
    logicalState: AvatarLogicalState = "idle"
  ): void {
    const ex = side * offsetX + fx;
    const ey = -41 + fy;
    const open = clamp(eyeOpenRaw, 0, 1.28);

    // Happy crescent smile eyes (^ ^) on Joy or serene closed eyelids on Sleep / blink
    const isSleeping = logicalState === "sleep";
    const happySquint = !isSleeping && eyeSmile > 0.6 && open > 0.4;

    if (isSleeping || open < 0.14 || happySquint) {
      ctx.save();
      ctx.strokeStyle = INK;
      ctx.lineWidth = isSleeping ? 3.6 : 4.0;
      ctx.lineCap = "round";

      const arcLift = happySquint ? -11.5 : isSleeping ? 9.5 : 3.5;
      const yBase = isSleeping ? ey + 0.5 : ey + 2.5;
      ctx.beginPath();
      ctx.moveTo(ex - 21, yBase);
      ctx.quadraticCurveTo(ex, yBase + arcLift, ex + 21, yBase);
      ctx.stroke();

      if (isSleeping) {
        // Soft delicate upper lid crease & serene resting lashes
        ctx.strokeStyle = "rgba(140, 102, 96, 0.35)";
        ctx.lineWidth = 1.4;
        ctx.beginPath();
        ctx.moveTo(ex - 15, yBase - 4.5);
        ctx.quadraticCurveTo(ex, yBase - 1.5, ex + 15, yBase - 4.5);
        ctx.stroke();

        if (character === "coral") {
          ctx.strokeStyle = INK;
          ctx.lineWidth = 2.0;
          for (let i = 0; i < 2; i += 1) {
            const lx = ex + side * (16 + i * 3);
            const ly = yBase + 2 + i * 1.4;
            ctx.beginPath();
            ctx.moveTo(lx, ly);
            ctx.lineTo(lx + side * 4.5, ly + 3.2);
            ctx.stroke();
          }
        }
      } else if (character === "coral" && happySquint) {
        this.drawFeminineLashes(ctx, side, ex + side * 19, ey - 1);
      }
      ctx.restore();
      return;
    }

    const eyeW = character === "coral" ? 21.5 : 20.5;
    const smileLift = eyeSmile * 3.5;
    const eyeH = (character === "coral" ? 21.0 : 19.0) * open;
    const irisX = ex + clamp(params.ParamEyeBallX, -1, 1) * 6.5;
    const irisY = ey + 1.5 - clamp(params.ParamEyeBallY, -1, 1) * 4.8;

    ctx.save();

    // Sclera
    ctx.beginPath();
    ctx.moveTo(ex - eyeW, ey + 2);
    ctx.bezierCurveTo(
      ex - eyeW * 0.55,
      ey - eyeH,
      ex + eyeW * 0.55,
      ey - eyeH,
      ex + eyeW,
      ey + 2
    );
    ctx.bezierCurveTo(
      ex + eyeW * 0.52,
      ey + eyeH * 0.72 - smileLift,
      ex - eyeW * 0.52,
      ey + eyeH * 0.72 - smileLift,
      ex - eyeW,
      ey + 2
    );
    ctx.closePath();

    ctx.fillStyle = "#FFFDFC";
    ctx.fill();

    ctx.save();
    ctx.clip();

    // Soft upper sclera shadow
    ctx.fillStyle = "rgba(170, 178, 194, 0.25)";
    ctx.fillRect(ex - eyeW, ey - eyeH, eyeW * 2, eyeH * 0.36);

    // Deep ocean-teal iris harmonizing with slate-blue hair & teal ribbon/tie
    const irisR = emotion === "surprised" ? 12.2 : 13.8;
    const irisGrad = ctx.createLinearGradient(irisX, irisY - irisR, irisX, irisY + irisR);
    irisGrad.addColorStop(0, "#1E4355");
    irisGrad.addColorStop(0.5, "#326B81");
    irisGrad.addColorStop(0.86, "#5FA1AC");
    irisGrad.addColorStop(1, "#90CFD4");

    ctx.fillStyle = irisGrad;
    ctx.beginPath();
    ctx.ellipse(irisX, irisY, irisR * 0.9, irisR * 1.06, 0, 0, Math.PI * 2);
    ctx.fill();

    ctx.strokeStyle = "#1B3442";
    ctx.lineWidth = 1.6;
    ctx.stroke();

    // Pupil
    const pupilScale = emotion === "surprised" ? 0.72 : 1.0;
    ctx.fillStyle = "#192A36";
    ctx.beginPath();
    ctx.ellipse(irisX, irisY - 0.5, 5.1 * pupilScale, 6.4 * pupilScale, 0, 0, Math.PI * 2);
    ctx.fill();

    // Lower iris luminous crescent
    ctx.fillStyle = "rgba(182, 238, 240, 0.45)";
    ctx.beginPath();
    ctx.ellipse(irisX, irisY + 6.8, 7.0, 3.2, 0, 0, Math.PI * 2);
    ctx.fill();

    // Crisp white catchlights
    const twinkle = 1 + Math.sin(tSec * 4.2 + side) * 0.05;
    ctx.fillStyle = "rgba(255, 255, 255, 0.97)";
    ctx.beginPath();
    ctx.ellipse(irisX - 4.9, irisY - 5.4, 3.6 * twinkle, 3.0 * twinkle, -0.3, 0, Math.PI * 2);
    ctx.fill();

    ctx.fillStyle = "rgba(255, 255, 255, 0.88)";
    ctx.beginPath();
    ctx.arc(irisX + 4.5, irisY + 4.2, 1.8 * twinkle, 0, Math.PI * 2);
    ctx.fill();

    // Tear sheen on sadness / empathy
    if (params.ParamTearSheen > 0.15) {
      ctx.fillStyle = `rgba(218, 244, 255, ${(0.82 * params.ParamTearSheen).toFixed(3)})`;
      ctx.beginPath();
      ctx.ellipse(ex + side * 6, ey + eyeH * 0.45, 5.2, 2.2, 0, 0, Math.PI * 2);
      ctx.fill();
    }

    ctx.restore(); // end sclera clip

    // Upper eyelid crease
    ctx.strokeStyle = "rgba(140, 102, 96, 0.36)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(ex - 15, ey - eyeH * 0.85 - 4);
    ctx.quadraticCurveTo(ex, ey - eyeH - 6, ex + 15, ey - eyeH * 0.85 - 4);
    ctx.stroke();

    // Clean warm dark-brown upper eyelash line
    ctx.strokeStyle = INK;
    ctx.lineWidth = character === "coral" ? 3.8 : 3.4;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(ex - eyeW - 1, ey + 2);
    ctx.bezierCurveTo(
      ex - eyeW * 0.55,
      ey - eyeH - 1.5,
      ex + eyeW * 0.55,
      ey - eyeH - 1.5,
      ex + eyeW + 1,
      ey + 2
    );
    ctx.stroke();

    if (character === "coral") {
      this.drawFeminineLashes(ctx, side, ex + side * eyeW, ey);
    }

    // Subtle lower lid hint
    ctx.strokeStyle = "rgba(80, 63, 60, 0.36)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(ex - 12, ey + eyeH * 0.52);
    ctx.quadraticCurveTo(ex, ey + eyeH * 0.62, ex + 12, ey + eyeH * 0.52);
    ctx.stroke();

    ctx.restore();
  }

  private drawEyebrows(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    character: CharacterId
  ): void {
    const fx = params.ParamAngleX * 0.52;
    const fy = -params.ParamAngleY * 0.36;

    const drawBrow = (side: -1 | 1, browY: number, browAngle: number) => {
      const bx = side * 43 + fx;
      const by = -72 + fy - browY * 11;
      const tilt = side * browAngle * 0.28;

      ctx.save();
      ctx.translate(bx, by);
      ctx.rotate(tilt);
      ctx.strokeStyle = INK;
      ctx.lineWidth = character === "kai" ? 3.3 : 2.9;
      ctx.lineCap = "round";
      ctx.beginPath();
      ctx.moveTo(-16, 2);
      ctx.quadraticCurveTo(0, -4.5, 16, 2);
      ctx.stroke();
      ctx.restore();
    };

    drawBrow(-1, params.ParamBrowLY, params.ParamBrowLAngle);
    drawBrow(1, params.ParamBrowRY, params.ParamBrowRAngle);
  }

  private drawAhoge(
    ctx: CanvasRenderingContext2D,
    sway: number,
    lift: number
  ): void {
    // Curved top ahoge pointing upward-left matching both Coral and Kai illustrations
    ctx.fillStyle = HAIR_BASE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.1;
    ctx.lineJoin = "round";
    ctx.beginPath();
    ctx.moveTo(6, -196);
    ctx.bezierCurveTo(
      20 + sway * 0.4,
      -236 - lift,
      -18 + sway * 1.1,
      -256 - lift * 1.1,
      -34 + sway * 1.35,
      -242 - lift * 0.85
    );
    ctx.bezierCurveTo(
      -8 + sway * 0.85,
      -236 - lift * 0.8,
      2 + sway * 0.3,
      -218 - lift * 0.3,
      -6,
      -196
    );
    ctx.closePath();
    ctx.fill();
    ctx.stroke();
  }

  private drawCoralFrontHairAndAhoge(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    emotion: EmotionTag
  ): void {
    const fx = params.ParamAngleX * 0.46;
    const fy = -params.ParamAngleY * 0.28;
    const sway = params.ParamHairFront * 12;

    ctx.save();
    ctx.translate(fx, fy);

    ctx.fillStyle = HAIR_BASE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;
    ctx.lineJoin = "round";

    // Crown & layered swept bangs matching Coral's illustration
    ctx.beginPath();
    ctx.moveTo(-104, -78);
    ctx.bezierCurveTo(-110, -160, -62, -200, 0, -200);
    ctx.bezierCurveTo(62, -200, 110, -160, 104, -78);
    // Right temple frame
    ctx.quadraticCurveTo(96 + sway * 0.45, -18, 86 + sway * 0.7, 22);
    ctx.quadraticCurveTo(84 + sway * 0.35, -36, 72 + sway * 0.25, -76);
    // Right swept bangs locks
    ctx.quadraticCurveTo(56 + sway * 0.28, -54, 42 + sway * 0.3, -52);
    ctx.quadraticCurveTo(40 + sway * 0.2, -78, 22 + sway * 0.2, -96);
    // Center swept lock curving slightly right
    ctx.quadraticCurveTo(22 + sway * 0.25, -58, 6 + sway * 0.3, -54);
    ctx.quadraticCurveTo(-8 + sway * 0.2, -72, -18 + sway * 0.2, -104);
    // Left bangs locks
    ctx.quadraticCurveTo(-24 + sway * 0.25, -58, -44 + sway * 0.3, -50);
    ctx.quadraticCurveTo(-48 + sway * 0.2, -78, -62 + sway * 0.2, -92);
    ctx.quadraticCurveTo(-76 + sway * 0.3, -54, -84 + sway * 0.6, 18);
    ctx.quadraticCurveTo(-100 + sway * 0.4, -22, -104, -78);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Subtle soft slate-blue crown sheen
    ctx.fillStyle = HAIR_GLOSS;
    ctx.globalAlpha = 0.22;
    ctx.beginPath();
    ctx.moveTo(-58, -142);
    ctx.quadraticCurveTo(0, -158, 58, -142);
    ctx.quadraticCurveTo(20, -138, 0, -134);
    ctx.quadraticCurveTo(-20, -138, -58, -142);
    ctx.closePath();
    ctx.fill();
    ctx.globalAlpha = 1;

    const lift = params.ahogeLift * 13 + (emotion === "surprised" ? 6 : 0);
    this.drawAhoge(ctx, sway, lift);

    ctx.restore();
  }

  private drawCoralSeashellClip(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const fx = params.ParamAngleX * 0.48;
    const fy = -params.ParamAngleY * 0.28;
    const sway = params.ParamAccessorySway * 4;

    // Coral-pink scalloped seashell hairclip (#FFA58A) on viewer's upper-right temple hair
    ctx.save();
    ctx.translate(65 + fx + sway * 0.22, -112 + fy);
    ctx.rotate(0.28 + sway * 0.015);
    ctx.lineJoin = "round";
    ctx.lineCap = "round";

    ctx.fillStyle = "#FFA58A";
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.6;

    // Scalloped shell fan silhouette
    ctx.beginPath();
    ctx.moveTo(-10, 14);
    ctx.quadraticCurveTo(-24, 2, -20, -10);
    ctx.quadraticCurveTo(-15, -18, -9, -15);
    ctx.quadraticCurveTo(-5, -23, 0, -18);
    ctx.quadraticCurveTo(5, -23, 9, -15);
    ctx.quadraticCurveTo(15, -18, 20, -10);
    ctx.quadraticCurveTo(24, 2, 10, 14);
    ctx.quadraticCurveTo(6, 20, 0, 19);
    ctx.quadraticCurveTo(-6, 20, -10, 14);
    ctx.closePath();
    ctx.fill();

    // Subtle coral shadow near bottom hinge
    ctx.fillStyle = "#F08A6C";
    ctx.beginPath();
    ctx.ellipse(0, 12, 9, 5, 0, 0, Math.PI * 2);
    ctx.fill();

    ctx.stroke();

    // Dark-brown radiating shell ribs
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.0;
    const ribs: Array<[number, number, number, number]> = [
      [-5, 11, -12, -10],
      [0, 11, 0, -13],
      [5, 11, 12, -10],
    ];
    for (const [x0, y0, x1, y1] of ribs) {
      ctx.beginPath();
      ctx.moveTo(x0, y0);
      ctx.lineTo(x1, y1);
      ctx.stroke();
    }

    ctx.restore();
  }

  private drawKaiFrontHairAndAhoge(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    emotion: EmotionTag
  ): void {
    const fx = params.ParamAngleX * 0.46;
    const fy = -params.ParamAngleY * 0.28;
    const sway = params.ParamHairFront * 12;

    ctx.save();
    ctx.translate(fx, fy);

    ctx.fillStyle = HAIR_BASE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;
    ctx.lineJoin = "round";

    // Short tousled spiky-tufted hair + loose parted bangs matching Kai's illustration
    ctx.beginPath();
    // Left tousled side spikes flaring past ear
    ctx.moveTo(-98, -42);
    ctx.lineTo(-124 + sway * 0.35, -38);
    ctx.quadraticCurveTo(-108, -56, -108, -74);
    ctx.lineTo(-122 + sway * 0.25, -82);
    ctx.quadraticCurveTo(-105, -96, -102, -118);
    ctx.lineTo(-118 + sway * 0.2, -138);
    ctx.quadraticCurveTo(-92, -148, -82, -166);
    // Crown
    ctx.bezierCurveTo(-56, -196, -24, -200, 0, -199);
    ctx.bezierCurveTo(28, -200, 62, -194, 86, -164);
    // Right tousled side spikes flaring past ear
    ctx.lineTo(116 + sway * 0.2, -142);
    ctx.quadraticCurveTo(100, -122, 102, -102);
    ctx.lineTo(120 + sway * 0.25, -92);
    ctx.quadraticCurveTo(106, -74, 108, -56);
    ctx.lineTo(124 + sway * 0.35, -36);
    ctx.quadraticCurveTo(104, -36, 94, -44);
    // Right temple & loose parted bangs
    ctx.quadraticCurveTo(92 + sway * 0.3, -64, 82 + sway * 0.2, -78);
    ctx.quadraticCurveTo(72 + sway * 0.25, -52, 58 + sway * 0.3, -48);
    ctx.quadraticCurveTo(56 + sway * 0.2, -82, 40 + sway * 0.2, -98);
    ctx.quadraticCurveTo(36 + sway * 0.25, -62, 20 + sway * 0.3, -52);
    ctx.quadraticCurveTo(14 + sway * 0.2, -84, 2 + sway * 0.2, -108);
    ctx.quadraticCurveTo(-6 + sway * 0.2, -72, -18 + sway * 0.25, -54);
    ctx.quadraticCurveTo(-24 + sway * 0.2, -82, -36 + sway * 0.2, -102);
    ctx.quadraticCurveTo(-44 + sway * 0.25, -64, -58 + sway * 0.3, -50);
    ctx.quadraticCurveTo(-72 + sway * 0.2, -74, -84 + sway * 0.2, -84);
    ctx.quadraticCurveTo(-92 + sway * 0.3, -62, -98, -42);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Subtle crown gloss
    ctx.fillStyle = HAIR_GLOSS;
    ctx.globalAlpha = 0.2;
    ctx.beginPath();
    ctx.moveTo(-54, -146);
    ctx.quadraticCurveTo(0, -160, 54, -146);
    ctx.quadraticCurveTo(18, -142, 0, -138);
    ctx.quadraticCurveTo(-18, -142, -54, -146);
    ctx.closePath();
    ctx.fill();
    ctx.globalAlpha = 1;

    const lift = params.ahogeLift * 13 + (emotion === "surprised" ? 6 : 0);
    this.drawAhoge(ctx, sway, lift);

    ctx.restore();
  }

  private drawEmotionOverlayAccents(
    ctx: CanvasRenderingContext2D,
    emotion: EmotionTag,
    params: CubismParams,
    tSec: number,
    logicalState: AvatarLogicalState = "idle"
  ): void {
    if (logicalState === "sleep") {
      ctx.save();
      const letters = ["z", "Z", "z"];
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      for (let i = 0; i < 3; i += 1) {
        const cycle = (tSec * 0.36 + i * 0.33) % 1;
        const x = 92 + cycle * 42 + Math.sin(tSec * 1.4 + i * 1.5) * 6;
        const y = -112 - cycle * 64;
        const alpha = Math.sin(cycle * Math.PI) * 0.78;
        const size = 14 + i * 3 + cycle * 5;
        ctx.globalAlpha = clamp(alpha, 0, 0.82);
        ctx.font = `700 ${Math.round(size)}px "SF Pro Rounded", -apple-system, sans-serif`;
        ctx.fillStyle = "#5B8499";
        ctx.fillText(letters[i], x, y);
      }
      ctx.restore();
      return;
    }

    if (logicalState === "write") {
      ctx.save();
      for (let i = 0; i < 3; i += 1) {
        const pulse = (tSec * 1.2 + i * 0.35) % 1;
        const x = 104 + i * 12;
        const y = -82 - pulse * 18;
        ctx.globalAlpha = Math.sin(pulse * Math.PI) * 0.65;
        ctx.fillStyle = "#5E9B96";
        ctx.beginPath();
        ctx.arc(x, y, 2.4 + (i % 2), 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.restore();
      return;
    }

    if (emotion === "thinking" || logicalState === "think") {
      ctx.save();
      for (let i = 0; i < 3; i += 1) {
        const bob = Math.sin(tSec * 2.8 - i * 0.8) * 3;
        ctx.fillStyle = "rgba(92, 114, 168, 0.72)";
        ctx.beginPath();
        ctx.arc(105 + i * 13, -135 - i * 11 + bob, 3.2 + i * 1.1, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.restore();
    } else if (emotion === "surprised" && params.ParamEyeLOpen > 1.02) {
      ctx.save();
      ctx.strokeStyle = "#E3A43B";
      ctx.lineWidth = 2.8;
      ctx.lineCap = "round";
      ctx.beginPath();
      ctx.moveTo(86, -170);
      ctx.lineTo(100, -186);
      ctx.moveTo(98, -155);
      ctx.lineTo(116, -162);
      ctx.stroke();
      ctx.restore();
    }
  }
}
