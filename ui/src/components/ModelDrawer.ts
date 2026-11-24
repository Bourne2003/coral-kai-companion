import {
  formatSoulError,
  invokeCommand,
  pickAndRegisterModel,
  pickLive2dFolder,
  type Live2dFolderFiles,
} from "../api/tauriClient";
import type { AvatarEngineMode } from "../avatar/types";
import type { ModelInfo, RamSafetyTier, SystemRamStatus } from "../types/ipc";
import { ICONS } from "./icons";

export interface ModelDrawerCallbacks {
  onClose: () => void;
  onRamUpdated: (ram: SystemRamStatus) => void;
  onEngineModeChange: (mode: AvatarEngineMode) => void;
  onLive2dFilesLoaded: (folder: Live2dFolderFiles) => Promise<void>;
  onLive2dModelSelected: (model: ModelInfo) => Promise<void>;
  onLive2dDefaultSelected: () => void;
  getEngineMode: () => AvatarEngineMode;
  getLive2dSummary: () => {
    id: string;
    name: string;
    sourceLabel: string;
    expressionCount: number;
    hasIdleMotion: boolean;
    textureCount: number;
    layerCount: number;
  };
  onToast: (message: string, kind?: "info" | "error") => void;
}

function safetyBadgeMeta(tier: RamSafetyTier): { label: string; cls: string } {
  switch (tier) {
    case "safe8Gb":
      return { label: "8GB M1 Safe", cls: "badge-safe" };
    case "tight8Gb":
      return { label: "Tight on 8GB", cls: "badge-tight" };
    case "recommended16GbPlus":
      return { label: "16GB+ Upgrade Recommended", cls: "badge-heavy" };
  }
}

function formatSizeGb(bytes: number): string {
  if (bytes <= 0) return "—";
  if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(2)} GB`;
  return `${Math.round(bytes / 1_000_000)} MB`;
}

export class ModelDrawer {
  readonly root: HTMLElement;
  private readonly bodyEl: HTMLElement;
  private readonly callbacks: ModelDrawerCallbacks;

  private ram: SystemRamStatus | null = null;
  private models: ModelInfo[] = [];
  private busyModelId: string | null = null;
  private errorBanner: string | null = null;

  constructor(callbacks: ModelDrawerCallbacks) {
    this.callbacks = callbacks;

    this.root = document.createElement("aside");
    this.root.className = "drawer hidden";
    this.root.setAttribute("aria-label", "Models and Memory Settings");

    const header = document.createElement("div");
    header.className = "drawer-header";

    const titleWrap = document.createElement("div");
    titleWrap.className = "drawer-title-wrap";
    const icon = document.createElement("span");
    icon.className = "drawer-title-icon";
    icon.innerHTML = ICONS.chip;
    const h2 = document.createElement("h2");
    h2.textContent = "Models & Avatar Runtime";
    titleWrap.append(icon, h2);

    const closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "icon-btn";
    closeBtn.title = "Close Models Drawer";
    closeBtn.innerHTML = ICONS.close;
    closeBtn.addEventListener("click", () => this.callbacks.onClose());

    header.append(titleWrap, closeBtn);

    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "drawer-body";

    this.root.append(header, this.bodyEl);
  }

  open(): void {
    this.root.classList.remove("hidden");
    void this.refresh();
  }

  close(): void {
    this.root.classList.add("hidden");
  }

  isOpen(): boolean {
    return !this.root.classList.contains("hidden");
  }

  updateRam(ram: SystemRamStatus): void {
    this.ram = ram;
    if (this.isOpen()) this.render();
  }

  async refresh(): Promise<void> {
    try {
      const [ram, models] = await Promise.all([
        invokeCommand("get_ram_status"),
        invokeCommand("list_models", { kind: null }),
      ]);
      this.ram = ram;
      this.models = models;
      this.callbacks.onRamUpdated(ram);
      this.render();
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.render();
    }
  }

  private async handleSwapLlm(modelId: string): Promise<void> {
    this.busyModelId = modelId;
    this.errorBanner = null;
    this.render();
    try {
      const active = await invokeCommand("hot_swap_llm", { modelId });
      await this.refresh();
      this.callbacks.onToast(`Switched LLM to ${active.name}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.busyModelId = null;
      this.render();
    }
  }

  private async handleSwitchAsr(modelId: string): Promise<void> {
    this.busyModelId = modelId;
    this.errorBanner = null;
    this.render();
    try {
      const active = await invokeCommand("switch_asr_model", { modelId });
      await this.refresh();
      this.callbacks.onToast(`Switched Whisper ASR to ${active.name}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.busyModelId = null;
      this.render();
    }
  }

  private async handleAddCustomModel(kind: "llm" | "asr"): Promise<void> {
    try {
      const registered = await pickAndRegisterModel(kind);
      if (!registered) return;
      await this.refresh();
      this.callbacks.onToast(`Registered ${registered.name}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.render();
    }
  }

  private async handlePickLive2dFolder(): Promise<void> {
    try {
      const { model, localFiles } = await pickLive2dFolder();
      if (!model && !localFiles) return;
      if (localFiles) {
        await this.callbacks.onLive2dFilesLoaded(localFiles);
      } else if (model) {
        await this.callbacks.onLive2dModelSelected(model);
      } else {
        this.callbacks.onEngineModeChange("live2d");
      }
      await this.refresh();
      this.callbacks.onToast(`Loaded Live2D avatar: ${model?.name ?? localFiles?.folderName}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
      this.render();
    }
  }

  private renderRamCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card ram-card";

    const ram = this.ram ?? {
      totalRamMb: 8192,
      availableRamMb: 2800,
      processRamMb: 3450,
      budgetCeilingMb: 4800,
      is8gbClass: true,
      activeLlmId: null,
      activeAsrId: null,
    };

    const top = document.createElement("div");
    top.className = "ram-header";

    const label = document.createElement("div");
    label.className = "ram-title";
    label.textContent = "Unified Memory Budget";

    const hwPill = document.createElement("span");
    hwPill.className = `pill ${ram.is8gbClass ? "pill-warn" : "pill-ok"}`;
    hwPill.textContent = ram.is8gbClass ? "8GB M1 Class" : "16GB+ Class";
    top.append(label, hwPill);

    const pctBudget = Math.min(100, Math.round((ram.processRamMb / Math.max(1, ram.budgetCeilingMb)) * 100));
    const pctTotal = Math.min(100, Math.round((ram.processRamMb / Math.max(1, ram.totalRamMb)) * 100));

    const barOuter = document.createElement("div");
    barOuter.className = "ram-bar-track";
    const barFill = document.createElement("div");
    barFill.className = `ram-bar-fill ${
      pctBudget > 92 ? "fill-danger" : pctBudget > 75 ? "fill-warn" : "fill-safe"
    }`;
    barFill.style.width = `${pctBudget}%`;
    barOuter.appendChild(barFill);

    const metrics = document.createElement("div");
    metrics.className = "ram-metrics";
    metrics.innerHTML = "";
    const m1 = document.createElement("span");
    m1.textContent = `Process: ${ram.processRamMb.toLocaleString()} MB (${pctTotal}% of Mac)`;
    const m2 = document.createElement("span");
    m2.textContent = `Budget: ${ram.budgetCeilingMb.toLocaleString()} / ${ram.totalRamMb.toLocaleString()} MB`;
    const m3 = document.createElement("span");
    m3.textContent = `System Free: ${ram.availableRamMb.toLocaleString()} MB`;
    metrics.append(m1, m2, m3);

    card.append(top, barOuter, metrics);
    return card;
  }

  private renderModelItem(model: ModelInfo): HTMLElement {
    const item = document.createElement("div");
    item.className = `model-item ${model.isActive ? "active" : ""}`;

    const head = document.createElement("div");
    head.className = "model-item-head";

    const name = document.createElement("div");
    name.className = "model-name";
    name.textContent = model.name;

    const badgeMeta = safetyBadgeMeta(model.safetyTier);
    const badge = document.createElement("span");
    badge.className = `safety-badge ${badgeMeta.cls}`;
    badge.textContent = badgeMeta.label;
    head.append(name, badge);

    const metaRow = document.createElement("div");
    metaRow.className = "model-meta";
    const ramText = `~${model.estimatedRamMb.toLocaleString()} MB RAM`;
    const sizeText = formatSizeGb(model.sizeBytes);
    const srcText = model.source;
    metaRow.textContent = `${ramText} · ${sizeText} · ${srcText}`;

    item.append(head, metaRow);

    if (model.notes) {
      const notes = document.createElement("div");
      notes.className = "model-notes";
      notes.textContent = model.notes;
      item.appendChild(notes);
    }

    const actions = document.createElement("div");
    actions.className = "model-actions";

    if (model.kind === "llm" || model.kind === "asr") {
      const btn = document.createElement("button");
      btn.type = "button";
      const isBusy = this.busyModelId === model.id;
      btn.className = model.isActive ? "btn-chip active-chip" : "btn-primary btn-sm";
      btn.disabled = model.isActive || isBusy || this.busyModelId !== null;
      btn.textContent = model.isActive
        ? "Active"
        : isBusy
        ? "Swapping..."
        : model.kind === "llm"
        ? "Hot-Swap LLM"
        : "Use ASR";
      btn.addEventListener("click", () => {
        if (model.kind === "llm") void this.handleSwapLlm(model.id);
        else void this.handleSwitchAsr(model.id);
      });
      actions.appendChild(btn);
    } else if (model.kind === "live2d") {
      const btn = document.createElement("button");
      btn.type = "button";
      const activeMode = this.callbacks.getEngineMode();
      const isUsingThis = model.isActive && activeMode === "live2d";
      btn.className = isUsingThis ? "btn-chip active-chip" : "btn-primary btn-sm";
      btn.textContent = isUsingThis ? "Active Cubism Rig" : "Activate Cubism";
      btn.addEventListener("click", () => {
        void this.callbacks
          .onLive2dModelSelected(model)
          .then(() => this.refresh())
          .catch((err) => {
            this.errorBanner = formatSoulError(err);
            this.callbacks.onToast(this.errorBanner, "error");
            this.render();
          });
      });
      actions.appendChild(btn);
    }

    item.appendChild(actions);
    return item;
  }

  private renderSection(
    title: string,
    items: ModelInfo[],
    addAction?: { label: string; onClick: () => void }
  ): HTMLElement {
    const section = document.createElement("section");
    section.className = "drawer-section";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = title;
    head.appendChild(h3);

    if (addAction) {
      const addBtn = document.createElement("button");
      addBtn.type = "button";
      addBtn.className = "btn-ghost btn-sm";
      addBtn.innerHTML = `${ICONS.folder} <span></span>`;
      const span = addBtn.querySelector("span");
      if (span) span.textContent = addAction.label;
      addBtn.addEventListener("click", addAction.onClick);
      head.appendChild(addBtn);
    }

    section.appendChild(head);

    if (items.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-hint";
      empty.textContent = "No models registered in this category yet.";
      section.appendChild(empty);
    } else {
      for (const m of items) {
        section.appendChild(this.renderModelItem(m));
      }
    }

    return section;
  }

  private renderEngineCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = "2D Character Engine";
    head.appendChild(h3);

    const seg = document.createElement("div");
    seg.className = "segmented";
    const mode = this.callbacks.getEngineMode();

    const btnMina = document.createElement("button");
    btnMina.type = "button";
    btnMina.className = mode === "mina" ? "seg-btn active" : "seg-btn";
    btnMina.textContent = "Mina Canvas";
    btnMina.addEventListener("click", () => {
      this.callbacks.onEngineModeChange("mina");
      this.render();
    });

    const btnLive2d = document.createElement("button");
    btnLive2d.type = "button";
    btnLive2d.className = mode === "live2d" ? "seg-btn active" : "seg-btn";
    btnLive2d.textContent = "Live2D .model3.json";
    btnLive2d.addEventListener("click", () => {
      this.callbacks.onEngineModeChange("live2d");
      this.render();
    });

    seg.append(btnMina, btnLive2d);

    const summary = this.callbacks.getLive2dSummary();
    const info = document.createElement("div");
    info.className = "model-notes";
    info.style.marginTop = "8px";
    info.textContent =
      mode === "mina"
        ? "Default Layered Expressive Character Engine (60 FPS procedural gaze, blink, springs, 7 emotions, visemes)."
        : `Cubism Runtime: ${summary.name} · ${summary.expressionCount} .exp3 presets · ${
            summary.hasIdleMotion ? "idle .motion3 loaded" : "procedural idle"
          } (${summary.sourceLabel})`;

    card.append(head, seg, info);
    return card;
  }

  render(): void {
    this.bodyEl.innerHTML = "";

    if (this.errorBanner) {
      const err = document.createElement("div");
      err.className = "error-banner";
      err.textContent = this.errorBanner;
      this.bodyEl.appendChild(err);
    }

    this.bodyEl.appendChild(this.renderRamCard());
    this.bodyEl.appendChild(this.renderEngineCard());

    const llms = this.models.filter((m) => m.kind === "llm");
    const asrs = this.models.filter((m) => m.kind === "asr");
    const live2ds = this.models.filter((m) => m.kind === "live2d");

    this.bodyEl.appendChild(
      this.renderSection("Local LLM (.gguf)", llms, {
        label: "Add .gguf",
        onClick: () => void this.handleAddCustomModel("llm"),
      })
    );
    this.bodyEl.appendChild(
      this.renderSection("Whisper Speech ASR (.bin)", asrs, {
        label: "Add .bin",
        onClick: () => void this.handleAddCustomModel("asr"),
      })
    );
    this.bodyEl.appendChild(
      this.renderSection("Live2D Avatars (.model3.json)", live2ds, {
        label: "Load Folder",
        onClick: () => void this.handlePickLive2dFolder(),
      })
    );
  }
}
