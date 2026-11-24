import type {
  AvatarLogicalState,
  AvatarSnapshot,
  CharacterId,
  EmotionState,
  ModelInfo,
  VisemeFrame,
} from "../types/ipc";
import { isTauriRuntime, type Live2dFolderFiles } from "../api/tauriClient";
import { CompanionProceduralRenderer } from "./companionRenderer";
import { Live2dCubismRuntime } from "./live2dRuntime";
import { AvatarMotionController } from "./motionController";
import type { AvatarEngineMode, CubismParams } from "./types";

export class AvatarStage {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly motion = new AvatarMotionController();
  private readonly companion = new CompanionProceduralRenderer();
  private readonly live2d = new Live2dCubismRuntime();

  private character: CharacterId = "coral";
  private engineMode: AvatarEngineMode = "vector";
  private petMode = false;
  private rafId: number | null = null;
  private lastFrameMs = performance.now();
  private logicalW = 440;
  private logicalH = 560;
  private resizeObserver: ResizeObserver | null = null;

  constructor(canvas: HTMLCanvasElement, initialCharacter: CharacterId = "coral") {
    this.canvas = canvas;
    this.character = initialCharacter;
    this.companion.setCharacter(initialCharacter);
    this.live2d.setCharacter(initialCharacter);

    const context = canvas.getContext("2d", { alpha: true });
    if (!context) {
      throw new Error("2D Canvas context unavailable");
    }
    this.ctx = context;

    this.resizeToContainer();
    this.setupListeners();
    this.startLoop();
  }

  private setupListeners(): void {
    const parent = this.canvas.parentElement;
    if (parent && typeof ResizeObserver !== "undefined") {
      this.resizeObserver = new ResizeObserver(() => this.resizeToContainer());
      this.resizeObserver.observe(parent);
    }

    window.addEventListener("pointermove", this.handlePointerMove, { passive: true });
  }

  private handlePointerMove = (evt: PointerEvent): void => {
    const rect = this.canvas.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;

    const headCenterX = rect.left + rect.width * 0.5;
    const headCenterY = rect.top + rect.height * 0.38;
    const normX = (evt.clientX - headCenterX) / Math.max(rect.width * 0.5, 160);
    const normY = (headCenterY - evt.clientY) / Math.max(rect.height * 0.5, 180);

    this.motion.setPointerLook(normX, normY, performance.now());
  };

  resizeToContainer(): void {
    const parent = this.canvas.parentElement;
    const width = Math.max(240, parent?.clientWidth || window.innerWidth || 440);
    const height = Math.max(260, parent?.clientHeight || window.innerHeight || 540);
    const dpr = Math.min(window.devicePixelRatio || 1, 2);

    this.logicalW = width;
    this.logicalH = height;

    const targetW = Math.round(width * dpr);
    const targetH = Math.round(height * dpr);
    if (this.canvas.width !== targetW || this.canvas.height !== targetH) {
      this.canvas.width = targetW;
      this.canvas.height = targetH;
      this.canvas.style.width = `${width}px`;
      this.canvas.style.height = `${height}px`;
    }
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  private startLoop(): void {
    if (this.rafId !== null) return;
    this.lastFrameMs = performance.now();

    const tick = (nowMs: number) => {
      const dtMs = nowMs - this.lastFrameMs;
      this.lastFrameMs = nowMs;

      const params = this.motion.update(dtMs, nowMs);
      const { tag } = this.motion.getEmotion();
      const logicalState = this.motion.getLogicalState();

      if (this.engineMode === "live2d") {
        this.live2d.render(
          this.ctx,
          this.logicalW,
          this.logicalH,
          params,
          tag,
          nowMs,
          this.petMode,
          logicalState
        );
      } else {
        this.companion.render(
          this.ctx,
          this.logicalW,
          this.logicalH,
          params,
          tag,
          nowMs,
          this.petMode,
          this.character,
          logicalState
        );
      }

      this.rafId = window.requestAnimationFrame(tick);
    };

    this.rafId = window.requestAnimationFrame(tick);
  }

  setCharacter(character: CharacterId): void {
    this.character = character;
    this.companion.setCharacter(character);
    this.live2d.setCharacter(character);

    const activeBundleId = this.live2d.getBundleInfo().id;
    if (
      activeBundleId === "coral_companion" ||
      activeBundleId === "kai_companion" ||
      activeBundleId === "default_companion"
    ) {
      void this.loadCompanionLive2dAsset(character, this.engineMode === "live2d");
    }
  }

  getCharacter(): CharacterId {
    return this.character;
  }

  setEmotion(state: EmotionState): void {
    this.motion.setEmotion(state);
  }

  applyAvatarSnapshot(snapshot: AvatarSnapshot): void {
    if (snapshot.characterId && snapshot.characterId !== this.character) {
      this.setCharacter(snapshot.characterId);
    }
    this.motion.applyAvatarSnapshot(snapshot);
  }

  getLogicalState(): AvatarLogicalState {
    return this.motion.getLogicalState();
  }

  isOcularMotionPaused(): boolean {
    return this.motion.isOcularMotionPaused();
  }

  pushViseme(frame: VisemeFrame): void {
    this.motion.pushViseme(frame, performance.now());
  }

  clearViseme(): void {
    this.motion.clearViseme();
  }

  setEngineMode(mode: AvatarEngineMode): void {
    this.engineMode = mode;
  }

  getEngineMode(): AvatarEngineMode {
    return this.engineMode;
  }

  setPetMode(enabled: boolean): void {
    this.petMode = enabled;
  }

  async loadLive2dFiles(folder: Live2dFolderFiles): Promise<{
    id: string;
    name: string;
    expressionCount: number;
  }> {
    const loaded = await this.live2d.loadFromFolderFiles(folder);
    this.engineMode = "live2d";
    return {
      id: loaded.id,
      name: loaded.name,
      expressionCount: loaded.expressions.size,
    };
  }

  async loadCompanionLive2dAsset(
    character: CharacterId = this.character,
    switchToLive2d = false
  ): Promise<{
    id: string;
    name: string;
    expressionCount: number;
  }> {
    const bundleId = character === "kai" ? "kai_companion" : "coral_companion";
    const displayName = character === "kai" ? "Kai · ไค" : "Coral · คอรัล";

    try {
      const loaded = await this.live2d.loadFromBase(
        `/live2d/${bundleId}`,
        "soulmate.model3.json",
        {
          id: bundleId,
          name: displayName,
          sourceLabel: `Bundled ${bundleId}`,
          useTauriAsset: false,
        }
      );
      if (switchToLive2d) {
        this.engineMode = "live2d";
      }
      return {
        id: loaded.id,
        name: loaded.name,
        expressionCount: loaded.expressions.size,
      };
    } catch {
      this.live2d.resetToBundledDefault(character);
      if (switchToLive2d) {
        this.engineMode = "live2d";
      }
      const info = this.live2d.getBundleInfo();
      return {
        id: info.id,
        name: info.name,
        expressionCount: info.expressionCount,
      };
    }
  }

  async loadDefaultLive2dAsset(switchToLive2d = false): Promise<{
    id: string;
    name: string;
    expressionCount: number;
  }> {
    return this.loadCompanionLive2dAsset(this.character, switchToLive2d);
  }

  async loadLive2dModelInfo(
    model: ModelInfo,
    switchToLive2d = true
  ): Promise<{
    id: string;
    name: string;
    expressionCount: number;
  }> {
    const rawPath = model.path.trim();
    if (model.id === "kai_companion") {
      this.character = "kai";
      this.companion.setCharacter("kai");
      this.live2d.setCharacter("kai");
    } else if (model.id === "coral_companion") {
      this.character = "coral";
      this.companion.setCharacter("coral");
      this.live2d.setCharacter("coral");
    }

    if (isTauriRuntime() && !rawPath.startsWith("/bundled/")) {
      const lower = rawPath.toLowerCase();
      let baseDir = rawPath;
      let model3Name = "soulmate.model3.json";
      if (lower.endsWith(".model3.json")) {
        const idx = rawPath.lastIndexOf("/");
        if (idx > 0) {
          baseDir = rawPath.slice(0, idx);
          model3Name = rawPath.slice(idx + 1);
        }
      }
      try {
        const loaded = await this.live2d.loadFromBase(baseDir, model3Name, {
          id: model.id,
          name: model.name,
          sourceLabel:
            model.source === "bundled"
              ? "Bundled Live2D"
              : `Folder: ${baseDir.split("/").pop() || model.name}`,
          useTauriAsset: true,
        });
        if (switchToLive2d) {
          this.engineMode = "live2d";
        }
        return {
          id: loaded.id,
          name: loaded.name,
          expressionCount: loaded.expressions.size,
        };
      } catch (err) {
        if (
          model.id === "coral_companion" ||
          model.id === "kai_companion" ||
          model.id === "default_companion"
        ) {
          const targetChar: CharacterId =
            model.id === "kai_companion" ? "kai" : "coral";
          return this.loadCompanionLive2dAsset(targetChar, switchToLive2d);
        }
        throw err;
      }
    }

    if (model.id === "kai_companion" || rawPath.includes("kai_companion")) {
      return this.loadCompanionLive2dAsset("kai", switchToLive2d);
    }

    if (
      model.id === "coral_companion" ||
      rawPath.includes("coral_companion") ||
      model.id === "default_companion" ||
      rawPath.includes("default_companion")
    ) {
      return this.loadCompanionLive2dAsset("coral", switchToLive2d);
    }

    if (switchToLive2d) {
      this.engineMode = "live2d";
    }
    const info = this.live2d.getBundleInfo();
    return {
      id: model.id,
      name: model.name,
      expressionCount: info.expressionCount,
    };
  }

  resetLive2dDefault(): void {
    void this.loadCompanionLive2dAsset(this.character, this.engineMode === "live2d");
  }

  getLive2dInfo() {
    return this.live2d.getBundleInfo();
  }

  getCubismParams(): CubismParams {
    return this.engineMode === "live2d"
      ? this.live2d.getEvaluatedParams()
      : this.motion.params;
  }

  destroy(): void {
    if (this.rafId !== null) {
      window.cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
    this.resizeObserver?.disconnect();
    window.removeEventListener("pointermove", this.handlePointerMove);
  }
}
