import {
  formatSoulError,
  invokeCommand,
  isTauriRuntime,
  listenEvent,
  type Live2dFolderFiles,
} from "./api/tauriClient";
import { AvatarStage } from "./avatar/AvatarStage";
import { ALL_EMOTIONS, EMOTION_META, type AvatarEngineMode } from "./avatar/types";
import { ICONS } from "./components/icons";
import { MemoryDrawer } from "./components/MemoryDrawer";
import { ModelDrawer } from "./components/ModelDrawer";
import type {
  ChatTurn,
  EmotionState,
  EmotionTag,
  LanguagePref,
  RelationshipState,
  SystemRamStatus,
  VoiceMode,
} from "./types/ipc";

interface ToastItem {
  id: number;
  message: string;
  kind: "info" | "error";
}

export class SoulMateApp {
  private readonly mountEl: HTMLElement;
  private stage!: AvatarStage;
  private modelDrawer!: ModelDrawer;
  private memoryDrawer!: MemoryDrawer;

  private unlisteners: Array<() => void> = [];
  private currentEmotion: EmotionState = {
    tag: "joy",
    intensity: 0.72,
    reason: "Ready to greet you",
    timestampMs: Date.now(),
  };

  private relationship: RelationshipState = {
    trust: 0.64,
    warmth: 0.72,
    familiarity: 0.58,
    moodBaseline: 0.68,
    totalTurns: 12,
    lastInteractionAt: Date.now(),
  };

  private ramStatus: SystemRamStatus | null = null;
  private voiceMode: VoiceMode = "pushToTalk";
  private languagePref: LanguagePref = "auto";
  private petMode = false;
  private testerOpen = false;
  private bondOpen = false;
  private historyOpen = false;
  private isListening = false;
  private isSending = false;
  private isSpeaking = false;
  private speakingDecayTimer: number | null = null;
  private pointerDownAt = 0;
  private pointerActive = false;
  private startedOnPointerDown = false;
  private ignoreNextClick = false;

  private subtitleText =
    "สวัสดีค่ะ! มีนาอยู่เป็นเพื่อนตรงนี้เสมอนะ พิมพ์หรือกดไมค์คุยกันได้เลยค่ะ";
  private transcriptPreview = "";
  private chatLog: ChatTurn[] = [];
  private testerIntensity = 0.88;
  private toastCounter = 0;

  // Key DOM refs
  private canvasEl!: HTMLCanvasElement;
  private topRamLabelEl!: HTMLElement;
  private topEngineBadgeEl!: HTMLElement;
  private bondFillEl!: HTMLElement;
  private bondPctEl!: HTMLElement;
  private bondPopoverEl!: HTMLElement;
  private testerBarEl!: HTMLElement;
  private subtitleEmotionEl!: HTMLElement;
  private subtitleTextEl!: HTMLElement;
  private transcriptLineEl!: HTMLElement;
  private historyPopoverEl!: HTMLElement;
  private chatInputEl!: HTMLInputElement;
  private sendBtnEl!: HTMLButtonElement;
  private micBtnEl!: HTMLButtonElement;
  private stopSpeakBtnEl!: HTMLButtonElement;
  private petBtnEl!: HTMLButtonElement;
  private testerBtnEl!: HTMLButtonElement;
  private toastStackEl!: HTMLElement;
  private voiceBtns: Map<VoiceMode, HTMLButtonElement> = new Map();
  private langBtns: Map<LanguagePref, HTMLButtonElement> = new Map();
  private emotionChipBtns: Map<EmotionTag, HTMLButtonElement> = new Map();

  constructor(mountEl: HTMLElement) {
    this.mountEl = mountEl;
  }

  async init(): Promise<void> {
    this.buildShell();
    this.stage = new AvatarStage(this.canvasEl);
    this.stage.setEmotion(this.currentEmotion);

    await this.subscribeEvents();
    await this.hydrateInitialState();
    this.syncUiState();
    window.addEventListener("keydown", this.handleGlobalKeydown);
  }

  private handleGlobalKeydown = (e: KeyboardEvent): void => {
    if (e.key !== "Escape") return;
    if (this.modelDrawer.isOpen()) this.modelDrawer.close();
    if (this.memoryDrawer.isOpen()) this.memoryDrawer.close();
    if (this.bondOpen || this.historyOpen || this.testerOpen) {
      this.bondOpen = false;
      this.historyOpen = false;
      this.testerOpen = false;
      this.syncPopovers();
    }
  };

  private buildShell(): void {
    this.mountEl.innerHTML = "";

    const shell = document.createElement("div");
    shell.className = "app-shell";

    // Ambient soft stage background (transparent in pet mode)
    const backdrop = document.createElement("div");
    backdrop.className = "stage-backdrop";
    shell.appendChild(backdrop);

    // Full-bleed character stage + drag region
    const stageArea = document.createElement("main");
    stageArea.className = "stage-area";

    const dragPad = document.createElement("div");
    dragPad.className = "pet-drag-pad";
    dragPad.setAttribute("data-tauri-drag-region", "");
    dragPad.title = "Drag window";
    stageArea.appendChild(dragPad);

    this.canvasEl = document.createElement("canvas");
    this.canvasEl.className = "avatar-canvas";
    stageArea.appendChild(this.canvasEl);

    shell.appendChild(stageArea);

    // Top Floating Overlay Bar
    const topBar = this.buildTopBar();
    shell.appendChild(topBar);

    // Popovers & Tester Strip
    this.bondPopoverEl = this.buildBondPopover();
    shell.appendChild(this.bondPopoverEl);

    this.testerBarEl = this.buildTesterStrip();
    shell.appendChild(this.testerBarEl);

    this.historyPopoverEl = this.buildHistoryPopover();
    shell.appendChild(this.historyPopoverEl);

    // Subtitle Bubble + Bottom Control Dock
    const bottomOverlay = document.createElement("section");
    bottomOverlay.className = "bottom-overlay";
    bottomOverlay.append(this.buildSubtitleCard(), this.buildDock());
    shell.appendChild(bottomOverlay);

    // Drawers
    this.modelDrawer = new ModelDrawer({
      onClose: () => this.modelDrawer.close(),
      onRamUpdated: (ram) => {
        this.ramStatus = ram;
        this.syncTopRamLabel();
      },
      onEngineModeChange: (mode) => {
        this.stage.setEngineMode(mode);
        this.syncEngineBadge();
      },
      onLive2dFilesLoaded: async (folder: Live2dFolderFiles) => {
        const res = await this.stage.loadLive2dFiles(folder);
        this.syncEngineBadge();
        this.showToast(`Loaded Live2D "${res.name}" (${res.expressionCount} expressions)`);
      },
      onLive2dModelSelected: async (model) => {
        if (isTauriRuntime() && model.path && !model.path.startsWith("/bundled/")) {
          const folderPath = model.path.toLowerCase().endsWith(".model3.json")
            ? model.path.slice(0, model.path.lastIndexOf("/"))
            : model.path;
          await invokeCommand("load_live2d_folder", { folderPath });
        }
        const res = await this.stage.loadLive2dModelInfo(model, true);
        this.syncEngineBadge();
        this.showToast(`Activated Live2D "${res.name}" (${res.expressionCount} expressions)`);
      },
      onLive2dDefaultSelected: () => {
        void this.stage.loadDefaultLive2dAsset(true).then(() => {
          this.syncEngineBadge();
        });
      },
      getEngineMode: () => this.stage?.getEngineMode() ?? "mina",
      getLive2dSummary: () => this.stage.getLive2dInfo(),
      onToast: (msg, kind) => this.showToast(msg, kind),
    });

    this.memoryDrawer = new MemoryDrawer({
      onClose: () => this.memoryDrawer.close(),
      onRelationshipUpdated: (rel) => {
        this.relationship = rel;
        this.syncBondMeter();
      },
      onToast: (msg, kind) => this.showToast(msg, kind),
    });

    shell.append(this.modelDrawer.root, this.memoryDrawer.root);

    this.toastStackEl = document.createElement("div");
    this.toastStackEl.className = "toast-stack";
    shell.appendChild(this.toastStackEl);

    this.mountEl.appendChild(shell);
  }

  private buildTopBar(): HTMLElement {
    const bar = document.createElement("header");
    bar.className = "top-bar";
    bar.setAttribute("data-tauri-drag-region", "");

    // Left: Companion identity + engine badge
    const brand = document.createElement("div");
    brand.className = "brand-pill";
    brand.setAttribute("data-tauri-drag-region", "");

    const nameSpan = document.createElement("span");
    nameSpan.className = "brand-name";
    nameSpan.textContent = "Mina · มีนา";

    this.topEngineBadgeEl = document.createElement("span");
    this.topEngineBadgeEl.className = "brand-sub";
    this.topEngineBadgeEl.textContent = isTauriRuntime() ? "Mina 60fps" : "Preview";

    brand.append(nameSpan, this.topEngineBadgeEl);

    // Right: Bond meter, Tester, Pet Mode, Models, Diary
    const actions = document.createElement("div");
    actions.className = "top-actions";

    // Bond & Mood Heart Meter Button
    const bondBtn = document.createElement("button");
    bondBtn.type = "button";
    bondBtn.className = "pill-btn bond-btn";
    bondBtn.title = "Bond & Mood Meter";

    const heartWrap = document.createElement("span");
    heartWrap.className = "heart-meter-icon";
    heartWrap.innerHTML = ICONS.heart;
    this.bondFillEl = document.createElement("span");
    this.bondFillEl.className = "heart-meter-glow";
    heartWrap.appendChild(this.bondFillEl);

    this.bondPctEl = document.createElement("span");
    this.bondPctEl.textContent = "68%";
    bondBtn.append(heartWrap, this.bondPctEl);
    bondBtn.addEventListener("click", () => {
      this.bondOpen = !this.bondOpen;
      if (this.bondOpen) this.testerOpen = false;
      this.syncPopovers();
    });

    // Expression & Lip-Sync Tester Toggle
    this.testerBtnEl = document.createElement("button");
    this.testerBtnEl.type = "button";
    this.testerBtnEl.className = "icon-btn";
    this.testerBtnEl.title = "Expression & Lip-Sync Tester";
    this.testerBtnEl.innerHTML = ICONS.sparkles;
    this.testerBtnEl.addEventListener("click", () => {
      this.testerOpen = !this.testerOpen;
      if (this.testerOpen) this.bondOpen = false;
      this.syncPopovers();
    });

    // Desktop Pet Mode Toggle
    this.petBtnEl = document.createElement("button");
    this.petBtnEl.type = "button";
    this.petBtnEl.className = "icon-btn";
    this.petBtnEl.title = "Toggle Desktop Pet Mode";
    this.petBtnEl.innerHTML = ICONS.pet;
    this.petBtnEl.addEventListener("click", () => void this.togglePetMode());

    // Models Drawer Button
    const modelBtn = document.createElement("button");
    modelBtn.type = "button";
    modelBtn.className = "pill-btn";
    modelBtn.title = "Model Switcher & RAM";
    const chipIcon = document.createElement("span");
    chipIcon.innerHTML = ICONS.chip;
    this.topRamLabelEl = document.createElement("span");
    this.topRamLabelEl.textContent = "Models";
    modelBtn.append(chipIcon, this.topRamLabelEl);
    modelBtn.addEventListener("click", () => {
      this.memoryDrawer.close();
      if (this.modelDrawer.isOpen()) this.modelDrawer.close();
      else this.modelDrawer.open();
    });

    // Soul Diary Drawer Button
    const diaryBtn = document.createElement("button");
    diaryBtn.type = "button";
    diaryBtn.className = "icon-btn";
    diaryBtn.title = "Soul Memory & Dream Diary";
    diaryBtn.innerHTML = ICONS.book;
    diaryBtn.addEventListener("click", () => {
      this.modelDrawer.close();
      if (this.memoryDrawer.isOpen()) this.memoryDrawer.close();
      else this.memoryDrawer.open();
    });

    actions.append(bondBtn, this.testerBtnEl, this.petBtnEl, modelBtn, diaryBtn);
    bar.append(brand, actions);
    return bar;
  }

  private buildBondPopover(): HTMLElement {
    const pop = document.createElement("div");
    pop.className = "floating-popover bond-popover hidden";
    return pop;
  }

  private buildTesterStrip(): HTMLElement {
    const strip = document.createElement("div");
    strip.className = "tester-strip hidden";

    const rowTop = document.createElement("div");
    rowTop.className = "tester-row-top";

    const title = document.createElement("span");
    title.className = "tester-title";
    title.textContent = "Expression & Lip-Sync Tester";

    const intensityWrap = document.createElement("label");
    intensityWrap.className = "intensity-wrap";
    const intLabel = document.createElement("span");
    intLabel.textContent = "Intensity";
    const slider = document.createElement("input");
    slider.type = "range";
    slider.min = "0.2";
    slider.max = "1";
    slider.step = "0.05";
    slider.value = String(this.testerIntensity);
    slider.addEventListener("input", () => {
      this.testerIntensity = Number(slider.value);
      this.applyEmotion({
        tag: this.currentEmotion.tag,
        intensity: this.testerIntensity,
        reason: "QA Tester",
        timestampMs: Date.now(),
      });
    });
    intensityWrap.append(intLabel, slider);
    rowTop.append(title, intensityWrap);

    const chips = document.createElement("div");
    chips.className = "emotion-chips";
    for (const tag of ALL_EMOTIONS) {
      const meta = EMOTION_META[tag];
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "emotion-chip";
      const dot = document.createElement("span");
      dot.className = "emotion-dot";
      dot.style.background = meta.accentColor;
      const lbl = document.createElement("span");
      lbl.textContent = meta.labelEn;
      btn.append(dot, lbl);
      btn.addEventListener("click", () => {
        this.applyEmotion({
          tag,
          intensity: this.testerIntensity,
          reason: `Tested ${meta.labelEn}`,
          timestampMs: Date.now(),
        });
      });
      this.emotionChipBtns.set(tag, btn);
      chips.appendChild(btn);
    }

    const rowActions = document.createElement("div");
    rowActions.className = "tester-actions";

    const ttsBtn = document.createElement("button");
    ttsBtn.type = "button";
    ttsBtn.className = "btn-primary btn-xs";
    ttsBtn.textContent = "Test Voice & Lip-Sync";
    ttsBtn.addEventListener("click", () => {
      const sample =
        this.languagePref === "english"
          ? "Hello! My lips and expressions move smoothly in real time at sixty frames per second!"
          : "สวัสดีค่ะ! ตอนนี้มีนากำลังทดสอบการขยับปากและสีหน้าแบบเรียลไทม์อยู่นะคะ";
      void this.triggerSpeechPreview(sample, this.currentEmotion.tag);
    });

    const engineBtn = document.createElement("button");
    engineBtn.type = "button";
    engineBtn.className = "btn-secondary btn-xs";
    engineBtn.textContent = "Switch Mina / Live2D";
    engineBtn.addEventListener("click", () => {
      const next: AvatarEngineMode =
        this.stage.getEngineMode() === "mina" ? "live2d" : "mina";
      this.stage.setEngineMode(next);
      this.syncEngineBadge();
      this.showToast(
        next === "live2d"
          ? "Switched stage to Live2D .model3.json Cubism runtime"
          : "Switched stage to Mina Procedural 60 FPS engine"
      );
    });

    const stopBtn = document.createElement("button");
    stopBtn.type = "button";
    stopBtn.className = "btn-ghost btn-xs";
    stopBtn.textContent = "Stop Lip-Sync";
    stopBtn.addEventListener("click", () => void this.handleStopSpeaking());

    rowActions.append(ttsBtn, engineBtn, stopBtn);
    strip.append(rowTop, chips, rowActions);
    return strip;
  }

  private buildHistoryPopover(): HTMLElement {
    const pop = document.createElement("div");
    pop.className = "floating-popover history-popover hidden";
    return pop;
  }

  private buildSubtitleCard(): HTMLElement {
    const card = document.createElement("div");
    card.className = "subtitle-card";

    const head = document.createElement("div");
    head.className = "subtitle-head";

    this.subtitleEmotionEl = document.createElement("span");
    this.subtitleEmotionEl.className = "emotion-pill";

    const rightWrap = document.createElement("div");
    rightWrap.className = "subtitle-tools";

    this.transcriptLineEl = document.createElement("span");
    this.transcriptLineEl.className = "transcript-pill hidden";

    const histBtn = document.createElement("button");
    histBtn.type = "button";
    histBtn.className = "mini-icon-btn";
    histBtn.title = "Conversation History";
    histBtn.innerHTML = ICONS.history;
    histBtn.addEventListener("click", () => {
      this.historyOpen = !this.historyOpen;
      this.syncPopovers();
    });

    rightWrap.append(this.transcriptLineEl, histBtn);
    head.append(this.subtitleEmotionEl, rightWrap);

    this.subtitleTextEl = document.createElement("p");
    this.subtitleTextEl.className = "subtitle-text";
    this.subtitleTextEl.textContent = this.subtitleText;

    card.append(head, this.subtitleTextEl);
    return card;
  }

  private buildDock(): HTMLElement {
    const dock = document.createElement("div");
    dock.className = "control-dock";

    // Top control strip: Voice mode + Language selector + Stop speaking
    const metaRow = document.createElement("div");
    metaRow.className = "dock-meta-row";

    const voiceSeg = document.createElement("div");
    voiceSeg.className = "mini-seg";
    const voiceOpts: Array<{ mode: VoiceMode; label: string }> = [
      { mode: "pushToTalk", label: "PTT" },
      { mode: "handsFree", label: "Hands-Free" },
      { mode: "muted", label: "Mute" },
    ];
    for (const opt of voiceOpts) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "mini-seg-btn";
      b.textContent = opt.label;
      b.addEventListener("click", () => void this.handleVoiceModeChange(opt.mode));
      this.voiceBtns.set(opt.mode, b);
      voiceSeg.appendChild(b);
    }

    const langSeg = document.createElement("div");
    langSeg.className = "mini-seg";
    const langOpts: Array<{ pref: LanguagePref; label: string }> = [
      { pref: "auto", label: "Auto" },
      { pref: "thai", label: "ไทย" },
      { pref: "english", label: "EN" },
    ];
    for (const opt of langOpts) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "mini-seg-btn";
      b.textContent = opt.label;
      b.addEventListener("click", () => void this.handleLanguageChange(opt.pref));
      this.langBtns.set(opt.pref, b);
      langSeg.appendChild(b);
    }

    this.stopSpeakBtnEl = document.createElement("button");
    this.stopSpeakBtnEl.type = "button";
    this.stopSpeakBtnEl.className = "stop-speak-btn hidden";
    this.stopSpeakBtnEl.title = "Stop Speaking";
    this.stopSpeakBtnEl.innerHTML = `${ICONS.stop} <span>Stop</span>`;
    this.stopSpeakBtnEl.addEventListener("click", () => void this.handleStopSpeaking());

    metaRow.append(voiceSeg, langSeg, this.stopSpeakBtnEl);

    // Input row: Mic button + Thai/English input + Send button
    const inputForm = document.createElement("form");
    inputForm.className = "chat-input-row";
    inputForm.addEventListener("submit", (e) => {
      e.preventDefault();
      void this.handleSendChat();
    });

    this.micBtnEl = document.createElement("button");
    this.micBtnEl.type = "button";
    this.micBtnEl.className = "mic-btn";
    this.micBtnEl.title = "Voice Input (Click or Hold)";
    this.micBtnEl.innerHTML = ICONS.mic;

    this.micBtnEl.addEventListener("pointerdown", () => {
      this.pointerActive = true;
      this.pointerDownAt = performance.now();
      this.ignoreNextClick = false;
      if (this.voiceMode === "pushToTalk" && !this.isListening) {
        this.startedOnPointerDown = true;
        void this.startMic();
      } else {
        this.startedOnPointerDown = false;
      }
    });
    this.micBtnEl.addEventListener("pointerup", () => {
      const heldMs = performance.now() - this.pointerDownAt;
      const wasHold = this.pointerActive && heldMs > 280;
      this.pointerActive = false;
      if (this.voiceMode === "pushToTalk" && wasHold && this.isListening) {
        this.startedOnPointerDown = false;
        this.ignoreNextClick = true;
        void this.stopMic();
      }
    });
    this.micBtnEl.addEventListener("pointercancel", () => {
      this.pointerActive = false;
      this.startedOnPointerDown = false;
      this.ignoreNextClick = false;
    });
    this.micBtnEl.addEventListener("click", () => {
      if (this.ignoreNextClick) {
        this.ignoreNextClick = false;
        return;
      }
      if (this.startedOnPointerDown) {
        this.startedOnPointerDown = false;
        return;
      }
      if (this.isListening) void this.stopMic();
      else void this.startMic();
    });

    this.chatInputEl = document.createElement("input");
    this.chatInputEl.type = "text";
    this.chatInputEl.className = "chat-input";
    this.chatInputEl.placeholder = "คุยกับมีนา... (Thai or English)";
    this.chatInputEl.autocomplete = "off";

    this.sendBtnEl = document.createElement("button");
    this.sendBtnEl.type = "submit";
    this.sendBtnEl.className = "send-btn";
    this.sendBtnEl.title = "Send Message";
    this.sendBtnEl.innerHTML = ICONS.send;

    inputForm.append(this.micBtnEl, this.chatInputEl, this.sendBtnEl);
    dock.append(metaRow, inputForm);
    return dock;
  }

  private async subscribeEvents(): Promise<void> {
    this.unlisteners.push(
      await listenEvent("chat://token", (chunk) => {
        this.subtitleText = chunk.fullText;
        this.subtitleTextEl.textContent = chunk.fullText;
      }),
      await listenEvent("avatar://emotion", (emotionState) => {
        this.applyEmotion(emotionState);
      }),
      await listenEvent("avatar://viseme", (frame) => {
        this.stage.pushViseme(frame);
        if (frame.energy > 0.04 || frame.mouthOpenY > 0.06) {
          this.markSpeakingActive();
        }
      }),
      await listenEvent("voice://transcript", (evt) => {
        this.transcriptPreview = evt.text;
        this.syncTranscriptLine();
        if (evt.isFinal) {
          this.isListening = false;
          this.startedOnPointerDown = false;
          this.syncMicState();
          const finalText = evt.text.trim();
          if (finalText) {
            this.chatInputEl.value = finalText;
            void this.handleSendChat();
          }
          window.setTimeout(() => {
            this.transcriptPreview = "";
            this.syncTranscriptLine();
          }, 1200);
        }
      }),
      await listenEvent("system://ram", (ram) => {
        this.ramStatus = ram;
        this.syncTopRamLabel();
        this.modelDrawer.updateRam(ram);
      })
    );
  }

  private async hydrateInitialState(): Promise<void> {
    try {
      const [ram, rel, live2dModels] = await Promise.all([
        invokeCommand("get_ram_status"),
        invokeCommand("get_relationship_state"),
        invokeCommand("list_live2d_models"),
      ]);
      this.ramStatus = ram;
      this.relationship = rel;
      this.syncTopRamLabel();
      this.syncBondMeter();

      const activeLive2d =
        live2dModels.find((m) => m.isActive) ?? live2dModels[0] ?? null;
      if (activeLive2d) {
        void this.stage.loadLive2dModelInfo(activeLive2d, false);
      } else {
        void this.stage.loadDefaultLive2dAsset(false);
      }
    } catch (err) {
      void this.stage.loadDefaultLive2dAsset(false);
      this.showToast(formatSoulError(err), "error");
    }
  }

  private markSpeakingActive(): void {
    this.isSpeaking = true;
    this.stopSpeakBtnEl.classList.remove("hidden");
    if (this.speakingDecayTimer !== null) {
      window.clearTimeout(this.speakingDecayTimer);
    }
    this.speakingDecayTimer = window.setTimeout(() => {
      this.isSpeaking = false;
      this.stopSpeakBtnEl.classList.add("hidden");
      this.speakingDecayTimer = null;
    }, 320);
  }

  private applyEmotion(state: EmotionState): void {
    this.currentEmotion = state;
    this.stage.setEmotion(state);
    this.syncEmotionPill();
  }

  private async handleSendChat(overrideMessage?: string): Promise<void> {
    const raw = (overrideMessage ?? this.chatInputEl.value).trim();
    if (!raw || this.isSending) return;

    this.isSending = true;
    this.sendBtnEl.disabled = true;
    if (!overrideMessage) this.chatInputEl.value = "";

    this.chatLog.push({
      id: `user-${Date.now()}`,
      role: "user",
      content: raw,
      emotion: null,
      timestampMs: Date.now(),
    });
    this.subtitleText = "กำลังคิด...";
    this.subtitleTextEl.textContent = this.subtitleText;
    this.syncHistoryPopover();

    try {
      const reply = await invokeCommand("send_chat_message", { message: raw });
      this.applyEmotion(reply.emotion);
      this.subtitleText = reply.turn.content;
      this.subtitleTextEl.textContent = reply.turn.content;
      this.chatLog.push(reply.turn);
      this.syncHistoryPopover();

      // Refresh bond & memory state quietly
      const rel = await invokeCommand("get_relationship_state");
      this.relationship = rel;
      this.syncBondMeter();
      if (this.memoryDrawer.isOpen()) {
        void this.memoryDrawer.refresh();
      }
    } catch (err) {
      const msg = formatSoulError(err);
      this.subtitleTextEl.textContent = msg;
      this.showToast(msg, "error");
    } finally {
      this.isSending = false;
      this.sendBtnEl.disabled = false;
    }
  }

  private async triggerSpeechPreview(text: string, emotion: EmotionTag): Promise<void> {
    try {
      this.subtitleText = text;
      this.subtitleTextEl.textContent = text;
      await invokeCommand("preview_speech_and_visemes", { text, emotion });
    } catch (err) {
      this.showToast(formatSoulError(err), "error");
    }
  }

  private async handleStopSpeaking(): Promise<void> {
    try {
      await invokeCommand("stop_speaking");
      this.stage.clearViseme();
      this.isSpeaking = false;
      this.stopSpeakBtnEl.classList.add("hidden");
    } catch (err) {
      this.showToast(formatSoulError(err), "error");
    }
  }

  private async handleVoiceModeChange(mode: VoiceMode): Promise<void> {
    try {
      await invokeCommand("set_voice_mode", { mode });
      this.voiceMode = mode;
      if (mode === "muted" && this.isListening) {
        await this.stopMic();
      }
      this.syncMicState();
    } catch (err) {
      this.showToast(formatSoulError(err), "error");
    }
  }

  private async handleLanguageChange(pref: LanguagePref): Promise<void> {
    try {
      await invokeCommand("set_language_pref", { pref });
      this.languagePref = pref;
      this.syncUiState();
    } catch (err) {
      this.showToast(formatSoulError(err), "error");
    }
  }

  private async startMic(): Promise<void> {
    if (this.voiceMode === "muted") {
      this.showToast("Mic is muted — switch to PTT or Hands-Free to speak.", "error");
      return;
    }
    try {
      this.isListening = true;
      this.syncMicState();
      await invokeCommand("start_listening");
    } catch (err) {
      this.isListening = false;
      this.syncMicState();
      this.showToast(formatSoulError(err), "error");
    }
  }

  private async stopMic(): Promise<void> {
    this.startedOnPointerDown = false;
    try {
      await invokeCommand("stop_listening");
    } catch (err) {
      this.showToast(formatSoulError(err), "error");
    } finally {
      this.isListening = false;
      this.syncMicState();
    }
  }

  private async togglePetMode(): Promise<void> {
    try {
      const next = await invokeCommand("set_desktop_pet_mode", { enabled: !this.petMode });
      this.petMode = next;
      document.body.classList.toggle("pet-mode", this.petMode);
      this.stage.setPetMode(this.petMode);
      this.petBtnEl.classList.toggle("active", this.petMode);
      this.showToast(
        this.petMode
          ? "Desktop Pet Mode enabled — drag anywhere above Mina to move"
          : "Normal Studio Window restored"
      );
    } catch (err) {
      this.showToast(formatSoulError(err), "error");
    }
  }

  private syncUiState(): void {
    this.syncEmotionPill();
    this.syncBondMeter();
    this.syncTopRamLabel();
    this.syncEngineBadge();
    this.syncMicState();
    this.syncPopovers();

    for (const [pref, btn] of this.langBtns.entries()) {
      btn.classList.toggle("active", pref === this.languagePref);
    }
  }

  private syncEmotionPill(): void {
    const meta = EMOTION_META[this.currentEmotion.tag];
    this.subtitleEmotionEl.textContent = `${meta.labelEn} · ${meta.labelTh}`;
    this.subtitleEmotionEl.style.background = meta.pillBg;
    this.subtitleEmotionEl.style.color = meta.accentColor;
    this.subtitleEmotionEl.style.borderColor = `${meta.accentColor}44`;

    for (const [tag, btn] of this.emotionChipBtns.entries()) {
      btn.classList.toggle("active", tag === this.currentEmotion.tag);
    }
  }

  private syncBondMeter(): void {
    const avg =
      (this.relationship.trust +
        this.relationship.warmth +
        this.relationship.familiarity +
        this.relationship.moodBaseline) /
      4;
    const pct = Math.round(Math.max(0, Math.min(1, avg)) * 100);
    this.bondPctEl.textContent = `${pct}%`;
    this.bondFillEl.style.opacity = String(0.35 + avg * 0.65);

    this.bondPopoverEl.innerHTML = "";
    const title = document.createElement("div");
    title.className = "popover-title";
    title.textContent = `Bond & Mood (${this.relationship.totalTurns} turns)`;
    this.bondPopoverEl.appendChild(title);

    const rows: Array<[string, number]> = [
      ["Trust", this.relationship.trust],
      ["Warmth", this.relationship.warmth],
      ["Familiarity", this.relationship.familiarity],
      ["Mood Baseline", this.relationship.moodBaseline],
    ];
    for (const [label, val] of rows) {
      const row = document.createElement("div");
      row.className = "popover-meter-row";
      const lbl = document.createElement("span");
      lbl.textContent = label;
      const track = document.createElement("div");
      track.className = "mini-bar-track";
      const fill = document.createElement("div");
      fill.className = "mini-bar-fill";
      fill.style.width = `${Math.round(val * 100)}%`;
      track.appendChild(fill);
      const pctSpan = document.createElement("strong");
      pctSpan.textContent = `${Math.round(val * 100)}%`;
      row.append(lbl, track, pctSpan);
      this.bondPopoverEl.appendChild(row);
    }
  }

  private syncTopRamLabel(): void {
    if (!this.ramStatus) return;
    const gb = (this.ramStatus.processRamMb / 1024).toFixed(1);
    this.topRamLabelEl.textContent = `${gb} GB`;
  }

  private syncEngineBadge(): void {
    const mode = this.stage?.getEngineMode() ?? "mina";
    if (mode === "live2d") {
      this.topEngineBadgeEl.textContent = "Cubism .model3";
    } else {
      this.topEngineBadgeEl.textContent = isTauriRuntime() ? "Mina 60fps" : "Preview";
    }
  }

  private syncMicState(): void {
    for (const [mode, btn] of this.voiceBtns.entries()) {
      btn.classList.toggle("active", mode === this.voiceMode);
    }
    this.micBtnEl.classList.toggle("listening", this.isListening);
    this.micBtnEl.classList.toggle("muted", this.voiceMode === "muted");
    this.stopSpeakBtnEl.classList.toggle("hidden", !this.isSpeaking);
  }

  private syncTranscriptLine(): void {
    if (this.transcriptPreview) {
      this.transcriptLineEl.textContent = `Listening: "${this.transcriptPreview}"`;
      this.transcriptLineEl.classList.remove("hidden");
    } else {
      this.transcriptLineEl.classList.add("hidden");
    }
  }

  private syncPopovers(): void {
    this.testerBarEl.classList.toggle("hidden", !this.testerOpen);
    this.testerBtnEl.classList.toggle("active", this.testerOpen);
    this.bondPopoverEl.classList.toggle("hidden", !this.bondOpen);
    this.historyPopoverEl.classList.toggle("hidden", !this.historyOpen);
    if (this.historyOpen) this.syncHistoryPopover();
  }

  private syncHistoryPopover(): void {
    this.historyPopoverEl.innerHTML = "";
    const head = document.createElement("div");
    head.className = "popover-title";
    head.textContent = "Recent Conversation";
    this.historyPopoverEl.appendChild(head);

    if (this.chatLog.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty-hint";
      empty.textContent = "Send a message in Thai or English to start chatting!";
      this.historyPopoverEl.appendChild(empty);
      return;
    }

    const list = document.createElement("div");
    list.className = "history-list";
    for (const turn of this.chatLog.slice(-10)) {
      const item = document.createElement("div");
      item.className = `history-item ${turn.role}`;
      const who = document.createElement("span");
      who.className = "history-who";
      who.textContent = turn.role === "user" ? "You" : "Mina";
      const txt = document.createElement("span");
      txt.textContent = turn.content;
      item.append(who, txt);
      list.appendChild(item);
    }
    this.historyPopoverEl.appendChild(list);
  }

  private showToast(message: string, kind: "info" | "error" = "info"): void {
    const id = ++this.toastCounter;
    const item: ToastItem = { id, message, kind };

    // Keep toast stack lean (at most 1 active toast) so it never covers drawer content
    this.toastStackEl.innerHTML = "";

    const el = document.createElement("div");
    el.className = `toast ${item.kind === "error" ? "toast-error" : "toast-info"}`;
    el.textContent = item.message;
    this.toastStackEl.appendChild(el);

    window.setTimeout(() => {
      el.classList.add("fade-out");
      window.setTimeout(() => el.remove(), 220);
    }, 2600);
  }

  destroy(): void {
    window.removeEventListener("keydown", this.handleGlobalKeydown);
    for (const unlisten of this.unlisteners) {
      unlisten();
    }
    this.unlisteners = [];
    this.stage?.destroy();
  }
}
