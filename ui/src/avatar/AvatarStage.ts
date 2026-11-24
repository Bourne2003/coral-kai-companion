import type { EmotionState, VisemeFrame } from "../types/ipc";
import type { Live2dFolderFiles } from "../api/tauriClient";
import { Live2dCubismRuntime } from "./live2dRuntime";
import { MinaProceduralRenderer } from "./minaRenderer";
import { AvatarMotionController } from "./motionController";
import type { AvatarEngineMode, CubismParams } from "./types";

export class AvatarStage {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly motion = new AvatarMotionController();
  private readonly mina = new MinaProceduralRenderer();
  private readonly live2d = new Live2dCubismRuntime();

  private engineMode: AvatarEngineMode = "mina";
  private petMode = false;
  private rafId: number | null = null;
  private lastFrameMs = performance.now();
  private logicalW = 440;
  private logicalH = 560;
  private resizeObserver: ResizeObserver | null = null;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
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

      if (this.engineMode === "live2d") {
        this.live2d.render(
          this.ctx,
          this.logicalW,
          this.logicalH,
          params,
          tag,
          nowMs,
          this.petMode
        );
      } else {
        this.mina.render(
          this.ctx,
          this.logicalW,
          this.logicalH,
          params,
          tag,
          nowMs,
          this.petMode
        );
      }

      this.rafId = window.requestAnimationFrame(tick);
    };

    this.rafId = window.requestAnimationFrame(tick);
  }

  setEmotion(state: EmotionState): void {
    this.motion.setEmotion(state);
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

  resetLive2dDefault(): void {
    this.live2d.resetToBundledDefault();
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
