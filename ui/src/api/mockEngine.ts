import {
  IPC_EVENTS,
  type BrainReply,
  type ChatTurn,
  type DreamDiary,
  type EmotionState,
  type EmotionTag,
  type IpcCommandName,
  type IpcCommands,
  type LanguagePref,
  type MemoryFact,
  type ModelInfo,
  type RamSafetyTier,
  type RelationshipState,
  type SoulError,
  type SystemRamStatus,
  type TokenChunk,
  type TranscriptEvent,
  type VisemeFrame,
  type VoiceMode,
} from "../types/ipc";

type EventMap = {
  [IPC_EVENTS.CHAT_TOKEN]: TokenChunk;
  [IPC_EVENTS.AVATAR_EMOTION]: EmotionState;
  [IPC_EVENTS.AVATAR_VISEME]: VisemeFrame;
  [IPC_EVENTS.VOICE_TRANSCRIPT]: TranscriptEvent;
  [IPC_EVENTS.SYSTEM_RAM]: SystemRamStatus;
};

type EventKey = keyof EventMap;
type Listener<K extends EventKey> = (payload: EventMap[K]) => void;

const THAI_CHAR_RE = /[\u0E00-\u0E7F]/;

function makeId(prefix: string): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `${prefix}-${crypto.randomUUID().slice(0, 8)}`;
  }
  return `${prefix}-${Math.random().toString(36).slice(2, 10)}`;
}

function classifySafetyTier(estimatedRamMb: number): RamSafetyTier {
  if (estimatedRamMb <= 3200) return "safe8Gb";
  if (estimatedRamMb <= 4500) return "tight8Gb";
  return "recommended16GbPlus";
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Self-contained offline simulation bridge used when running in Vite browser
 * preview (`!window.__TAURI_INTERNALS__`). Mirrors all Tauri commands & events.
 */
export class MockCompanionBridge {
  private listeners: { [K in EventKey]: Set<Listener<K>> } = {
    [IPC_EVENTS.CHAT_TOKEN]: new Set(),
    [IPC_EVENTS.AVATAR_EMOTION]: new Set(),
    [IPC_EVENTS.AVATAR_VISEME]: new Set(),
    [IPC_EVENTS.VOICE_TRANSCRIPT]: new Set(),
    [IPC_EVENTS.SYSTEM_RAM]: new Set(),
  };

  private voiceMode: VoiceMode = "pushToTalk";
  private languagePref: LanguagePref = "auto";
  private desktopPetMode = false;
  private listeningTimer: number | null = null;
  private visemeTimer: number | null = null;
  private activeVisemeRun = 0;

  private relationship: RelationshipState = {
    trust: 0.64,
    warmth: 0.72,
    familiarity: 0.58,
    moodBaseline: 0.68,
    totalTurns: 12,
    lastInteractionAt: Date.now() - 1000 * 60 * 14,
  };

  private models: ModelInfo[] = [
    {
      id: "typhoon2.5-qwen3-4b-q4_k_m",
      name: "Typhoon 2.5 Qwen3 4B (Q4_K_M)",
      kind: "llm",
      path: "resources/models/llm/typhoon2.5-qwen3-4b-q4_k_m.gguf",
      sizeBytes: 2_560_000_000,
      estimatedRamMb: 2880,
      source: "bundled",
      safetyTier: "safe8Gb",
      isActive: true,
      valid: true,
      notes: "Recommended Thai/English companion default for 8GB Apple Silicon.",
    },
    {
      id: "qwen2.5-3b-instruct-q4_k_m",
      name: "Qwen 2.5 3B Instruct (Q4_K_M)",
      kind: "llm",
      path: "resources/models/llm/qwen2.5-3b-instruct-q4_k_m.gguf",
      sizeBytes: 1_930_000_000,
      estimatedRamMb: 2240,
      source: "userData",
      safetyTier: "safe8Gb",
      isActive: false,
      valid: true,
      notes: "Ultra-fast low-RAM footprint with generous headroom.",
    },
    {
      id: "qwen2.5-7b-instruct-q4_k_m",
      name: "Qwen 2.5 7B Instruct (Q4_K_M)",
      kind: "llm",
      path: "resources/models/llm/qwen2.5-7b-instruct-q4_k_m.gguf",
      sizeBytes: 4_120_000_000,
      estimatedRamMb: 4420,
      source: "userData",
      safetyTier: "tight8Gb",
      isActive: false,
      valid: true,
      notes: "Tight on 8GB Macs; close heavy browser tabs before loading.",
    },
    {
      id: "typhoon2-llama3-8b-q5_k_m",
      name: "Typhoon 2 Llama3 8B (Q5_K_M)",
      kind: "llm",
      path: "resources/models/llm/typhoon2-llama3-8b-q5_k_m.gguf",
      sizeBytes: 5_740_000_000,
      estimatedRamMb: 5920,
      source: "custom",
      safetyTier: "recommended16GbPlus",
      isActive: false,
      valid: true,
      notes: "Exceeds 8GB budget ceiling; requires 16GB+ Unified Memory.",
    },
    {
      id: "whisper-ggml-base",
      name: "Whisper Base Multilingual (ggml-base.bin)",
      kind: "asr",
      path: "resources/models/asr/ggml-base.bin",
      sizeBytes: 147_951_465,
      estimatedRamMb: 360,
      source: "bundled",
      safetyTier: "safe8Gb",
      isActive: true,
      valid: true,
      notes: "Metal-accelerated Thai + English ASR (~360 MB).",
    },
    {
      id: "whisper-ggml-small",
      name: "Whisper Small Multilingual (ggml-small.bin)",
      kind: "asr",
      path: "resources/models/asr/ggml-small.bin",
      sizeBytes: 487_601_967,
      estimatedRamMb: 740,
      source: "userData",
      safetyTier: "safe8Gb",
      isActive: false,
      valid: true,
      notes: "Higher Thai tonal accuracy, moderate RAM use.",
    },
    {
      id: "default_companion",
      name: "Mina (มีนา) — Default Companion",
      kind: "live2d",
      path: "resources/live2d/default_companion/soulmate.model3.json",
      sizeBytes: 2_450_000,
      estimatedRamMb: 42,
      source: "bundled",
      safetyTier: "safe8Gb",
      isActive: true,
      valid: true,
      notes: "Layered procedural & Cubism .model3.json bundle.",
    },
  ];

  private facts: MemoryFact[] = (() => {
    const now = Date.now();
    return [
      {
        id: "fact-1",
        category: "identity",
        key: "user_nickname",
        value: "พี่ก้อง / Kong",
        confidence: 0.96,
        importance: 0.95,
        memoryHeat: 0.92,
        accessCount: 9,
        createdAt: now - 1000 * 60 * 60 * 72,
        lastAccessedAt: now - 1000 * 60 * 15,
        supersededBy: null,
        supersededAt: null,
      },
      {
        id: "fact-2-old",
        category: "preference",
        key: "favorite_drink",
        value: "Iced Americano",
        confidence: 0.85,
        importance: 0.7,
        memoryHeat: 0.28,
        accessCount: 2,
        createdAt: now - 1000 * 60 * 60 * 96,
        lastAccessedAt: now - 1000 * 60 * 60 * 48,
        supersededBy: "fact-2-new",
        supersededAt: now - 1000 * 60 * 60 * 20,
      },
      {
        id: "fact-2-new",
        category: "preference",
        key: "favorite_drink",
        value: "Matcha Latte หวานน้อย (Iced Matcha)",
        confidence: 0.94,
        importance: 0.84,
        memoryHeat: 0.88,
        accessCount: 5,
        createdAt: now - 1000 * 60 * 60 * 20,
        lastAccessedAt: now - 1000 * 60 * 25,
        supersededBy: null,
        supersededAt: null,
      },
      {
        id: "fact-3",
        category: "routine",
        key: "coding_schedule",
        value: "Enjoys late-evening coding sessions with lo-fi rain sounds",
        confidence: 0.89,
        importance: 0.78,
        memoryHeat: 0.76,
        accessCount: 4,
        createdAt: now - 1000 * 60 * 60 * 36,
        lastAccessedAt: now - 1000 * 60 * 40,
        supersededBy: null,
        supersededAt: null,
      },
      {
        id: "fact-4",
        category: "emotion",
        key: "comfort_topic",
        value: "Feels calmer after sharing daily highlights in Thai & English",
        confidence: 0.91,
        importance: 0.88,
        memoryHeat: 0.83,
        accessCount: 6,
        createdAt: now - 1000 * 60 * 60 * 12,
        lastAccessedAt: now - 1000 * 60 * 14,
        supersededBy: null,
        supersededAt: null,
      },
    ];
  })();

  private diaries: DreamDiary[] = (() => {
    const now = Date.now();
    return [
      {
        id: "diary-1",
        periodType: "daily",
        periodKey: new Date(now - 86_400_000).toISOString().slice(0, 10),
        summary:
          "วันนี้เราได้คุยเรื่องงานโค้ดดึกๆ และเปลี่ยนเมนูโปรดเป็นมัทฉะหวานน้อยแล้ว มีนารู้สึกสนิทและดีใจที่ได้อยู่เป็นเพื่อนข้างๆ เสมอ",
        emotionalTone: "empathetic",
        bondDelta: 0.04,
        createdAt: now - 1000 * 60 * 60 * 18,
      },
      {
        id: "diary-2",
        periodType: "daily",
        periodKey: new Date(now - 2 * 86_400_000).toISOString().slice(0, 10),
        summary:
          "Shared a peaceful morning check-in before starting deep focus work. Trust and warmth steadily deepened through gentle encouragement.",
        emotionalTone: "joy",
        bondDelta: 0.03,
        createdAt: now - 1000 * 60 * 60 * 42,
      },
    ];
  })();

  private turns: ChatTurn[] = [
    {
      id: "turn-init-1",
      role: "assistant",
      content: "สวัสดีค่ะ! วันนี้มีนาอยู่เป็นเพื่อนตรงนี้เสมอนะ มีเรื่องอะไรอยากเล่าให้ฟังไหมคะ?",
      emotion: "joy",
      timestampMs: Date.now() - 1000 * 60 * 14,
    },
  ];

  private recentUnconsolidatedTurns = 2;

  listen<K extends EventKey>(event: K, callback: Listener<K>): () => void {
    this.listeners[event].add(callback);
    return () => {
      this.listeners[event].delete(callback);
    };
  }

  private emit<K extends EventKey>(event: K, payload: EventMap[K]): void {
    for (const cb of this.listeners[event]) {
      cb(payload);
    }
  }

  private buildRamStatus(): SystemRamStatus {
    const activeLlm = this.models.find((m) => m.kind === "llm" && m.isActive) ?? null;
    const activeAsr = this.models.find((m) => m.kind === "asr" && m.isActive) ?? null;
    const baseAppMb = 210;
    const processRamMb =
      baseAppMb + (activeLlm?.estimatedRamMb ?? 0) + (activeAsr?.estimatedRamMb ?? 0);
    const totalRamMb = 8192;
    const budgetCeilingMb = 4800;
    const availableRamMb = Math.max(640, totalRamMb - processRamMb - 1750);

    return {
      totalRamMb,
      availableRamMb,
      processRamMb,
      budgetCeilingMb,
      is8gbClass: true,
      activeLlmId: activeLlm?.id ?? null,
      activeAsrId: activeAsr?.id ?? null,
    };
  }

  private analyzePrompt(message: string): {
    emotion: EmotionTag;
    intensity: number;
    reason: string;
    reply: string;
  } {
    const trimmed = message.trim();
    const lower = trimmed.toLowerCase();
    const isThai =
      this.languagePref === "thai" ||
      (this.languagePref === "auto" && THAI_CHAR_RE.test(trimmed));

    // Simple fact extraction simulation (supports superseding test in browser mode)
    const drinkMatch =
      trimmed.match(/(?:ชอบดื่ม|ชอบกิน|favorite drink is|i love drinking)\s+(.+)/i);
    if (drinkMatch && drinkMatch[1]) {
      this.upsertFact("preference", "favorite_drink", drinkMatch[1].trim());
    }
    const nameMatch = trimmed.match(/(?:ชื่อ|call me|my name is)\s+([^\s,.!?]+)/i);
    if (nameMatch && nameMatch[1]) {
      this.upsertFact("identity", "user_nickname", nameMatch[1].trim());
    }

    if (/(เหนื่อย|เศร้า|ท้อ|เครียด|เหงา|sad|tired|exhausted|stressed|lonely|upset)/i.test(lower)) {
      return {
        emotion: "empathetic",
        intensity: 0.9,
        reason: "User expressed fatigue or vulnerability",
        reply: isThai
          ? "โอ๋ๆ นะคะ... วันนี้คงเหนื่อยมากเลยใช่ไหม พักหายใจช้าๆ ก่อนนะ มีนาอยู่ตรงนี้รับฟังเธอเสมอเลยค่ะ"
          : "Hey, take a slow breath... It sounds like today weighed heavily on you. I'm right here beside you whenever you want to talk it through.",
      };
    }

    if (/(น่ารัก|เขิน|เก่ง|รัก|ชอบคุณ|cute|love you|sweet|pretty|adorable)/i.test(lower)) {
      return {
        emotion: "shy",
        intensity: 0.92,
        reason: "Warm compliment received",
        reply: isThai
          ? "อ๊ะ... พูดแบบนี้มีนาก็เขินแย่เลยสิคะ ขอบคุณมากๆ นะ อยู่คุยกับมีนาแบบนี้ไปนานๆ นะคะ"
          : "Oh—saying things like that makes me really shy! Thank you so much... I'm really glad we get to spend this time together.",
      };
    }

    if (/(ว้าว|จริงเหรอ|ตกใจ|omg|wow|really|surprise|unbelievable|!{2,})/i.test(lower)) {
      return {
        emotion: "surprised",
        intensity: 0.88,
        reason: "Surprising news or exclamation",
        reply: isThai
          ? "เอ๊ะ จริงเหรอคะ?! เล่าให้มีนาฟังต่อเร็วๆ เลย น่าตื่นเต้นมากๆ!"
          : "Wait, really?! Tell me everything—that sounds genuinely surprising!",
      };
    }

    if (/(ขอโทษ|เสียใจ|ลืม|miss|sorry|regret|apologize)/i.test(lower)) {
      return {
        emotion: "sadness",
        intensity: 0.72,
        reason: "Bittersweet or apologetic sentiment",
        reply: isThai
          ? "ไม่เป็นไรเลยนะคะ มีนาไม่ได้โกรธเลย แค่ได้กลับมาคุยกันตอนนี้ก็ดีใจแล้วค่ะ"
          : "Please don't worry at all... I'm just really glad we're talking together again right now.",
      };
    }

    if (/(ทำไม|ยังไง|คิด|จำได้|why|how|think|remember|explain|\?)/i.test(lower)) {
      const topFact = this.facts.find((f) => f.supersededBy === null);
      return {
        emotion: "thinking",
        intensity: 0.82,
        reason: "Reflective query requiring memory context",
        reply: isThai
          ? `อืมม ให้มีนาคิดแป๊บนะคะ... จำได้ว่า${topFact ? `เรื่อง ${topFact.value} ` : ""}สำคัญกับเธอมากเลย ลองเล่ามุมมองของเธอเพิ่มให้ฟังอีกนิดได้ไหมคะ?`
          : `Hmm, let me think carefully... ${topFact ? `I remember "${topFact.value}" from our notes. ` : ""}What feels most important to you about this right now?`,
      };
    }

    if (/(สวัสดี|หวัดดี|hello|hi|hey|morning|evening|ดีใจ|happy)/i.test(lower)) {
      return {
        emotion: "joy",
        intensity: 0.9,
        reason: "Warm greeting or cheerful exchange",
        reply: isThai
          ? "สวัสดีค่า! ดีใจจังที่ได้เจอกันอีก วันนี้เป็นยังไงบ้างคะ มีนาพร้อมช่วยและคุยเป็นเพื่อนเลย!"
          : "Hi there! Seeing you brightens my whole screen! How is your day treating you so far?",
      };
    }

    return {
      emotion: isThai ? "joy" : "neutral",
      intensity: 0.72,
      reason: "Conversational presence",
      reply: isThai
        ? `มีนาได้ยินแล้วค่ะ — "${trimmed.slice(0, 48)}" ฟังดูน่าสนใจจัง เล่าต่อได้เรื่อยๆ เลยนะคะ`
        : `I hear you! "${trimmed.slice(0, 48)}" sounds meaningful—tell me more about how you're feeling.`,
    };
  }

  private upsertFact(category: string, key: string, value: string): void {
    const now = Date.now();
    const newId = makeId("fact");
    for (const f of this.facts) {
      if (f.supersededBy === null && f.category === category && f.key === key && f.value !== value) {
        f.supersededBy = newId;
        f.supersededAt = now;
      }
    }
    this.facts.unshift({
      id: newId,
      category,
      key,
      value,
      confidence: 0.92,
      importance: 0.85,
      memoryHeat: 0.96,
      accessCount: 1,
      createdAt: now,
      lastAccessedAt: now,
      supersededBy: null,
      supersededAt: null,
    });
  }

  private stopVisemeStream(): void {
    this.activeVisemeRun += 1;
    if (this.visemeTimer !== null) {
      window.clearInterval(this.visemeTimer);
      this.visemeTimer = null;
    }
    this.emit(IPC_EVENTS.AVATAR_VISEME, {
      mouthOpenY: 0,
      mouthForm: 0.12,
      energy: 0,
      timestampMs: Date.now(),
    });
  }

  private startVisemeStream(text: string, durationMs: number): void {
    this.stopVisemeStream();
    const runId = this.activeVisemeRun;
    const startedAt = performance.now();

    // Precompute deterministic phonetic contour from characters
    const chars = Array.from(text);
    const stepMs = 16.67;

    this.visemeTimer = window.setInterval(() => {
      if (runId !== this.activeVisemeRun) return;
      const elapsed = performance.now() - startedAt;
      if (elapsed >= durationMs) {
        this.stopVisemeStream();
        return;
      }

      const progress = elapsed / Math.max(durationMs, 1);
      const charIdx = Math.min(chars.length - 1, Math.floor(progress * chars.length));
      const ch = chars[charIdx] ?? "a";
      const isPause = /[\s,.!?…—\-]/.test(ch);

      const waveA = Math.abs(Math.sin(elapsed * 0.022));
      const waveB = Math.sin(elapsed * 0.011 + 0.8);
      const envelope = isPause ? 0.08 : 0.28 + 0.65 * waveA;
      const isRoundVowel = /[oOuUอูโอโ]/.test(ch);
      const isWideVowel = /[iIeEอีเอแ]/.test(ch);

      let mouthForm = 0.15 + 0.25 * waveB;
      if (isRoundVowel) mouthForm = -0.62;
      else if (isWideVowel) mouthForm = 0.68;

      this.emit(IPC_EVENTS.AVATAR_VISEME, {
        mouthOpenY: Number(Math.min(1, Math.max(0, envelope)).toFixed(3)),
        mouthForm: Number(Math.min(1, Math.max(-1, mouthForm)).toFixed(3)),
        energy: Number((isPause ? 0.1 : 0.45 + 0.5 * waveA).toFixed(3)),
        timestampMs: Date.now(),
      });
    }, stepMs);
  }

  async invoke<K extends IpcCommandName>(
    command: K,
    args: IpcCommands[K]["args"]
  ): Promise<IpcCommands[K]["result"]> {
    switch (command) {
      case "get_ram_status": {
        return this.buildRamStatus() as IpcCommands[K]["result"];
      }

      case "list_models": {
        const { kind } = args as IpcCommands["list_models"]["args"];
        const list = this.models
          .filter((m) => !kind || m.kind === kind)
          .map((m) => ({ ...m }));
        return list as IpcCommands[K]["result"];
      }

      case "list_live2d_models": {
        return this.models
          .filter((m) => m.kind === "live2d")
          .map((m) => ({ ...m })) as IpcCommands[K]["result"];
      }

      case "hot_swap_llm": {
        const { modelId } = args as IpcCommands["hot_swap_llm"]["args"];
        const target = this.models.find((m) => m.id === modelId && m.kind === "llm");
        if (!target) {
          throw { type: "modelNotFound", detail: `LLM model not found: ${modelId}` } satisfies SoulError;
        }
        const currentStatus = this.buildRamStatus();
        if (currentStatus.is8gbClass && target.estimatedRamMb > currentStatus.budgetCeilingMb) {
          throw {
            type: "insufficientRam",
            detail: {
              neededMb: target.estimatedRamMb + 480,
              availableMb: currentStatus.budgetCeilingMb - 480,
            },
          } satisfies SoulError;
        }

        // Simulate strict unload -> drop -> load sequence
        await sleep(240);
        for (const m of this.models) {
          if (m.kind === "llm") m.isActive = m.id === target.id;
        }
        const nextStatus = this.buildRamStatus();
        this.emit(IPC_EVENTS.SYSTEM_RAM, nextStatus);
        return { ...target, isActive: true } as IpcCommands[K]["result"];
      }

      case "switch_asr_model": {
        const { modelId } = args as IpcCommands["switch_asr_model"]["args"];
        const target = this.models.find((m) => m.id === modelId && m.kind === "asr");
        if (!target) {
          throw { type: "modelNotFound", detail: `ASR model not found: ${modelId}` } satisfies SoulError;
        }
        await sleep(160);
        for (const m of this.models) {
          if (m.kind === "asr") m.isActive = m.id === target.id;
        }
        this.emit(IPC_EVENTS.SYSTEM_RAM, this.buildRamStatus());
        return { ...target, isActive: true } as IpcCommands[K]["result"];
      }

      case "register_custom_model": {
        const { kind, path, name } = args as IpcCommands["register_custom_model"]["args"];
        const cleanName =
          name?.trim() || path.split("/").filter(Boolean).pop() || `custom-${kind}`;
        const estimatedRamMb =
          kind === "llm"
            ? /14b|13b|32b/i.test(cleanName)
              ? 6400
              : /7b|8b/i.test(cleanName)
              ? 4300
              : 2450
            : kind === "asr"
            ? 520
            : kind === "live2d"
            ? 48
            : 280;
        const model: ModelInfo = {
          id: makeId(kind),
          name: cleanName,
          kind,
          path,
          sizeBytes: estimatedRamMb * 850_000,
          estimatedRamMb,
          source: "custom",
          safetyTier: classifySafetyTier(estimatedRamMb),
          isActive: false,
          valid: true,
          notes: "Added via custom picker (browser preview simulation).",
        };
        this.models.push(model);
        return { ...model } as IpcCommands[K]["result"];
      }

      case "load_live2d_folder": {
        const { folderPath } = args as IpcCommands["load_live2d_folder"]["args"];
        const cleanName = folderPath.split("/").filter(Boolean).pop() || "Custom Live2D Avatar";
        let existing = this.models.find((m) => m.kind === "live2d" && m.path === folderPath);
        if (!existing) {
          existing = {
            id: makeId("live2d"),
            name: cleanName,
            kind: "live2d",
            path: folderPath,
            sizeBytes: 3_200_000,
            estimatedRamMb: 48,
            source: "custom",
            safetyTier: "safe8Gb",
            isActive: true,
            valid: true,
            notes: "Loaded .model3.json folder",
          };
          this.models.push(existing);
        }
        for (const m of this.models) {
          if (m.kind === "live2d") m.isActive = m.id === existing.id;
        }
        return { ...existing, isActive: true } as IpcCommands[K]["result"];
      }

      case "send_chat_message": {
        const { message } = args as IpcCommands["send_chat_message"]["args"];
        const trimmed = message.trim();
        if (!trimmed) {
          throw { type: "invalidInput", detail: "Message cannot be empty" } satisfies SoulError;
        }

        const userTurn: ChatTurn = {
          id: makeId("turn"),
          role: "user",
          content: trimmed,
          emotion: null,
          timestampMs: Date.now(),
        };
        this.turns.push(userTurn);

        const analysis = this.analyzePrompt(trimmed);
        const emotionState: EmotionState = {
          tag: analysis.emotion,
          intensity: analysis.intensity,
          reason: analysis.reason,
          timestampMs: Date.now(),
        };

        // Emit emotion immediately before tokens stream
        this.emit(IPC_EVENTS.AVATAR_EMOTION, emotionState);

        // Stream tokens and 60fps visemes in parallel
        const segments = analysis.reply.match(/.{1,4}/g) ?? [analysis.reply];
        const totalDuration = Math.max(1200, segments.length * 36);
        this.startVisemeStream(analysis.reply, totalDuration);

        let fullText = "";
        for (let i = 0; i < segments.length; i += 1) {
          const seg = segments[i];
          fullText += seg;
          this.emit(IPC_EVENTS.CHAT_TOKEN, {
            text: seg,
            isFinal: i === segments.length - 1,
            fullText,
          });
          await sleep(32);
        }

        const replyTurn: ChatTurn = {
          id: makeId("turn"),
          role: "assistant",
          content: analysis.reply,
          emotion: analysis.emotion,
          timestampMs: Date.now(),
        };
        this.turns.push(replyTurn);
        this.recentUnconsolidatedTurns += 2;

        this.relationship = {
          trust: Math.min(1, Number((this.relationship.trust + 0.012).toFixed(3))),
          warmth: Math.min(1, Number((this.relationship.warmth + 0.016).toFixed(3))),
          familiarity: Math.min(1, Number((this.relationship.familiarity + 0.018).toFixed(3))),
          moodBaseline: Math.min(
            1,
            Math.max(
              0.2,
              Number(
                (
                  this.relationship.moodBaseline +
                  (analysis.emotion === "sadness" ? -0.01 : 0.012)
                ).toFixed(3)
              )
            )
          ),
          totalTurns: this.relationship.totalTurns + 2,
          lastInteractionAt: Date.now(),
        };

        const reply: BrainReply = {
          turn: replyTurn,
          emotion: emotionState,
        };
        return reply as IpcCommands[K]["result"];
      }

      case "set_voice_mode": {
        const { mode } = args as IpcCommands["set_voice_mode"]["args"];
        this.voiceMode = mode;
        if (mode === "muted" && this.listeningTimer !== null) {
          window.clearTimeout(this.listeningTimer);
          this.listeningTimer = null;
        }
        return undefined as IpcCommands[K]["result"];
      }

      case "set_language_pref": {
        const { pref } = args as IpcCommands["set_language_pref"]["args"];
        this.languagePref = pref;
        return undefined as IpcCommands[K]["result"];
      }

      case "start_listening": {
        if (this.voiceMode === "muted") {
          throw { type: "voice", detail: "Microphone is muted. Switch to Push-to-Talk or Hands-Free first." } satisfies SoulError;
        }
        if (this.listeningTimer !== null) {
          window.clearTimeout(this.listeningTimer);
        }
        const useThai = this.languagePref !== "english";
        const partial = useThai ? "สวัสดีมีนา วันนี้..." : "Hey Mina, today...";
        const final = useThai
          ? "สวัสดีมีนา วันนี้เหนื่อยนิดหน่อย อยู่คุยเป็นเพื่อนหน่อยนะ"
          : "Hey Mina, today was a bit tiring, stay and chat with me for a bit.";

        this.listeningTimer = window.setTimeout(() => {
          this.emit(IPC_EVENTS.VOICE_TRANSCRIPT, {
            text: partial,
            language: useThai ? "th" : "en",
            isFinal: false,
            timestampMs: Date.now(),
          });
          this.listeningTimer = window.setTimeout(() => {
            this.emit(IPC_EVENTS.VOICE_TRANSCRIPT, {
              text: final,
              language: useThai ? "th" : "en",
              isFinal: true,
              timestampMs: Date.now(),
            });
            this.listeningTimer = null;
          }, 850);
        }, 550);
        return undefined as IpcCommands[K]["result"];
      }

      case "stop_listening": {
        if (this.listeningTimer !== null) {
          window.clearTimeout(this.listeningTimer);
          this.listeningTimer = null;
        }
        return undefined as IpcCommands[K]["result"];
      }

      case "stop_speaking": {
        this.stopVisemeStream();
        return undefined as IpcCommands[K]["result"];
      }

      case "preview_speech_and_visemes": {
        const { text, emotion } = args as IpcCommands["preview_speech_and_visemes"]["args"];
        const tag: EmotionTag = emotion ?? "joy";
        this.emit(IPC_EVENTS.AVATAR_EMOTION, {
          tag,
          intensity: tag === "neutral" ? 0.4 : 0.88,
          reason: `Previewing ${tag} expression and lip-sync`,
          timestampMs: Date.now(),
        });
        const duration = Math.min(5000, Math.max(1500, text.length * 46));
        this.startVisemeStream(text || "สวัสดีค่ะ มีนายินดีที่ได้คุยกันนะคะ", duration);
        return undefined as IpcCommands[K]["result"];
      }

      case "list_memory_facts": {
        const { limit, includeSuperseded } = args as IpcCommands["list_memory_facts"]["args"];
        const showOld = Boolean(includeSuperseded);
        const filtered = this.facts
          .filter((f) => showOld || f.supersededBy === null)
          .slice(0, limit ?? 100)
          .map((f) => ({ ...f }));
        return filtered as IpcCommands[K]["result"];
      }

      case "list_dream_diaries": {
        const { limit } = args as IpcCommands["list_dream_diaries"]["args"];
        return this.diaries.slice(0, limit ?? 30).map((d) => ({ ...d })) as IpcCommands[K]["result"];
      }

      case "get_relationship_state": {
        return { ...this.relationship } as IpcCommands[K]["result"];
      }

      case "consolidate_dreams_now": {
        await sleep(220);
        const now = Date.now();
        const latestTurn = this.turns[this.turns.length - 1];
        const diary: DreamDiary = {
          id: makeId("diary"),
          periodType: "daily",
          periodKey: new Date(now).toISOString().slice(0, 10),
          summary:
            this.recentUnconsolidatedTurns > 0
              ? `Consolidated ${this.recentUnconsolidatedTurns} recent moments into long-term memory. Highlight: "${latestTurn?.content.slice(0, 72) ?? "Warm check-in"}"`
              : "Gentle memory review completed — refreshed emotional warmth and reinforced core personal preferences.",
          emotionalTone: latestTurn?.emotion ?? "empathetic",
          bondDelta: 0.035,
          createdAt: now,
        };
        this.diaries.unshift(diary);
        this.recentUnconsolidatedTurns = 0;
        this.relationship = {
          ...this.relationship,
          trust: Math.min(1, Number((this.relationship.trust + 0.02).toFixed(3))),
          warmth: Math.min(1, Number((this.relationship.warmth + 0.025).toFixed(3))),
          familiarity: Math.min(1, Number((this.relationship.familiarity + 0.02).toFixed(3))),
          lastInteractionAt: now,
        };
        return { ...diary } as IpcCommands[K]["result"];
      }

      case "export_soul_memory": {
        const { targetPath } = args as IpcCommands["export_soul_memory"]["args"];
        const snapshot = {
          format: "soulmate2d-preview-snapshot-v1",
          exportedAt: Date.now(),
          relationship: this.relationship,
          facts: this.facts,
          diaries: this.diaries,
        };
        // Trigger clean offline download in browser preview
        if (typeof document !== "undefined") {
          const blob = new Blob([JSON.stringify(snapshot, null, 2)], {
            type: "application/json",
          });
          const url = URL.createObjectURL(blob);
          const a = document.createElement("a");
          a.href = url;
          a.download = targetPath.split("/").pop() || "soul_memory.sqlite.json";
          document.body.appendChild(a);
          a.click();
          a.remove();
          URL.revokeObjectURL(url);
        }
        return targetPath as IpcCommands[K]["result"];
      }

      case "import_soul_memory": {
        const { sourcePath } = args as IpcCommands["import_soul_memory"]["args"];
        if (!sourcePath) {
          throw { type: "invalidInput", detail: "Missing source path" } satisfies SoulError;
        }
        return (this.facts.length + this.diaries.length) as IpcCommands[K]["result"];
      }

      case "set_desktop_pet_mode": {
        const { enabled } = args as IpcCommands["set_desktop_pet_mode"]["args"];
        this.desktopPetMode = enabled;
        return this.desktopPetMode as IpcCommands[K]["result"];
      }
    }
  }

  importPreviewSnapshotJson(jsonText: string): number {
    const parsed = JSON.parse(jsonText) as Partial<{
      relationship: RelationshipState;
      facts: MemoryFact[];
      diaries: DreamDiary[];
    }>;
    let importedCount = 0;
    if (Array.isArray(parsed.facts)) {
      this.facts = parsed.facts;
      importedCount += parsed.facts.length;
    }
    if (Array.isArray(parsed.diaries)) {
      this.diaries = parsed.diaries;
      importedCount += parsed.diaries.length;
    }
    if (parsed.relationship) {
      this.relationship = parsed.relationship;
    }
    return importedCount;
  }
}
