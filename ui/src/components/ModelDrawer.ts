import {
  formatSoulError,
  invokeCommand,
  isTauriRuntime,
  pickAndRegisterModel,
  pickLive2dFolder,
  pickMcpConfigFile,
  type Live2dFolderFiles,
} from "../api/tauriClient";
import type { AvatarEngineMode } from "../avatar/types";
import type {
  InnerLoopBeatKind,
  InnerLoopConfig,
  InnerLoopStatus,
  McpConfig,
  McpToolInfo,
  ModelInfo,
  ModelKind,
  PluginInfo,
  RamSafetyTier,
  SystemRamStatus,
} from "../types/ipc";
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

type ModelDrawerTab = "models" | "innerLoop";

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

function beatStatusMeta(
  kind: InnerLoopBeatKind | null,
  enabled: boolean
): { label: string; cls: string } {
  if (!enabled || kind === "skippedDisabled" || kind === null) {
    return { label: "Disabled", cls: "pill-muted" };
  }
  switch (kind) {
    case "skippedNoModel":
      return { label: "SkippedNoModel", cls: "pill-warn" };
    case "skippedBusy":
      return { label: "SkippedBusy", cls: "pill-warn" };
    case "skippedLowRam":
      return { label: "SkippedLowRam", cls: "pill-warn" };
    case "memoryReflection":
      return { label: "MemoryReflection", cls: "pill-ok" };
    case "emotionUpdate":
      return { label: "EmotionUpdate", cls: "pill-ok" };
    case "proactiveOutreach":
      return { label: "ProactiveOutreach", cls: "pill-tone" };
  }
}

export class ModelDrawer {
  readonly root: HTMLElement;
  private readonly tabsEl: HTMLElement;
  private readonly bodyEl: HTMLElement;
  private readonly callbacks: ModelDrawerCallbacks;

  private activeTab: ModelDrawerTab = "models";
  private ram: SystemRamStatus | null = null;
  private models: ModelInfo[] = [];
  private innerLoopStatus: InnerLoopStatus | null = null;
  private plugins: PluginInfo[] = [];
  private mcpConfig: McpConfig | null = null;
  private mcpTools: McpToolInfo[] = [];
  private mcpNote: string | null = null;

  private busyModelId: string | null = null;
  private addingDemoModels = false;
  private runningBeat = false;
  private errorBanner: string | null = null;

  constructor(callbacks: ModelDrawerCallbacks) {
    this.callbacks = callbacks;

    this.root = document.createElement("aside");
    this.root.className = "drawer hidden";
    this.root.setAttribute("aria-label", "Models, Inner Loop and MCP Settings");

    const header = document.createElement("div");
    header.className = "drawer-header";

    const titleWrap = document.createElement("div");
    titleWrap.className = "drawer-title-wrap";
    const icon = document.createElement("span");
    icon.className = "drawer-title-icon";
    icon.innerHTML = ICONS.chip;
    const h2 = document.createElement("h2");
    h2.textContent = "Models & Runtime";
    titleWrap.append(icon, h2);

    const closeBtn = document.createElement("button");
    closeBtn.type = "button";
    closeBtn.className = "icon-btn";
    closeBtn.title = "Close Models Drawer";
    closeBtn.innerHTML = ICONS.close;
    closeBtn.addEventListener("click", () => this.callbacks.onClose());

    header.append(titleWrap, closeBtn);

    this.tabsEl = document.createElement("div");
    this.tabsEl.className = "drawer-tabs";

    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "drawer-body";

    this.root.append(header, this.tabsEl, this.bodyEl);
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
    if (this.isOpen() && this.activeTab === "models") this.render();
  }

  updateInnerLoopStatus(status: InnerLoopStatus): void {
    this.innerLoopStatus = status;
    if (this.isOpen() && this.activeTab === "innerLoop") this.render();
  }

  async refresh(): Promise<void> {
    try {
      const [ram, models, innerLoopStatus, plugins, mcpConfig] = await Promise.all([
        invokeCommand("get_ram_status"),
        invokeCommand("list_models", { kind: null }),
        invokeCommand("get_inner_loop_status"),
        invokeCommand("list_plugins"),
        invokeCommand("get_mcp_config"),
      ]);
      this.ram = ram;
      this.models = models;
      this.innerLoopStatus = innerLoopStatus;
      this.plugins = plugins;
      this.mcpConfig = mcpConfig;

      if (mcpConfig.enabled) {
        try {
          this.mcpTools = await invokeCommand("list_mcp_tools");
          this.mcpNote = null;
        } catch (err) {
          this.mcpTools = [];
          this.mcpNote = `Non-blocking MCP notice: ${formatSoulError(err)}`;
        }
      } else {
        this.mcpTools = [];
        this.mcpNote = null;
      }

      this.callbacks.onRamUpdated(ram);
      this.errorBanner = null;
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

  private async handleSwitchEmbed(modelId: string): Promise<void> {
    this.busyModelId = modelId;
    this.errorBanner = null;
    this.render();
    try {
      const active = await invokeCommand("switch_embed_model", { modelId });
      await this.refresh();
      this.callbacks.onToast(`Activated ONNX Embedder: ${active.name}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.busyModelId = null;
      this.render();
    }
  }

  private async handleRemoveCustomModel(model: ModelInfo): Promise<void> {
    this.busyModelId = model.id;
    this.errorBanner = null;
    this.render();
    try {
      await invokeCommand("remove_custom_model", {
        kind: model.kind,
        modelId: model.id,
      });
      await this.refresh();
      this.callbacks.onToast(`Removed ${model.name}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.busyModelId = null;
      this.render();
    }
  }

  private async handleAddCustomModel(
    kind: Extract<ModelKind, "llm" | "asr" | "embed">
  ): Promise<void> {
    this.errorBanner = null;
    try {
      const registered = await pickAndRegisterModel(kind);
      if (!registered) return;
      await this.refresh();
      this.callbacks.onToast(`Imported ${registered.name}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
      this.render();
    }
  }

  private async handleAddPreviewDemoModels(): Promise<void> {
    if (isTauriRuntime() || this.addingDemoModels) return;
    this.addingDemoModels = true;
    this.errorBanner = null;
    this.render();
    try {
      const hasLlm = this.models.some((m) => m.kind === "llm");
      const hasAsr = this.models.some((m) => m.kind === "asr");

      if (!hasLlm) {
        const llm = await invokeCommand("register_custom_model", {
          kind: "llm",
          path: "/Preview/Models/typhoon2.5-qwen3-4b-q4_k_m.gguf",
          name: "Typhoon 2.5 Qwen3 4B (Preview Demo)",
        });
        await invokeCommand("hot_swap_llm", { modelId: llm.id });
      }
      if (!hasAsr) {
        const asr = await invokeCommand("register_custom_model", {
          kind: "asr",
          path: "/Preview/Models/ggml-base.bin",
          name: "Whisper Base Multilingual (Preview Demo)",
        });
        await invokeCommand("switch_asr_model", { modelId: asr.id });
      }

      await this.refresh();
      this.callbacks.onToast("Preview Demo LLM + Whisper ASR loaded and activated.");
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.addingDemoModels = false;
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

  private async handleUpdateInnerLoopConfig(patch: Partial<InnerLoopConfig>): Promise<void> {
    const current = this.innerLoopStatus?.config ?? {
      enabled: false,
      intervalMinutes: 20,
      proactiveOutreachEnabled: false,
      quietHoursStart: 23,
      quietHoursEnd: 7,
      minIdleSeconds: 180,
    };
    const nextConfig: InnerLoopConfig = { ...current, ...patch };
    try {
      this.innerLoopStatus = await invokeCommand("update_inner_loop_config", {
        config: nextConfig,
      });
      this.render();
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
      this.render();
    }
  }

  private async handleTriggerInnerLoopBeat(): Promise<void> {
    this.runningBeat = true;
    this.render();
    try {
      const status = await invokeCommand("trigger_inner_loop_beat");
      this.innerLoopStatus = status;
      this.callbacks.onToast(status.lastBeatSummary || `Beat: ${status.lastBeatKind}`);
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
    } finally {
      this.runningBeat = false;
      this.render();
    }
  }

  private async handleTogglePlugin(name: string, enabled: boolean): Promise<void> {
    try {
      this.plugins = await invokeCommand("set_plugin_enabled", { name, enabled });
      this.callbacks.onToast(`${name} ${enabled ? "enabled" : "disabled"}.`);
      this.render();
    } catch (err) {
      this.errorBanner = formatSoulError(err);
      this.callbacks.onToast(this.errorBanner, "error");
      this.render();
    }
  }

  private async handleUpdateMcpConfig(patch: Partial<McpConfig>): Promise<void> {
    const current = this.mcpConfig ?? {
      enabled: false,
      configPath: null,
      servers: [],
    };
    const nextConfig: McpConfig = { ...current, ...patch };
    try {
      this.mcpConfig = await invokeCommand("update_mcp_config", { config: nextConfig });
      if (this.mcpConfig.enabled) {
        try {
          this.mcpTools = await invokeCommand("list_mcp_tools");
          this.mcpNote = null;
        } catch (err) {
          this.mcpTools = [];
          this.mcpNote = `Non-blocking MCP notice: ${formatSoulError(err)}`;
        }
      } else {
        this.mcpTools = [];
        this.mcpNote = null;
      }
      this.render();
    } catch (err) {
      this.mcpNote = `Non-blocking MCP error: ${formatSoulError(err)}`;
      this.callbacks.onToast(this.mcpNote, "error");
      this.render();
    }
  }

  private async handlePickMcpConfig(): Promise<void> {
    try {
      const path = await pickMcpConfigFile();
      if (!path) return;
      await this.handleUpdateMcpConfig({ configPath: path });
      this.callbacks.onToast(`Selected MCP config: ${path}`);
    } catch (err) {
      this.mcpNote = `Non-blocking MCP picker notice: ${formatSoulError(err)}`;
      this.callbacks.onToast(this.mcpNote, "error");
      this.render();
    }
  }

  private renderTabs(): void {
    this.tabsEl.innerHTML = "";
    const seg = document.createElement("div");
    seg.className = "segmented drawer-tab-seg";

    const tabs: Array<{ id: ModelDrawerTab; label: string }> = [
      { id: "models", label: "Models" },
      { id: "innerLoop", label: "Inner Loop & MCP" },
    ];
    for (const tab of tabs) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = this.activeTab === tab.id ? "seg-btn active" : "seg-btn";
      btn.textContent = tab.label;
      btn.addEventListener("click", () => {
        this.activeTab = tab.id;
        this.render();
      });
      seg.appendChild(btn);
    }

    this.tabsEl.appendChild(seg);
  }

  private renderRamCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card ram-card";

    const ram = this.ram ?? {
      totalRamMb: 8192,
      availableRamMb: 5900,
      processRamMb: 210,
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

    const pctBudget = Math.min(
      100,
      Math.round((ram.processRamMb / Math.max(1, ram.budgetCeilingMb)) * 100)
    );
    const pctTotal = Math.min(
      100,
      Math.round((ram.processRamMb / Math.max(1, ram.totalRamMb)) * 100)
    );

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

  private renderPreviewHelperCard(): HTMLElement | null {
    if (isTauriRuntime()) return null;
    const hasBoth =
      this.models.some((m) => m.kind === "llm" && m.isActive) &&
      this.models.some((m) => m.kind === "asr" && m.isActive);
    if (hasBoth) return null;

    const card = document.createElement("section");
    card.className = "drawer-card byom-preview-card";

    const head = document.createElement("div");
    head.className = "section-head";
    const title = document.createElement("h3");
    title.textContent = "Browser Preview — BYOM Mode";
    const badge = document.createElement("span");
    badge.className = "pill pill-soft";
    badge.textContent = "Empty State";
    head.append(title, badge);

    const desc = document.createElement("p");
    desc.className = "model-notes";
    desc.textContent =
      "App boots with zero bundled AI weights (BYOM). Use Add .gguf / Add .bin below or load simulated preview models to test chat & voice.";

    const btnRow = document.createElement("div");
    btnRow.className = "migration-actions";
    const demoBtn = document.createElement("button");
    demoBtn.type = "button";
    demoBtn.className = "btn-primary btn-sm";
    demoBtn.disabled = this.addingDemoModels;
    demoBtn.textContent = this.addingDemoModels
      ? "Loading Demo Models..."
      : "Add Demo Model (Preview)";
    demoBtn.addEventListener("click", () => void this.handleAddPreviewDemoModels());
    btnRow.appendChild(demoBtn);

    card.append(head, desc, btnRow);
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
    const srcText = model.source === "custom" ? "BYOM Local File" : model.source;
    metaRow.textContent = `${ramText} · ${sizeText} · ${srcText}`;

    const pathEl = document.createElement("div");
    pathEl.className = "model-path";
    pathEl.title = model.path;
    pathEl.textContent = model.path;

    item.append(head, metaRow, pathEl);

    if (model.notes) {
      const notes = document.createElement("div");
      notes.className = "model-notes";
      notes.textContent = model.notes;
      item.appendChild(notes);
    }

    const actions = document.createElement("div");
    actions.className = "model-actions";

    if (model.source === "custom") {
      const removeBtn = document.createElement("button");
      removeBtn.type = "button";
      removeBtn.className = "btn-danger-ghost btn-sm";
      removeBtn.disabled = this.busyModelId !== null;
      removeBtn.textContent = "Remove";
      removeBtn.addEventListener("click", () => void this.handleRemoveCustomModel(model));
      actions.appendChild(removeBtn);
    }

    if (model.kind === "llm" || model.kind === "asr" || model.kind === "embed") {
      const btn = document.createElement("button");
      btn.type = "button";
      const isBusy = this.busyModelId === model.id;
      btn.className = model.isActive ? "btn-chip active-chip" : "btn-primary btn-sm";
      btn.disabled = model.isActive || isBusy || this.busyModelId !== null;
      btn.textContent = model.isActive
        ? "Active"
        : isBusy
        ? "Loading..."
        : model.kind === "llm"
        ? "Hot-Swap LLM"
        : model.kind === "asr"
        ? "Use ASR"
        : "Use Embed";
      btn.addEventListener("click", () => {
        if (model.kind === "llm") void this.handleSwapLlm(model.id);
        else if (model.kind === "asr") void this.handleSwitchAsr(model.id);
        else void this.handleSwitchEmbed(model.id);
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
    emptyHint: string,
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
      const empty = document.createElement("div");
      empty.className = "empty-byom-box";
      empty.textContent = emptyHint;
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

    const btnVector = document.createElement("button");
    btnVector.type = "button";
    btnVector.className = mode === "vector" ? "seg-btn active" : "seg-btn";
    btnVector.textContent = "Vector 60fps";
    btnVector.addEventListener("click", () => {
      this.callbacks.onEngineModeChange("vector");
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

    seg.append(btnVector, btnLive2d);

    const summary = this.callbacks.getLive2dSummary();
    const info = document.createElement("div");
    info.className = "model-notes";
    info.style.marginTop = "8px";
    info.textContent =
      mode === "vector"
        ? "Dual-Character Procedural Engine (Coral & Kai — 60 FPS cursor gaze, blink, springs, 7 emotions, viseme lip-sync)."
        : `Cubism Runtime: ${summary.name} · ${summary.expressionCount} .exp3 presets · ${
            summary.hasIdleMotion ? "idle .motion3 loaded" : "procedural idle"
          } (${summary.sourceLabel})`;

    card.append(head, seg, info);
    return card;
  }

  private renderToggleSwitch(
    checked: boolean,
    onChange: (next: boolean) => void,
    ariaLabel: string
  ): HTMLElement {
    const label = document.createElement("label");
    label.className = "toggle-switch";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = checked;
    input.setAttribute("aria-label", ariaLabel);
    input.addEventListener("change", () => onChange(input.checked));
    const slider = document.createElement("span");
    slider.className = "toggle-slider";
    label.append(input, slider);
    return label;
  }

  private renderInnerLoopCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card";

    const status = this.innerLoopStatus ?? {
      config: {
        enabled: false,
        intervalMinutes: 20,
        proactiveOutreachEnabled: false,
        quietHoursStart: 23,
        quietHoursEnd: 7,
        minIdleSeconds: 180,
      },
      lastBeatAtMs: null,
      lastBeatKind: "skippedDisabled" as const,
      lastBeatSummary: "Inner Loop is disabled (opt-in).",
      nextBeatAtMs: null,
      totalBeats: 0,
    };

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = "G.R.I.L.L.O. Inner Loop";
    const pillMeta = beatStatusMeta(status.lastBeatKind, status.config.enabled);
    const pill = document.createElement("span");
    pill.className = `pill ${pillMeta.cls}`;
    pill.textContent = pillMeta.label;
    head.append(h3, pill);

    const desc = document.createElement("p");
    desc.className = "model-notes";
    desc.textContent =
      "Periodic silent background heartbeats for memory reflection & emotion decay (OFF by default; skips automatically if no LLM is loaded).";

    const controls = document.createElement("div");
    controls.className = "settings-list";

    const enableRow = document.createElement("div");
    enableRow.className = "setting-row";
    const enableText = document.createElement("div");
    enableText.innerHTML = `<div class="setting-name">Enable Inner Loop</div><div class="setting-sub">Opt-in periodic companion reflection</div>`;
    enableRow.append(
      enableText,
      this.renderToggleSwitch(
        status.config.enabled,
        (enabled) => void this.handleUpdateInnerLoopConfig({ enabled }),
        "Enable G.R.I.L.L.O. Inner Loop"
      )
    );

    const intervalRow = document.createElement("div");
    intervalRow.className = "setting-row";
    const intervalText = document.createElement("div");
    intervalText.innerHTML = `<div class="setting-name">Beat Interval</div><div class="setting-sub">Minutes between background checks</div>`;
    const intervalSeg = document.createElement("div");
    intervalSeg.className = "mini-seg";
    for (const mins of [15, 20, 30]) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className =
        status.config.intervalMinutes === mins ? "mini-seg-btn active" : "mini-seg-btn";
      btn.textContent = `${mins}m`;
      btn.addEventListener(
        "click",
        () => void this.handleUpdateInnerLoopConfig({ intervalMinutes: mins })
      );
      intervalSeg.appendChild(btn);
    }
    intervalRow.append(intervalText, intervalSeg);

    const outreachRow = document.createElement("div");
    outreachRow.className = "setting-row";
    const outreachText = document.createElement("div");
    outreachText.innerHTML = `<div class="setting-name">Proactive Outreach</div><div class="setting-sub">Allow gentle check-ins when idle</div>`;
    outreachRow.append(
      outreachText,
      this.renderToggleSwitch(
        status.config.proactiveOutreachEnabled,
        (proactiveOutreachEnabled) =>
          void this.handleUpdateInnerLoopConfig({ proactiveOutreachEnabled }),
        "Enable Proactive Outreach"
      )
    );

    controls.append(enableRow, intervalRow, outreachRow);

    const beatMeta = document.createElement("div");
    beatMeta.className = "beat-status-box";
    beatMeta.textContent = `${status.lastBeatSummary ?? "Ready"} · Total beats: ${status.totalBeats}`;

    const actions = document.createElement("div");
    actions.className = "model-actions";
    const runBtn = document.createElement("button");
    runBtn.type = "button";
    runBtn.className = "btn-primary btn-sm";
    runBtn.disabled = this.runningBeat;
    runBtn.textContent = this.runningBeat ? "Checking..." : "Run Beat Check";
    runBtn.addEventListener("click", () => void this.handleTriggerInnerLoopBeat());
    actions.appendChild(runBtn);

    card.append(head, desc, controls, beatMeta, actions);
    return card;
  }

  private renderPluginsCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card";

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = "Modular Plugins";
    const countPill = document.createElement("span");
    countPill.className = "pill pill-soft";
    countPill.textContent = `${this.plugins.filter((p) => p.enabled).length}/${this.plugins.length} active`;
    head.append(h3, countPill);
    card.appendChild(head);

    if (this.plugins.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-hint";
      empty.textContent = "No modular plugins reported.";
      card.appendChild(empty);
      return card;
    }

    const list = document.createElement("div");
    list.className = "plugins-list";
    for (const plugin of this.plugins) {
      const item = document.createElement("div");
      item.className = "plugin-item";

      const top = document.createElement("div");
      top.className = "setting-row";

      const info = document.createElement("div");
      const nameEl = document.createElement("div");
      nameEl.className = "setting-name";
      nameEl.textContent = plugin.name;
      const descEl = document.createElement("div");
      descEl.className = "setting-sub";
      descEl.textContent = plugin.description;
      info.append(nameEl, descEl);

      top.append(
        info,
        this.renderToggleSwitch(
          plugin.enabled,
          (enabled) => void this.handleTogglePlugin(plugin.name, enabled),
          `Toggle ${plugin.name}`
        )
      );

      const actionsWrap = document.createElement("div");
      actionsWrap.className = "plugin-actions-chips";
      for (const act of plugin.supportedActions) {
        const chip = document.createElement("span");
        chip.className = "pill pill-soft";
        chip.textContent = act;
        actionsWrap.appendChild(chip);
      }

      item.append(top, actionsWrap);
      list.appendChild(item);
    }

    card.appendChild(list);
    return card;
  }

  private renderMcpCard(): HTMLElement {
    const card = document.createElement("section");
    card.className = "drawer-card";

    const mcp = this.mcpConfig ?? {
      enabled: false,
      configPath: null,
      servers: [],
    };

    const head = document.createElement("div");
    head.className = "section-head";
    const h3 = document.createElement("h3");
    h3.textContent = "Optional MCP Client";
    const pill = document.createElement("span");
    pill.className = `pill ${mcp.enabled ? "pill-ok" : "pill-muted"}`;
    pill.textContent = mcp.enabled ? "Enabled" : "Disabled";
    head.append(h3, pill);

    const note = document.createElement("p");
    note.className = "model-notes";
    note.textContent =
      "Optional Model Context Protocol tools (OFF by default). MCP connection errors never block chat or offline companion mode.";

    const toggleRow = document.createElement("div");
    toggleRow.className = "setting-row";
    toggleRow.style.marginTop = "8px";
    const toggleLabel = document.createElement("div");
    toggleLabel.innerHTML = `<div class="setting-name">Enable MCP Client</div><div class="setting-sub">Load external tool servers from JSON config</div>`;
    toggleRow.append(
      toggleLabel,
      this.renderToggleSwitch(
        mcp.enabled,
        (enabled) => void this.handleUpdateMcpConfig({ enabled }),
        "Enable Optional MCP Client"
      )
    );

    const pathBox = document.createElement("div");
    pathBox.className = "model-path";
    pathBox.textContent = mcp.configPath || "No MCP .json config selected";

    const pathActions = document.createElement("div");
    pathActions.className = "model-actions";

    if (mcp.configPath) {
      const clearBtn = document.createElement("button");
      clearBtn.type = "button";
      clearBtn.className = "btn-ghost btn-xs";
      clearBtn.textContent = "Clear";
      clearBtn.addEventListener("click", () =>
        void this.handleUpdateMcpConfig({ configPath: null, servers: [] })
      );
      pathActions.appendChild(clearBtn);
    }

    const pickBtn = document.createElement("button");
    pickBtn.type = "button";
    pickBtn.className = "btn-secondary btn-sm";
    pickBtn.innerHTML = `${ICONS.folder} <span>Pick .json Config</span>`;
    pickBtn.addEventListener("click", () => void this.handlePickMcpConfig());
    pathActions.appendChild(pickBtn);

    card.append(head, note, toggleRow, pathBox, pathActions);

    if (this.mcpNote) {
      const warn = document.createElement("div");
      warn.className = "beat-status-box";
      warn.textContent = this.mcpNote;
      card.appendChild(warn);
    }

    const serversTitle = document.createElement("div");
    serversTitle.className = "setting-name";
    serversTitle.style.marginTop = "10px";
    serversTitle.textContent = `Configured Servers (${mcp.servers.length}) · Tools (${this.mcpTools.length})`;
    card.appendChild(serversTitle);

    if (mcp.servers.length === 0) {
      const empty = document.createElement("p");
      empty.className = "empty-hint";
      empty.style.marginTop = "4px";
      empty.textContent = "No MCP servers configured. Companion runs 100% self-contained.";
      card.appendChild(empty);
    } else {
      const srvList = document.createElement("div");
      srvList.className = "plugins-list";
      srvList.style.marginTop = "6px";
      for (const srv of mcp.servers) {
        const row = document.createElement("div");
        row.className = "plugin-item";
        const top = document.createElement("div");
        top.className = "fact-top";
        const nm = document.createElement("strong");
        nm.className = "setting-name";
        nm.textContent = `${srv.name} (${srv.transport})`;
        const st = document.createElement("span");
        st.className = `pill ${srv.enabled ? "pill-ok" : "pill-muted"}`;
        st.textContent = srv.enabled ? "Configured" : "Off";
        top.append(nm, st);

        const target = document.createElement("div");
        target.className = "setting-sub";
        target.textContent = srv.url || [srv.command, ...srv.args].filter(Boolean).join(" ") || "—";

        row.append(top, target);
        srvList.appendChild(row);
      }
      card.appendChild(srvList);
    }

    return card;
  }

  render(): void {
    this.renderTabs();
    this.bodyEl.innerHTML = "";

    if (this.errorBanner) {
      const err = document.createElement("div");
      err.className = "error-banner";
      err.textContent = this.errorBanner;
      this.bodyEl.appendChild(err);
    }

    if (this.activeTab === "innerLoop") {
      this.bodyEl.appendChild(this.renderInnerLoopCard());
      this.bodyEl.appendChild(this.renderPluginsCard());
      this.bodyEl.appendChild(this.renderMcpCard());
      return;
    }

    this.bodyEl.appendChild(this.renderRamCard());
    this.bodyEl.appendChild(this.renderEngineCard());

    const previewCard = this.renderPreviewHelperCard();
    if (previewCard) {
      this.bodyEl.appendChild(previewCard);
    }

    const llms = this.models.filter((m) => m.kind === "llm");
    const asrs = this.models.filter((m) => m.kind === "asr");
    const embeds = this.models.filter((m) => m.kind === "embed");
    const live2ds = this.models.filter((m) => m.kind === "live2d");

    this.bodyEl.appendChild(
      this.renderSection(
        "Local LLM (.gguf)",
        llms,
        "ยังไม่ได้นำเข้าไฟล์โมเดล LLM (.gguf) — กด 'Add .gguf' ด้านบนขวาเพื่อเลือกไฟล์จากเครื่องของคุณ (เช่น Typhoon / Qwen Q4_K_M)",
        {
          label: "Add .gguf",
          onClick: () => void this.handleAddCustomModel("llm"),
        }
      )
    );
    this.bodyEl.appendChild(
      this.renderSection(
        "Whisper Speech ASR (.bin)",
        asrs,
        "ยังไม่ได้นำเข้าไฟล์โมเดลเสียง Whisper (.bin) — กด 'Add .bin' เพื่อเลือกไฟล์สำหรับรับเสียงพูด (เช่น ggml-base.bin)",
        {
          label: "Add .bin",
          onClick: () => void this.handleAddCustomModel("asr"),
        }
      )
    );
    this.bodyEl.appendChild(
      this.renderSection(
        "Embedding Model (.onnx, optional)",
        embeds,
        "ยังไม่ได้นำเข้าไฟล์ .onnx (ไม่บังคับ) — ระบบใช้ตัวค้นหาความจำสำรองให้อัตโนมัติ หรือกด 'Add .onnx' (ต้องมี tokenizer.json อยู่โฟลเดอร์เดียวกัน)",
        {
          label: "Add .onnx",
          onClick: () => void this.handleAddCustomModel("embed"),
        }
      )
    );
    this.bodyEl.appendChild(
      this.renderSection(
        "Live2D Avatars (.model3.json)",
        live2ds,
        "No Live2D avatar bundles found. Tap 'Load Folder' to import a .model3.json character directory.",
        {
          label: "Load Folder",
          onClick: () => void this.handlePickLive2dFolder(),
        }
      )
    );
  }
}
