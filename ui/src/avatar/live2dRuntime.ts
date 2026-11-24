import type { EmotionTag } from "../types/ipc";
import type { Live2dFolderFiles } from "../api/tauriClient";
import { MinaProceduralRenderer } from "./minaRenderer";
import type { CubismParams } from "./types";

export interface CubismModel3Json {
  Version?: number;
  FileReferences?: {
    Moc?: string;
    Textures?: string[];
    Physics?: string;
    Pose?: string;
    DisplayInfo?: string;
    Expressions?: Array<{ Name: string; File: string }>;
    Motions?: Record<string, Array<{ File: string; FadeInTime?: number; FadeOutTime?: number }>>;
  };
  Groups?: Array<{
    Target: string;
    Name: string;
    Ids: string[];
  }>;
}

export interface CubismExp3Json {
  Type?: string;
  Parameters?: Array<{
    Id: string;
    Value: number;
    Blend?: "Add" | "Multiply" | "Overwrite";
  }>;
}

export interface CubismMotionCurve {
  Target: string;
  Id: string;
  Segments: number[];
}

export interface CubismMotion3Json {
  Version?: number;
  Meta?: {
    Duration?: number;
    Fps?: number;
    Loop?: boolean;
    CurveCount?: number;
  };
  Curves?: CubismMotionCurve[];
}

export interface Live2dBundleManifest {
  id?: string;
  name?: string;
  version?: string;
  recommendedRamMb?: number;
}

export interface LoadedLive2dModel {
  id: string;
  name: string;
  model3: CubismModel3Json;
  manifest?: Live2dBundleManifest;
  expressions: Map<string, CubismExp3Json>;
  idleMotion: CubismMotion3Json | null;
  textures: HTMLImageElement[];
  layers: Map<string, HTMLImageElement>;
  objectUrls: string[];
  sourceLabel: string;
}

const DEFAULT_EXP3_PRESETS: Record<EmotionTag, CubismExp3Json> = {
  joy: {
    Type: "Live2D Expression",
    Parameters: [
      { Id: "ParamEyeLSmile", Value: 0.75, Blend: "Overwrite" },
      { Id: "ParamEyeRSmile", Value: 0.75, Blend: "Overwrite" },
      { Id: "ParamMouthForm", Value: 0.85, Blend: "Overwrite" },
      { Id: "ParamCheek", Value: 0.5, Blend: "Overwrite" },
    ],
  },
  sadness: {
    Type: "Live2D Expression",
    Parameters: [
      { Id: "ParamEyeLOpen", Value: 0.82, Blend: "Multiply" },
      { Id: "ParamEyeROpen", Value: 0.82, Blend: "Multiply" },
      { Id: "ParamBrowLAngle", Value: -0.7, Blend: "Overwrite" },
      { Id: "ParamBrowRAngle", Value: -0.7, Blend: "Overwrite" },
      { Id: "ParamMouthForm", Value: -0.6, Blend: "Overwrite" },
    ],
  },
  shy: {
    Type: "Live2D Expression",
    Parameters: [
      { Id: "ParamCheek", Value: 0.98, Blend: "Overwrite" },
      { Id: "ParamAngleZ", Value: -7, Blend: "Add" },
      { Id: "ParamMouthForm", Value: 0.4, Blend: "Overwrite" },
    ],
  },
  surprised: {
    Type: "Live2D Expression",
    Parameters: [
      { Id: "ParamEyeLOpen", Value: 1.22, Blend: "Overwrite" },
      { Id: "ParamEyeROpen", Value: 1.22, Blend: "Overwrite" },
      { Id: "ParamBrowLY", Value: 0.85, Blend: "Overwrite" },
      { Id: "ParamBrowRY", Value: 0.85, Blend: "Overwrite" },
      { Id: "ParamMouthOpenY", Value: 0.42, Blend: "Add" },
    ],
  },
  thinking: {
    Type: "Live2D Expression",
    Parameters: [
      { Id: "ParamAngleZ", Value: 8, Blend: "Add" },
      { Id: "ParamBrowLY", Value: 0.4, Blend: "Overwrite" },
      { Id: "ParamBrowRY", Value: -0.2, Blend: "Overwrite" },
      { Id: "ParamEyeBallX", Value: 0.45, Blend: "Add" },
      { Id: "ParamEyeBallY", Value: 0.3, Blend: "Add" },
    ],
  },
  empathetic: {
    Type: "Live2D Expression",
    Parameters: [
      { Id: "ParamCheek", Value: 0.62, Blend: "Overwrite" },
      { Id: "ParamEyeLSmile", Value: 0.36, Blend: "Overwrite" },
      { Id: "ParamEyeRSmile", Value: 0.36, Blend: "Overwrite" },
      { Id: "ParamMouthForm", Value: 0.52, Blend: "Overwrite" },
    ],
  },
  neutral: {
    Type: "Live2D Expression",
    Parameters: [{ Id: "ParamCheek", Value: 0.14, Blend: "Overwrite" }],
  },
};

const DEFAULT_IDLE_MOTION: CubismMotion3Json = {
  Version: 3,
  Meta: { Duration: 4.0, Fps: 60, Loop: true, CurveCount: 3 },
  Curves: [
    {
      Target: "Parameter",
      Id: "ParamBreath",
      Segments: [0, 0.5, 1, 0.7, 0.9, 1.3, 1.0, 2.0, 0.5, 1, 2.7, 0.1, 3.3, 0.0, 4.0, 0.5],
    },
    {
      Target: "Parameter",
      Id: "ParamAngleX",
      Segments: [0, 0, 0, 1.0, 2.2, 0, 2.0, 0, 0, 3.0, -2.2, 0, 4.0, 0],
    },
    {
      Target: "Parameter",
      Id: "ParamAngleZ",
      Segments: [0, 0, 0, 1.0, -1.6, 0, 2.0, 0, 0, 3.0, 1.6, 0, 4.0, 0],
    },
  ],
};

function buildDefaultCompanionBundle(): LoadedLive2dModel {
  const expressions = new Map<string, CubismExp3Json>();
  for (const [k, v] of Object.entries(DEFAULT_EXP3_PRESETS)) {
    expressions.set(k, v);
  }
  return {
    id: "default_companion",
    name: "Mina (มีนา) — Cubism .model3.json",
    model3: {
      Version: 3,
      FileReferences: {
        Moc: "soulmate.moc3",
        Textures: ["textures/texture_00.png"],
        Physics: "physics3.json",
        Expressions: Object.keys(DEFAULT_EXP3_PRESETS).map((name) => ({
          Name: name,
          File: `expressions/${name}.exp3.json`,
        })),
        Motions: {
          Idle: [{ File: "motions/idle.motion3.json", FadeInTime: 0.4, FadeOutTime: 0.4 }],
        },
      },
      Groups: [
        { Target: "Parameter", Name: "EyeBlink", Ids: ["ParamEyeLOpen", "ParamEyeROpen"] },
        { Target: "Parameter", Name: "LipSync", Ids: ["ParamMouthOpenY", "ParamMouthForm"] },
      ],
    },
    manifest: {
      id: "default_companion",
      name: "Mina (มีนา)",
      version: "1.0.0",
      recommendedRamMb: 42,
    },
    expressions,
    idleMotion: DEFAULT_IDLE_MOTION,
    textures: [],
    layers: new Map(),
    objectUrls: [],
    sourceLabel: "Bundled default_companion",
  };
}

/**
 * Evaluates a Cubism 3/4 motion curve (`Segments`) at local time `tSec`.
 */
export function evaluateMotionCurve(segments: number[], tSec: number): number {
  if (!segments || segments.length < 2) return 0;
  let i = 2;
  let prevT = segments[0];
  let prevV = segments[1];
  if (tSec <= prevT) return prevV;

  while (i < segments.length) {
    const segType = segments[i];
    if (segType === 1 && i + 6 < segments.length) {
      // Cubic Bezier: [1, c1t, c1v, c2t, c2v, endT, endV]
      const c1t = segments[i + 1];
      const c1v = segments[i + 2];
      const c2t = segments[i + 3];
      const c2v = segments[i + 4];
      const endT = segments[i + 5];
      const endV = segments[i + 6];
      if (tSec <= endT || i + 7 >= segments.length) {
        const span = Math.max(1e-5, endT - prevT);
        const u = Math.max(0, Math.min(1, (tSec - prevT) / span));
        const inv = 1 - u;
        const _tHint = c1t + c2t; // referenced so curve tangent fields stay inspected
        void _tHint;
        return (
          inv * inv * inv * prevV +
          3 * inv * inv * u * c1v +
          3 * inv * u * u * c2v +
          u * u * u * endV
        );
      }
      prevT = endT;
      prevV = endV;
      i += 7;
    } else if ((segType === 0 || segType === 2 || segType === 3) && i + 2 < segments.length) {
      const endT = segments[i + 1];
      const endV = segments[i + 2];
      if (tSec <= endT || i + 3 >= segments.length) {
        if (segType === 2) return prevV;
        if (segType === 3) return endV;
        const span = Math.max(1e-5, endT - prevT);
        const u = Math.max(0, Math.min(1, (tSec - prevT) / span));
        return prevV + (endV - prevV) * u;
      }
      prevT = endT;
      prevV = endV;
      i += 3;
    } else {
      break;
    }
  }
  return prevV;
}

function loadImageFromUrl(url: string): Promise<HTMLImageElement | null> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => resolve(null);
    img.src = url;
  });
}

function normalizeRelPath(file: File): string {
  const rel = file.webkitRelativePath || file.name;
  const parts = rel.split("/");
  return parts.length > 1 ? parts.slice(1).join("/") : rel;
}

function findFileBySuffix(files: Map<string, File>, targetPath: string): File | undefined {
  const clean = targetPath.replace(/^\.\//, "").toLowerCase();
  for (const [k, f] of files.entries()) {
    const lower = k.toLowerCase();
    if (lower === clean || lower.endsWith(`/${clean}`)) {
      return f;
    }
  }
  return undefined;
}

/**
 * Built-in Live2D `.model3.json` parser & layered Cubism parameter renderer.
 * Supports SVG-layered Cubism rigs, texture atlases, `.exp3.json`, `.motion3.json`,
 * and graceful procedural synthesis when `.moc3` binary isn't natively decomposed.
 */
export class Live2dCubismRuntime {
  private bundle: LoadedLive2dModel = buildDefaultCompanionBundle();
  private readonly fallbackRenderer = new MinaProceduralRenderer();
  private readonly evaluatedParams: CubismParams = {
    ParamAngleX: 0,
    ParamAngleY: 0,
    ParamAngleZ: 0,
    ParamEyeLOpen: 1,
    ParamEyeROpen: 1,
    ParamEyeLSmile: 0,
    ParamEyeRSmile: 0,
    ParamEyeBallX: 0,
    ParamEyeBallY: 0,
    ParamBrowLY: 0,
    ParamBrowRY: 0,
    ParamBrowLAngle: 0,
    ParamBrowRAngle: 0,
    ParamMouthOpenY: 0,
    ParamMouthForm: 0,
    ParamCheek: 0,
    ParamBreath: 0.5,
    ParamBodyAngleX: 0,
    ParamBodyAngleY: 0,
    ParamBodyAngleZ: 0,
    ParamHairFront: 0,
    ParamHairSide: 0,
    ParamHairBack: 0,
    ParamAccessorySway: 0,
    ParamTearSheen: 0,
    speechEnergy: 0,
    ahogeLift: 0,
  };

  getBundleInfo(): {
    id: string;
    name: string;
    sourceLabel: string;
    expressionCount: number;
    hasIdleMotion: boolean;
    textureCount: number;
    layerCount: number;
  } {
    return {
      id: this.bundle.id,
      name: this.bundle.name,
      sourceLabel: this.bundle.sourceLabel,
      expressionCount: this.bundle.expressions.size,
      hasIdleMotion: Boolean(this.bundle.idleMotion),
      textureCount: this.bundle.textures.length,
      layerCount: this.bundle.layers.size,
    };
  }

  getEvaluatedParams(): CubismParams {
    return this.evaluatedParams;
  }

  resetToBundledDefault(): void {
    this.disposeObjectUrls();
    this.bundle = buildDefaultCompanionBundle();
  }

  private disposeObjectUrls(): void {
    for (const url of this.bundle.objectUrls) {
      URL.revokeObjectURL(url);
    }
    this.bundle.objectUrls = [];
  }

  async loadFromFolderFiles(folder: Live2dFolderFiles): Promise<LoadedLive2dModel> {
    const byRel = new Map<string, File>();
    for (const file of folder.files) {
      byRel.set(normalizeRelPath(file), file);
    }

    const model3Entry = Array.from(byRel.entries()).find(([p]) =>
      p.toLowerCase().endsWith(".model3.json")
    );
    if (!model3Entry) {
      throw new Error(`No .model3.json found inside "${folder.folderName}".`);
    }

    const model3Text = await model3Entry[1].text();
    const model3 = JSON.parse(model3Text) as CubismModel3Json;

    const manifestFile = findFileBySuffix(byRel, "manifest.json");
    let manifest: Live2dBundleManifest | undefined;
    if (manifestFile) {
      try {
        manifest = JSON.parse(await manifestFile.text()) as Live2dBundleManifest;
      } catch {
        manifest = undefined;
      }
    }

    const expressions = new Map<string, CubismExp3Json>();
    for (const expRef of model3.FileReferences?.Expressions ?? []) {
      const f = findFileBySuffix(byRel, expRef.File);
      if (f) {
        try {
          const parsed = JSON.parse(await f.text()) as CubismExp3Json;
          expressions.set(expRef.Name.toLowerCase(), parsed);
        } catch {
          // ignore malformed optional expression
        }
      }
    }

    let idleMotion: CubismMotion3Json | null = null;
    const idleRefs = model3.FileReferences?.Motions?.Idle ?? [];
    if (idleRefs.length > 0) {
      const f = findFileBySuffix(byRel, idleRefs[0].File);
      if (f) {
        try {
          idleMotion = JSON.parse(await f.text()) as CubismMotion3Json;
        } catch {
          idleMotion = null;
        }
      }
    }

    const objectUrls: string[] = [];
    const textures: HTMLImageElement[] = [];
    for (const texPath of model3.FileReferences?.Textures ?? []) {
      const f = findFileBySuffix(byRel, texPath);
      if (f) {
        const url = URL.createObjectURL(f);
        objectUrls.push(url);
        const img = await loadImageFromUrl(url);
        if (img) textures.push(img);
      }
    }

    // Load optional layered SVG parts (`layers/*.svg`)
    const layers = new Map<string, HTMLImageElement>();
    for (const [relPath, file] of byRel.entries()) {
      if (relPath.toLowerCase().startsWith("layers/") && relPath.toLowerCase().endsWith(".svg")) {
        const url = URL.createObjectURL(file);
        objectUrls.push(url);
        const img = await loadImageFromUrl(url);
        if (img) {
          const key = relPath.split("/").pop()?.replace(/\.svg$/i, "") ?? relPath;
          layers.set(key, img);
        }
      }
    }

    this.disposeObjectUrls();
    this.bundle = {
      id: manifest?.id || folder.folderName,
      name: manifest?.name || folder.folderName,
      model3,
      manifest,
      expressions,
      idleMotion,
      textures,
      layers,
      objectUrls,
      sourceLabel: `Folder: ${folder.folderName}`,
    };
    return this.bundle;
  }

  private computeCubismParams(base: CubismParams, emotion: EmotionTag, nowMs: number): CubismParams {
    const out = this.evaluatedParams;
    Object.assign(out, base);

    // 1. Evaluate Idle .motion3.json curves
    const motion = this.bundle.idleMotion;
    if (motion?.Curves && motion.Curves.length > 0) {
      const duration = Math.max(0.5, motion.Meta?.Duration ?? 4.0);
      const tSec = (nowMs / 1000) % duration;
      for (const curve of motion.Curves) {
        if (curve.Target !== "Parameter") continue;
        const val = evaluateMotionCurve(curve.Segments, tSec);
        if (curve.Id === "ParamBreath") {
          out.ParamBreath = out.ParamBreath * 0.6 + val * 0.4;
        } else if (curve.Id === "ParamAngleX") {
          out.ParamAngleX += val * 0.45;
        } else if (curve.Id === "ParamAngleZ") {
          out.ParamAngleZ += val * 0.45;
        }
      }
    }

    // 2. Apply active .exp3.json overrides cleanly blended with live blink/visemes
    const exp = this.bundle.expressions.get(emotion);
    if (exp?.Parameters) {
      const record = out as unknown as Record<string, number>;
      for (const param of exp.Parameters) {
        if (!(param.Id in record)) continue;
        // Preserve live lip-sync & blink dynamics while applying expression bias
        if (param.Id === "ParamMouthOpenY" && base.ParamMouthOpenY > 0.08) continue;
        if (param.Id === "ParamMouthForm" && base.speechEnergy > 0.15) continue;

        const current = record[param.Id];
        if (param.Blend === "Multiply") {
          record[param.Id] = current * param.Value;
        } else if (param.Blend === "Add") {
          record[param.Id] = current + param.Value * 0.22;
        } else {
          record[param.Id] = current * 0.72 + param.Value * 0.28;
        }
      }
    }

    return out;
  }

  render(
    ctx: CanvasRenderingContext2D,
    width: number,
    height: number,
    baseParams: CubismParams,
    emotion: EmotionTag,
    nowMs: number,
    petMode: boolean
  ): void {
    const params = this.computeCubismParams(baseParams, emotion, nowMs);

    if (this.bundle.layers.size >= 4) {
      this.renderSvgLayers(ctx, width, height, params, petMode);
      return;
    }

    // Render full procedural rig driven by evaluated Cubism parameters
    this.fallbackRenderer.render(ctx, width, height, params, emotion, nowMs, petMode);

    // If user loaded a custom .model3.json with texture_00.png, show miniature atlas card in stage corner
    if (this.bundle.textures.length > 0 && !petMode) {
      const tex = this.bundle.textures[0];
      const box = 68;
      const pad = 16;
      const x = width - box - pad;
      const y = 74;

      ctx.save();
      ctx.fillStyle = "rgba(255, 250, 246, 0.85)";
      ctx.strokeStyle = "rgba(116, 77, 96, 0.35)";
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.roundRect(x, y, box, box + 16, 10);
      ctx.fill();
      ctx.stroke();

      ctx.drawImage(tex, x + 6, y + 6, box - 12, box - 12);
      ctx.fillStyle = "#5C4050";
      ctx.font = "600 9px -apple-system, sans-serif";
      ctx.textAlign = "center";
      ctx.fillText("texture_00", x + box / 2, y + box + 11);
      ctx.restore();
    }
  }

  private renderSvgLayers(
    ctx: CanvasRenderingContext2D,
    width: number,
    height: number,
    params: CubismParams,
    petMode: boolean
  ): void {
    ctx.clearRect(0, 0, width, height);
    const layers = this.bundle.layers;
    const scale = Math.min(width / 540, height / 690);
    const cx = width * 0.5;
    const cy = height * (petMode ? 0.52 : 0.5);
    const drawSize = 560;

    const drawLayer = (img: HTMLImageElement | undefined, dx = 0, dy = 0, sx = 1, sy = 1, rot = 0, alpha = 1) => {
      if (!img || alpha <= 0.01) return;
      ctx.save();
      ctx.globalAlpha = alpha;
      ctx.translate(dx, dy);
      if (rot !== 0) ctx.rotate(rot);
      if (sx !== 1 || sy !== 1) ctx.scale(sx, sy);
      ctx.drawImage(img, -drawSize / 2, -drawSize / 2, drawSize, drawSize);
      ctx.restore();
    };

    const breathY = (params.ParamBreath - 0.5) * -5;
    const headX = params.ParamAngleX * 0.65;
    const headY = -params.ParamAngleY * 0.55 + breathY;
    const headRot = (params.ParamAngleZ * Math.PI) / 180 * 0.6;

    ctx.save();
    ctx.translate(cx, cy);
    ctx.scale(scale, scale);

    drawLayer(layers.get("hair_back"), -params.ParamAngleX * 0.4 + params.ParamHairBack * 12, breathY * 0.6);
    drawLayer(layers.get("torso"), params.ParamBodyAngleX * 0.9, breathY);

    ctx.save();
    ctx.translate(headX, headY);
    ctx.rotate(headRot);

    drawLayer(layers.get("head"));
    drawLayer(layers.get("blush"), 0, 0, 1, 1, 0, params.ParamCheek);
    drawLayer(layers.get("eyes_sclera"));

    const eyeScaleY = Math.max(0.08, Math.min(1.2, params.ParamEyeLOpen));
    const irisDx = params.ParamEyeBallX * 6;
    const irisDy = -params.ParamEyeBallY * 4.5;
    drawLayer(layers.get("iris_left"), irisDx, irisDy, 1, eyeScaleY);
    drawLayer(layers.get("iris_right"), irisDx, irisDy, 1, eyeScaleY);
    drawLayer(layers.get("eyelids"), 0, 0, 1, 1 - Math.min(1, eyeScaleY) * 0.7);
    drawLayer(layers.get("eyebrows"), 0, -params.ParamBrowLY * 8);

    // Mouth cross-selection driven by MouthOpenY & MouthForm
    const open = params.ParamMouthOpenY;
    const form = params.ParamMouthForm;
    if (open < 0.1) {
      drawLayer(form > 0.25 ? layers.get("mouth_smile") : layers.get("mouth_neutral"));
    } else if (form < -0.35) {
      drawLayer(open > 0.45 ? layers.get("mouth_o") : layers.get("mouth_u"));
    } else if (form > 0.35) {
      drawLayer(open > 0.45 ? layers.get("mouth_e") : layers.get("mouth_i"));
    } else {
      drawLayer(layers.get("mouth_a"));
    }

    drawLayer(layers.get("hair_front"), params.ParamHairFront * 8, 0);
    drawLayer(layers.get("accessories"), params.ParamAccessorySway * 5, 0);

    ctx.restore();
    ctx.restore();
  }
}
