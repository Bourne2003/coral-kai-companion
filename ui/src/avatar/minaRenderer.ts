import type { EmotionTag } from "../types/ipc";
import { EMOTION_META, type CubismParams } from "./types";

const INK = "#4D3347";
const SKIN_LIGHT = "#FFF2EA";
const SKIN_MID = "#FDE1D3";
const SKIN_SHADE = "#F1BBA9";
const HAIR_DARK = "#533646";
const HAIR_BASE = "#744D60";
const HAIR_MID = "#94687B";
const HAIR_GLOSS = "#C798A9";

function clamp(v: number, min: number, max: number): number {
  return v < min ? min : v > max ? max : v;
}

function lerp(a: number, b: number, t: number): number {
  return a + (b - a) * t;
}

/**
 * Default Layered Expressive Character Engine ("Mina / มีนา").
 * Renders an original human-style anime 2D companion at 60 FPS on high-DPI Canvas 2D
 * with 2.5D head/body parallax, spring-animated hair/ribbon, multi-layered eyes,
 * cheek blush, emotion particles, and real-time viseme mouth articulation.
 */
export class MinaProceduralRenderer {
  render(
    ctx: CanvasRenderingContext2D,
    width: number,
    height: number,
    params: CubismParams,
    emotion: EmotionTag,
    nowMs: number,
    petMode: boolean
  ): void {
    ctx.clearRect(0, 0, width, height);

    const tSec = nowMs / 1000;
    const scale = Math.min(width / 540, height / 710);
    const centerX = width * 0.5;
    const centerY = height * (petMode ? 0.53 : 0.515);

    ctx.save();
    ctx.translate(centerX, centerY);
    ctx.scale(scale, scale);

    // Subtle emotion ambient aura behind Mina (disabled/softened in pet mode so desktop stays airy)
    this.drawAmbientAura(ctx, emotion, params, tSec, petMode);

    // Breathing & torso transform
    const breathY = (params.ParamBreath - 0.5) * -5.5;
    const torsoShiftX = params.ParamBodyAngleX * 1.1;
    const torsoTilt = (params.ParamBodyAngleZ * Math.PI) / 180;

    // 1. Back hair behind torso & shoulders (parallax moves opposite to head yaw)
    this.drawBackHair(ctx, params, breathY, tSec);

    // 2. Torso & cozy cardigan outfit
    ctx.save();
    ctx.translate(torsoShiftX, breathY);
    ctx.rotate(torsoTilt * 0.45);
    this.drawTorso(ctx, params);
    ctx.restore();

    // 3. Head group (2.5D parallax + tilt + speech bounce)
    const headShiftX = params.ParamAngleX * 0.72 + torsoShiftX * 0.5;
    const headShiftY = -params.ParamAngleY * 0.62 + breathY * 1.15;
    const headTilt = (params.ParamAngleZ * Math.PI) / 180;

    ctx.save();
    ctx.translate(headShiftX, headShiftY);
    ctx.rotate(headTilt * 0.62);

    this.drawNeckAndShadow(ctx, params);
    this.drawSideHairLocks(ctx, params);
    this.drawFaceBase(ctx, params);
    this.drawBlush(ctx, params, emotion);
    this.drawNose(ctx, params);
    this.drawMouth(ctx, params);
    this.drawEyes(ctx, params, emotion, tSec);
    this.drawEyebrows(ctx, params);
    this.drawFrontHairAndAhoge(ctx, params, emotion);
    this.drawHairpinAccessory(ctx, params);
    this.drawEmotionOverlayAccents(ctx, emotion, params, tSec);

    ctx.restore();
    ctx.restore();

    // Softly feather bottom edge of character waist so it never clips harshly behind UI dock
    const fadeStart = height * 0.64;
    const fadeEnd = height * 0.82;
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
    petMode: boolean
  ): void {
    const meta = EMOTION_META[emotion];
    const auraGrad = ctx.createRadialGradient(0, -35, 30, 0, -20, 260);
    auraGrad.addColorStop(0, meta.auraColor);
    auraGrad.addColorStop(0.6, petMode ? "rgba(255,255,255,0.03)" : "rgba(255,244,238,0.08)");
    auraGrad.addColorStop(1, "rgba(255,255,255,0)");

    ctx.save();
    ctx.globalAlpha = petMode ? 0.52 : 0.9;
    ctx.fillStyle = auraGrad;
    ctx.beginPath();
    ctx.arc(0, -25, 260, 0, Math.PI * 2);
    ctx.fill();

    // Subtle floating emotion motes (6 particles, zero allocations)
    for (let i = 0; i < 6; i += 1) {
      const seed = i * 1.37;
      const angle = tSec * (0.28 + (i % 3) * 0.08) + seed;
      const radius = 155 + (i % 2) * 38;
      const x = Math.cos(angle) * radius;
      const y = -65 + Math.sin(angle * 1.2 + i) * 75 - ((tSec * 14 + i * 23) % 45);
      const alpha = 0.25 + 0.25 * Math.sin(tSec * 2.2 + i * 1.7) + params.speechEnergy * 0.15;

      ctx.globalAlpha = clamp(alpha, 0.05, 0.65);
      ctx.fillStyle = meta.accentColor;

      if (emotion === "joy" || emotion === "surprised") {
        // 4-point sparkle
        const r = 4.5 + (i % 2) * 2.2;
        ctx.beginPath();
        ctx.moveTo(x, y - r);
        ctx.quadraticCurveTo(x, y, x + r, y);
        ctx.quadraticCurveTo(x, y, x, y + r);
        ctx.quadraticCurveTo(x, y, x - r, y);
        ctx.quadraticCurveTo(x, y, x, y - r);
        ctx.fill();
      } else {
        ctx.beginPath();
        ctx.arc(x, y, 2.8 + (i % 3) * 1.1, 0, Math.PI * 2);
        ctx.fill();
      }
    }
    ctx.restore();
  }

  private drawBackHair(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    breathY: number,
    tSec: number
  ): void {
    const px = -params.ParamAngleX * 0.55;
    const py = params.ParamAngleY * 0.35 + breathY * 0.7;
    const sway = params.ParamHairBack * 26 + Math.sin(tSec * 1.5) * 2.5;

    ctx.save();
    ctx.translate(px, py);

    const backGrad = ctx.createLinearGradient(0, -230, 0, 290);
    backGrad.addColorStop(0, HAIR_DARK);
    backGrad.addColorStop(0.55, HAIR_BASE);
    backGrad.addColorStop(1, "#492D3C");

    ctx.fillStyle = backGrad;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.2;
    ctx.lineJoin = "round";

    // Flowing back hair silhouette reaching past shoulders
    ctx.beginPath();
    ctx.moveTo(-118, -145);
    ctx.bezierCurveTo(-152, -45, -168 + sway * 0.4, 110, -145 + sway, 248);
    ctx.quadraticCurveTo(-96 + sway * 0.85, 222, -72 + sway * 0.7, 258);
    ctx.quadraticCurveTo(-20 + sway * 0.6, 228, 0 + sway * 0.55, 260);
    ctx.quadraticCurveTo(35 + sway * 0.6, 230, 74 + sway * 0.7, 256);
    ctx.quadraticCurveTo(108 + sway * 0.85, 220, 148 + sway, 246);
    ctx.bezierCurveTo(170 + sway * 0.4, 110, 154, -45, 118, -145);
    ctx.bezierCurveTo(75, -240, -75, -240, -118, -145);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Soft inner back-hair shading under neck
    ctx.fillStyle = "rgba(42, 22, 35, 0.35)";
    ctx.beginPath();
    ctx.ellipse(0, 75, 92, 85, 0, 0, Math.PI * 2);
    ctx.fill();

    ctx.restore();
  }

  private drawTorso(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    ctx.save();
    ctx.lineJoin = "round";
    ctx.lineCap = "round";

    // Inner blouse / chest V-neck
    ctx.fillStyle = "#F5B8C4";
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3;
    ctx.beginPath();
    ctx.moveTo(-58, 74);
    ctx.lineTo(58, 74);
    ctx.lineTo(75, 245);
    ctx.lineTo(-75, 245);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Cream cardigan left & right panels
    const cardiGrad = ctx.createLinearGradient(0, 68, 0, 290);
    cardiGrad.addColorStop(0, "#FFF9F4");
    cardiGrad.addColorStop(0.65, "#F6E9E1");
    cardiGrad.addColorStop(1, "#E9D6CD");

    ctx.fillStyle = cardiGrad;
    ctx.beginPath();
    ctx.moveTo(-42, 74);
    ctx.bezierCurveTo(-92, 84, -124, 108, -138, 175);
    ctx.lineTo(-152, 295);
    ctx.lineTo(152, 295);
    ctx.lineTo(138, 175);
    ctx.bezierCurveTo(124, 108, 92, 84, 42, 74);
    ctx.quadraticCurveTo(22, 134, 0, 152);
    ctx.quadraticCurveTo(-22, 134, -42, 74);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Cardigan soft lapel & knit seam lines
    ctx.strokeStyle = "rgba(130, 88, 104, 0.36)";
    ctx.lineWidth = 2.2;
    ctx.beginPath();
    ctx.moveTo(-36, 78);
    ctx.quadraticCurveTo(-18, 138, 0, 156);
    ctx.lineTo(0, 292);
    ctx.stroke();

    ctx.beginPath();
    ctx.moveTo(36, 78);
    ctx.quadraticCurveTo(18, 138, 0, 156);
    ctx.stroke();

    // Delicate shoulder fold lines
    ctx.beginPath();
    ctx.moveTo(-98, 135);
    ctx.quadraticCurveTo(-88, 195, -92, 275);
    ctx.moveTo(98, 135);
    ctx.quadraticCurveTo(88, 195, 92, 275);
    ctx.stroke();

    // Collar flaps
    ctx.fillStyle = "#FFFDFB";
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.6;
    ctx.beginPath();
    ctx.moveTo(-34, 72);
    ctx.lineTo(-48, 104);
    ctx.lineTo(-8, 114);
    ctx.lineTo(-22, 74);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.beginPath();
    ctx.moveTo(34, 72);
    ctx.lineTo(48, 104);
    ctx.lineTo(8, 114);
    ctx.lineTo(22, 74);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Spring-animated collar ribbon bow
    const bowSway = params.ParamAccessorySway * 11;
    ctx.save();
    ctx.translate(0, 110);
    ctx.rotate(bowSway * 0.015);

    ctx.fillStyle = "#6CA7A1";
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.4;

    // Ribbon tails
    ctx.beginPath();
    ctx.moveTo(-5, 6);
    ctx.quadraticCurveTo(-22 + bowSway * 0.6, 28, -28 + bowSway, 50);
    ctx.lineTo(-10 + bowSway * 0.7, 46);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.beginPath();
    ctx.moveTo(5, 6);
    ctx.quadraticCurveTo(22 + bowSway * 0.6, 28, 28 + bowSway, 50);
    ctx.lineTo(10 + bowSway * 0.7, 46);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Ribbon loops
    ctx.beginPath();
    ctx.moveTo(-4, 0);
    ctx.bezierCurveTo(-34, -18, -38, 16, -6, 9);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.beginPath();
    ctx.moveTo(4, 0);
    ctx.bezierCurveTo(34, -18, 38, 16, 6, 9);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Gold heart/pearl center knot
    ctx.fillStyle = "#F5CF83";
    ctx.beginPath();
    ctx.arc(0, 4, 7.5, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();
    ctx.restore();

    ctx.restore();
  }

  private drawNeckAndShadow(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const shiftX = -params.ParamAngleX * 0.15;
    ctx.save();
    ctx.fillStyle = SKIN_MID;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.8;

    ctx.beginPath();
    ctx.moveTo(-26 + shiftX, 24);
    ctx.lineTo(-28 + shiftX, 82);
    ctx.quadraticCurveTo(0, 96, 28 + shiftX, 82);
    ctx.lineTo(26 + shiftX, 24);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Chin cast shadow on neck
    ctx.fillStyle = SKIN_SHADE;
    ctx.beginPath();
    ctx.moveTo(-26 + shiftX, 26);
    ctx.quadraticCurveTo(0, 66, 26 + shiftX, 26);
    ctx.lineTo(26 + shiftX, 42);
    ctx.quadraticCurveTo(0, 62, -26 + shiftX, 42);
    ctx.closePath();
    ctx.fill();

    ctx.restore();
  }

  private drawSideHairLocks(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const sway = params.ParamHairSide * 22;
    const parallax = params.ParamAngleX * 0.28;

    ctx.save();
    ctx.fillStyle = HAIR_BASE;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3;
    ctx.lineJoin = "round";

    // Left shoulder lock
    ctx.beginPath();
    ctx.moveTo(-98 + parallax * 0.4, -75);
    ctx.bezierCurveTo(-124, 5, -122 + sway * 0.4, 98, -94 + sway, 172);
    ctx.bezierCurveTo(-78 + sway * 0.8, 128, -76 + sway * 0.3, 45, -78, -50);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Right shoulder lock
    ctx.beginPath();
    ctx.moveTo(98 + parallax * 0.4, -75);
    ctx.bezierCurveTo(124, 5, 122 + sway * 0.4, 98, 94 + sway, 172);
    ctx.bezierCurveTo(78 + sway * 0.8, 128, 76 + sway * 0.3, 45, 78, -50);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.restore();
  }

  private drawFaceBase(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const yawOffset = params.ParamAngleX * 0.38;
    const pitchOffset = -params.ParamAngleY * 0.25;

    ctx.save();
    ctx.lineJoin = "round";

    // Ears
    const drawEar = (side: -1 | 1) => {
      const earX = side * 97 - yawOffset * 0.35;
      const earY = -28 + pitchOffset * 0.3;
      ctx.fillStyle = SKIN_MID;
      ctx.strokeStyle = INK;
      ctx.lineWidth = 2.8;
      ctx.beginPath();
      ctx.ellipse(earX, earY, 13, 21, side * 0.16, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();

      ctx.strokeStyle = "#D69E92";
      ctx.lineWidth = 1.8;
      ctx.beginPath();
      ctx.arc(earX - side * 2, earY + 2, 6, side < 0 ? 1.1 : 3.2, side < 0 ? 4.3 : 0.2);
      ctx.stroke();
    };
    drawEar(-1);
    drawEar(1);

    // Head silhouette
    const skinGrad = ctx.createRadialGradient(-18 + yawOffset, -68, 22, 0, -45, 142);
    skinGrad.addColorStop(0, SKIN_LIGHT);
    skinGrad.addColorStop(0.76, SKIN_MID);
    skinGrad.addColorStop(1, "#F6C9B9");

    ctx.fillStyle = skinGrad;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.1;

    ctx.beginPath();
    ctx.moveTo(-98, -72);
    ctx.bezierCurveTo(-104, -158, -58, -198, 0, -198);
    ctx.bezierCurveTo(58, -198, 104, -158, 98, -72);
    // Soft anime cheek & dainty chin
    ctx.bezierCurveTo(98, -12, 72 + yawOffset * 0.4, 24, 14 + yawOffset * 0.65, 42 + pitchOffset * 0.35);
    ctx.quadraticCurveTo(yawOffset * 0.7, 46 + pitchOffset * 0.35, -14 + yawOffset * 0.65, 42 + pitchOffset * 0.35);
    ctx.bezierCurveTo(-72 + yawOffset * 0.4, 24, -98, -12, -98, -72);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Subtle temple hair shadow across forehead
    ctx.fillStyle = "rgba(230, 175, 165, 0.25)";
    ctx.beginPath();
    ctx.moveTo(-92, -94);
    ctx.quadraticCurveTo(0, -68, 92, -94);
    ctx.quadraticCurveTo(85, -150, 0, -165);
    ctx.quadraticCurveTo(-85, -150, -92, -94);
    ctx.closePath();
    ctx.fill();

    ctx.restore();
  }

  private drawBlush(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    emotion: EmotionTag
  ): void {
    const alpha = clamp(params.ParamCheek, 0, 1);
    if (alpha <= 0.03) return;

    const fx = params.ParamAngleX * 0.55;
    const fy = -params.ParamAngleY * 0.35;

    ctx.save();
    for (const side of [-1, 1] as const) {
      const cx = side * 52 + fx;
      const cy = -12 + fy;
      const grad = ctx.createRadialGradient(cx, cy, 3, cx, cy, 26);
      grad.addColorStop(0, `rgba(255, 126, 146, ${(0.58 * alpha).toFixed(3)})`);
      grad.addColorStop(1, "rgba(255, 126, 146, 0)");

      ctx.fillStyle = grad;
      ctx.beginPath();
      ctx.ellipse(cx, cy, 26, 14, 0, 0, Math.PI * 2);
      ctx.fill();

      // Anime shy diagonal blush strokes when shy or strong blush
      if (alpha > 0.45 || emotion === "shy") {
        ctx.strokeStyle = `rgba(232, 92, 122, ${(0.55 * alpha).toFixed(3)})`;
        ctx.lineWidth = 1.9;
        ctx.lineCap = "round";
        for (let k = -1; k <= 1; k += 1) {
          const sx = cx + k * 6.5;
          ctx.beginPath();
          ctx.moveTo(sx - 2.5, cy + 4);
          ctx.lineTo(sx + 2.5, cy - 4);
          ctx.stroke();
        }
      }
    }
    ctx.restore();
  }

  private drawNose(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const fx = params.ParamAngleX * 0.7;
    const fy = -params.ParamAngleY * 0.45;
    ctx.save();
    ctx.strokeStyle = "#C98D82";
    ctx.lineWidth = 2.0;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(fx - 1.5, -14 + fy);
    ctx.quadraticCurveTo(fx + 2.2, -10 + fy, fx - 0.5, -7 + fy);
    ctx.stroke();
    ctx.restore();
  }

  private drawMouth(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const fx = params.ParamAngleX * 0.65;
    const fy = -params.ParamAngleY * 0.4;
    const openY = clamp(params.ParamMouthOpenY, 0, 1);
    const form = clamp(params.ParamMouthForm, -1, 1);

    const mx = fx;
    const my = 16 + fy;

    ctx.save();
    ctx.lineCap = "round";
    ctx.lineJoin = "round";

    // Width narrows for rounded 'o'/'u' shapes (form < 0) and widens for smiles / 'i'/'e'
    const halfWidth = lerp(11.5, 22.5, (form + 1) * 0.5);
    const cornerLift = form * 6.2;

    if (openY < 0.07) {
      // Closed expressive mouth (smile, neutral, pout, shy wavy line)
      ctx.strokeStyle = INK;
      ctx.lineWidth = 2.5;
      ctx.beginPath();
      ctx.moveTo(mx - halfWidth, my - cornerLift * 0.55);
      ctx.quadraticCurveTo(
        mx,
        my + cornerLift * 0.65 + (form < -0.2 ? -4 : 2),
        mx + halfWidth,
        my - cornerLift * 0.55
      );
      ctx.stroke();

      // Soft lower lip shadow accent
      ctx.strokeStyle = "rgba(222, 148, 148, 0.42)";
      ctx.lineWidth = 1.6;
      ctx.beginPath();
      ctx.moveTo(mx - 6, my + 5);
      ctx.quadraticCurveTo(mx, my + 7, mx + 6, my + 5);
      ctx.stroke();
      ctx.restore();
      return;
    }

    // Open mouth articulation driven by VisemeFrame (mouthOpenY + mouthForm)
    const aperture = 4 + openY * 18.5;
    const rx = halfWidth * lerp(0.56, 0.96, (form + 1) * 0.5);
    const upperY = my - aperture * 0.25 - cornerLift * 0.28;
    const lowerY = my + aperture * 0.92;

    ctx.save();
    ctx.beginPath();
    ctx.moveTo(mx - rx, my - cornerLift * 0.42);
    ctx.quadraticCurveTo(mx, upperY, mx + rx, my - cornerLift * 0.42);
    ctx.bezierCurveTo(mx + rx * 0.88, lowerY, mx - rx * 0.88, lowerY, mx - rx, my - cornerLift * 0.42);
    ctx.closePath();

    // Inner mouth cavity
    const cavGrad = ctx.createLinearGradient(mx, upperY, mx, lowerY);
    cavGrad.addColorStop(0, "#6A2B3F");
    cavGrad.addColorStop(1, "#9B445C");
    ctx.fillStyle = cavGrad;
    ctx.fill();

    // Clip teeth & tongue neatly inside mouth path
    ctx.clip();

    // Subtle upper teeth highlight
    if (form > -0.45 && openY > 0.16) {
      ctx.fillStyle = "#FFFDFB";
      ctx.beginPath();
      ctx.ellipse(mx, upperY + 2.5, rx * 0.72, 4.6, 0, 0, Math.PI * 2);
      ctx.fill();
    }

    // Warm coral tongue arc
    ctx.fillStyle = "#E88292";
    ctx.beginPath();
    ctx.ellipse(mx, lowerY - 1.5, rx * 0.66, aperture * 0.46, 0, Math.PI, Math.PI * 2);
    ctx.fill();
    ctx.restore();

    // Crisp outer mouth contour
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.4;
    ctx.beginPath();
    ctx.moveTo(mx - rx, my - cornerLift * 0.42);
    ctx.quadraticCurveTo(mx, upperY, mx + rx, my - cornerLift * 0.42);
    ctx.bezierCurveTo(mx + rx * 0.88, lowerY, mx - rx * 0.88, lowerY, mx - rx, my - cornerLift * 0.42);
    ctx.stroke();

    ctx.restore();
  }

  private drawEyes(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    emotion: EmotionTag,
    tSec: number
  ): void {
    const fx = params.ParamAngleX * 0.56;
    const fy = -params.ParamAngleY * 0.36;

    this.drawSingleEye(ctx, -1, 44, fx, fy, params.ParamEyeLOpen, params.ParamEyeLSmile, params, emotion, tSec);
    this.drawSingleEye(ctx, 1, 44, fx, fy, params.ParamEyeROpen, params.ParamEyeRSmile, params, emotion, tSec);
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
    tSec: number
  ): void {
    const ex = side * offsetX + fx;
    const ey = -42 + fy;
    const open = clamp(eyeOpenRaw, 0, 1.28);

    // If eyes are closed (blinking) or strongly smiling (^ ^ joy arc)
    const happySquint = eyeSmile > 0.65 && open > 0.4;

    if (open < 0.14 || happySquint) {
      ctx.save();
      ctx.strokeStyle = INK;
      ctx.lineWidth = 3.5;
      ctx.lineCap = "round";

      const arcLift = happySquint ? -9.5 : 3.5;
      ctx.beginPath();
      ctx.moveTo(ex - 21, ey + 2);
      ctx.quadraticCurveTo(ex, ey + arcLift, ex + 21, ey + 2);
      ctx.stroke();

      // Cute outer eyelash wing
      ctx.lineWidth = 2.4;
      ctx.beginPath();
      ctx.moveTo(ex + side * 19, ey + 1);
      ctx.lineTo(ex + side * 25, ey - 3);
      ctx.stroke();
      ctx.restore();
      return;
    }

    const eyeW = 22.5;
    const smileLift = eyeSmile * 3.8;
    const eyeH = 21.5 * open;
    const irisX = ex + clamp(params.ParamEyeBallX, -1, 1) * 6.8;
    const irisY = ey + 1.5 - clamp(params.ParamEyeBallY, -1, 1) * 5.2;

    ctx.save();

    // Sclera shape (lower lid lifts gently when smiling!)
    ctx.beginPath();
    ctx.moveTo(ex - eyeW, ey + 2);
    ctx.bezierCurveTo(ex - eyeW * 0.55, ey - eyeH, ex + eyeW * 0.55, ey - eyeH, ex + eyeW, ey + 2);
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

    // Clip iris & upper lid shadow inside sclera
    ctx.save();
    ctx.clip();

    // Subtle upper sclera shadow
    ctx.fillStyle = "rgba(182, 165, 198, 0.28)";
    ctx.fillRect(ex - eyeW, ey - eyeH, eyeW * 2, eyeH * 0.38);

    // Iris gradient (twilight plum-violet -> warm amber-rose glow)
    const irisR = emotion === "surprised" ? 13.0 : 14.5;
    const irisGrad = ctx.createLinearGradient(irisX, irisY - irisR, irisX, irisY + irisR);
    irisGrad.addColorStop(0, "#3F2958");
    irisGrad.addColorStop(0.5, "#7E4E89");
    irisGrad.addColorStop(0.84, "#DF8B86");
    irisGrad.addColorStop(1, "#F7BC94");

    ctx.fillStyle = irisGrad;
    ctx.beginPath();
    ctx.ellipse(irisX, irisY, irisR * 0.9, irisR * 1.08, 0, 0, Math.PI * 2);
    ctx.fill();

    // Iris ring border
    ctx.strokeStyle = "#332046";
    ctx.lineWidth = 1.6;
    ctx.stroke();

    // Pupil
    const pupilScale = emotion === "surprised" ? 0.72 : emotion === "joy" ? 1.08 : 1.0;
    ctx.fillStyle = "#29183B";
    ctx.beginPath();
    ctx.ellipse(irisX, irisY - 0.5, 5.4 * pupilScale, 6.8 * pupilScale, 0, 0, Math.PI * 2);
    ctx.fill();

    // Lower iris luminous crescent reflection
    ctx.fillStyle = "rgba(255, 224, 185, 0.55)";
    ctx.beginPath();
    ctx.ellipse(irisX, irisY + 7.2, 7.6, 3.6, 0, 0, Math.PI * 2);
    ctx.fill();

    // Primary & secondary catchlights
    const twinkle = 1 + Math.sin(tSec * 4.5 + side) * 0.06;
    ctx.fillStyle = "rgba(255, 255, 255, 0.96)";
    ctx.beginPath();
    ctx.ellipse(irisX - 5.2, irisY - 5.8, 3.8 * twinkle, 3.2 * twinkle, -0.3, 0, Math.PI * 2);
    ctx.fill();

    ctx.fillStyle = "rgba(255, 255, 255, 0.85)";
    ctx.beginPath();
    ctx.arc(irisX + 4.8, irisY + 4.5, 1.9 * twinkle, 0, Math.PI * 2);
    ctx.fill();

    // Glistening tear sheen on sadness / empathy
    if (params.ParamTearSheen > 0.15) {
      ctx.fillStyle = `rgba(218, 240, 255, ${(0.78 * params.ParamTearSheen).toFixed(3)})`;
      ctx.beginPath();
      ctx.ellipse(ex + side * 6, ey + eyeH * 0.45, 5.5, 2.2, 0, 0, Math.PI * 2);
      ctx.fill();
    }

    ctx.restore(); // end sclera clip

    // Upper eyelid crease
    ctx.strokeStyle = "rgba(136, 92, 108, 0.45)";
    ctx.lineWidth = 1.6;
    ctx.beginPath();
    ctx.moveTo(ex - 16, ey - eyeH * 0.85 - 4);
    ctx.quadraticCurveTo(ex, ey - eyeH - 6, ex + 16, ey - eyeH * 0.85 - 4);
    ctx.stroke();

    // Thick expressive upper eyelash line
    ctx.strokeStyle = INK;
    ctx.lineWidth = 4.0;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(ex - eyeW - 1, ey + 2);
    ctx.bezierCurveTo(
      ex - eyeW * 0.55,
      ey - eyeH - 1.5,
      ex + eyeW * 0.55,
      ey - eyeH - 1.5,
      ex + eyeW + 2,
      ey + 2
    );
    ctx.stroke();

    // Winged outer lash accent
    ctx.lineWidth = 2.6;
    ctx.beginPath();
    ctx.moveTo(ex + side * (eyeW - 1), ey);
    ctx.lineTo(ex + side * (eyeW + 6.5), ey - 4.5);
    ctx.stroke();

    // Delicate lower lid line
    ctx.strokeStyle = "rgba(92, 60, 82, 0.42)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(ex - 13, ey + eyeH * 0.52);
    ctx.quadraticCurveTo(ex, ey + eyeH * 0.64, ex + 13, ey + eyeH * 0.52);
    ctx.stroke();

    ctx.restore();
  }

  private drawEyebrows(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const fx = params.ParamAngleX * 0.52;
    const fy = -params.ParamAngleY * 0.38;

    const drawBrow = (side: -1 | 1, browY: number, browAngle: number) => {
      const bx = side * 44 + fx;
      const by = -76 + fy - browY * 11;
      const tilt = side * browAngle * 0.28;

      ctx.save();
      ctx.translate(bx, by);
      ctx.rotate(tilt);
      ctx.strokeStyle = "#4F3242";
      ctx.lineWidth = 3.1;
      ctx.lineCap = "round";
      ctx.beginPath();
      ctx.moveTo(-17, 2);
      ctx.quadraticCurveTo(0, -4.5, 17, 2);
      ctx.stroke();
      ctx.restore();
    };

    drawBrow(-1, params.ParamBrowLY, params.ParamBrowLAngle);
    drawBrow(1, params.ParamBrowRY, params.ParamBrowRAngle);
  }

  private drawFrontHairAndAhoge(
    ctx: CanvasRenderingContext2D,
    params: CubismParams,
    emotion: EmotionTag
  ): void {
    const fx = params.ParamAngleX * 0.48;
    const fy = -params.ParamAngleY * 0.3;
    const sway = params.ParamHairFront * 14;

    ctx.save();
    ctx.translate(fx, fy);

    const hairGrad = ctx.createLinearGradient(0, -210, 0, -20);
    hairGrad.addColorStop(0, HAIR_DARK);
    hairGrad.addColorStop(0.55, HAIR_BASE);
    hairGrad.addColorStop(1, HAIR_MID);

    ctx.fillStyle = hairGrad;
    ctx.strokeStyle = INK;
    ctx.lineWidth = 3.0;
    ctx.lineJoin = "round";

    // Crown & sculpted wispy anime bangs framing forehead and temples
    ctx.beginPath();
    ctx.moveTo(-110, -76);
    ctx.bezierCurveTo(-116, -162, -64, -206, 0, -206);
    ctx.bezierCurveTo(64, -206, 116, -162, 110, -76);
    // Right temple frame strand
    ctx.quadraticCurveTo(106 + sway * 0.6, -10, 88 + sway, 32);
    ctx.quadraticCurveTo(90 + sway * 0.4, -38, 72 + sway * 0.35, -72);
    // Right bangs locks
    ctx.quadraticCurveTo(62 + sway * 0.3, -44, 48 + sway * 0.35, -48);
    ctx.quadraticCurveTo(42 + sway * 0.2, -76, 22 + sway * 0.2, -96);
    ctx.quadraticCurveTo(18 + sway * 0.25, -56, 2 + sway * 0.3, -52);
    ctx.quadraticCurveTo(-4 + sway * 0.2, -78, -14 + sway * 0.2, -96);
    // Left bangs locks
    ctx.quadraticCurveTo(-24 + sway * 0.25, -58, -44 + sway * 0.35, -50);
    ctx.quadraticCurveTo(-56 + sway * 0.25, -78, -68 + sway * 0.3, -92);
    ctx.quadraticCurveTo(-82 + sway * 0.4, -46, -88 + sway, 32);
    ctx.quadraticCurveTo(-106 + sway * 0.6, -10, -110, -76);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    // Glossy anime hair sheen highlight ribbon
    ctx.fillStyle = HAIR_GLOSS;
    ctx.globalAlpha = 0.38;
    ctx.beginPath();
    ctx.moveTo(-66, -134);
    ctx.quadraticCurveTo(-22, -146, 0, -138);
    ctx.quadraticCurveTo(26, -146, 68, -134);
    ctx.quadraticCurveTo(28, -132, 0, -126);
    ctx.quadraticCurveTo(-26, -132, -66, -134);
    ctx.closePath();
    ctx.fill();
    ctx.globalAlpha = 1;

    // Expressive top ahoge strand (perks up on joy/surprised, droops on sadness)
    const lift = params.ahogeLift * 14 + (emotion === "surprised" ? 6 : 0);
    ctx.fillStyle = HAIR_BASE;
    ctx.beginPath();
    ctx.moveTo(-4, -203);
    ctx.bezierCurveTo(-18 + sway, -242 - lift, 26 + sway * 1.2, -252 - lift, 34 + sway * 1.4, -222 - lift * 0.6);
    ctx.bezierCurveTo(18 + sway, -234 - lift * 0.8, 4 + sway * 0.4, -220 - lift * 0.3, 6, -203);
    ctx.closePath();
    ctx.fill();
    ctx.stroke();

    ctx.restore();
  }

  private drawHairpinAccessory(ctx: CanvasRenderingContext2D, params: CubismParams): void {
    const fx = params.ParamAngleX * 0.5;
    const fy = -params.ParamAngleY * 0.3;
    const sway = params.ParamAccessorySway * 5;

    ctx.save();
    ctx.translate(64 + fx + sway * 0.25, -108 + fy);
    ctx.rotate(-0.34 + sway * 0.02);

    // Gold clip bar
    ctx.fillStyle = "#F5CF83";
    ctx.strokeStyle = INK;
    ctx.lineWidth = 2.0;
    ctx.beginPath();
    ctx.roundRect(-16, -3.5, 32, 7, 3.5);
    ctx.fill();
    ctx.stroke();

    // Cute sakura-star bloom accent
    ctx.fillStyle = "#FFB4C6";
    for (let i = 0; i < 5; i += 1) {
      const a = (i * Math.PI * 2) / 5 - Math.PI / 2;
      ctx.beginPath();
      ctx.ellipse(Math.cos(a) * 6.5, Math.sin(a) * 6.5, 4.2, 2.8, a, 0, Math.PI * 2);
      ctx.fill();
    }
    ctx.fillStyle = "#FFF0A8";
    ctx.beginPath();
    ctx.arc(0, 0, 2.8, 0, Math.PI * 2);
    ctx.fill();

    ctx.restore();
  }

  private drawEmotionOverlayAccents(
    ctx: CanvasRenderingContext2D,
    emotion: EmotionTag,
    params: CubismParams,
    tSec: number
  ): void {
    if (emotion === "thinking") {
      // Three subtle floating contemplation dots near upper right temple
      ctx.save();
      for (let i = 0; i < 3; i += 1) {
        const bob = Math.sin(tSec * 2.8 - i * 0.8) * 3;
        ctx.fillStyle = "rgba(126, 120, 201, 0.72)";
        ctx.beginPath();
        ctx.arc(105 + i * 13, -135 - i * 11 + bob, 3.2 + i * 1.1, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.restore();
    } else if (emotion === "surprised" && params.ParamEyeLOpen > 1.02) {
      // Subtle anime exclamation spark lines near crown
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
