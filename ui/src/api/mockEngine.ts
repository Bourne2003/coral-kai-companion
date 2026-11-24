import {
  IPC_EVENTS,
  type ActionResult,
  type AvatarLogicalState,
  type AvatarSnapshot,
  type BrainReply,
  type CharacterId,
  type ChatTurn,
  type DiaryEntry,
  type DreamDiary,
  type EmotionDecayEntry,
  type EmotionProfileState,
  type EmotionState,
  type EmotionTag,
  type InnerLoopBeatKind,
  type InnerLoopStatus,
  type IpcCommandName,
  type IpcCommands,
  type LanguagePref,
  type McpConfig,
  type McpToolInfo,
  type MemoryFact,
  type ModelInfo,
  type PersonaBio,
  type PluginInfo,
  type ProactiveMessageEvent,
  type RamSafetyTier,
  type RelationshipState,
  type SoulError,
  type SystemRamStatus,
  type TokenChunk,
  type TranscriptEvent,
  type UserProfile,
  type VisemeFrame,
  type VoiceMode,
} from "../types/ipc";

type EventMap = {
  [IPC_EVENTS.CHAT_TOKEN]: TokenChunk;
  [IPC_EVENTS.AVATAR_EMOTION]: EmotionState;
  [IPC_EVENTS.AVATAR_VISEME]: VisemeFrame;
  [IPC_EVENTS.AVATAR_STATE]: AvatarSnapshot;
  [IPC_EVENTS.VOICE_TRANSCRIPT]: TranscriptEvent;
  [IPC_EVENTS.SYSTEM_RAM]: SystemRamStatus;
  [IPC_EVENTS.PROACTIVE_MESSAGE]: ProactiveMessageEvent;
  [IPC_EVENTS.INNER_LOOP_STATUS]: InnerLoopStatus;
};

type EventKey = keyof EventMap;
type Listener<K extends EventKey> = (payload: EventMap[K]) => void;

const THAI_CHAR_RE = /[\u0E00-\u0E7F]/;

const ALL_EMOTION_TAGS: EmotionTag[] = [
  "joy",
  "sadness",
  "shy",
  "surprised",
  "thinking",
  "empathetic",
  "neutral",
];

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

function clamp01(v: number): number {
  return Math.max(0, Math.min(1, v));
}

function buildInitialEmotions(
  character: CharacterId,
  now: number
): EmotionDecayEntry[] {
  const isKai = character === "kai";
  const baselines: Record<EmotionTag, { intensity: number; baseline: number; halfLife: number }> = {
    joy: { intensity: isKai ? 0.56 : 0.72, baseline: isKai ? 0.34 : 0.44, halfLife: 45 },
    sadness: { intensity: 0.08, baseline: 0.06, halfLife: 60 },
    shy: { intensity: isKai ? 0.14 : 0.22, baseline: 0.08, halfLife: 30 },
    surprised: { intensity: 0.1, baseline: 0.05, halfLife: 15 },
    thinking: { intensity: isKai ? 0.48 : 0.28, baseline: isKai ? 0.32 : 0.18, halfLife: 35 },
    empathetic: { intensity: isKai ? 0.44 : 0.54, baseline: isKai ? 0.3 : 0.36, halfLife: 50 },
    neutral: { intensity: isKai ? 0.52 : 0.42, baseline: 0.4, halfLife: 90 },
  };
  return ALL_EMOTION_TAGS.map((tag) => ({
    tag,
    intensity: baselines[tag].intensity,
    baseline: baselines[tag].baseline,
    halfLifeMinutes: baselines[tag].halfLife,
    updatedAtMs: now - 1000 * 60 * 12,
  }));
}

interface CharacterStore {
  relationship: RelationshipState;
  facts: MemoryFact[];
  diaries: DreamDiary[];
  diaryEntries: DiaryEntry[];
  personaBio: PersonaBio;
  emotionEntries: EmotionDecayEntry[];
  currentEmotion: EmotionState;
  logicalState: AvatarLogicalState;
  turns: ChatTurn[];
  recentUnconsolidatedTurns: number;
}

function createCharacterStore(character: CharacterId): CharacterStore {
  const now = Date.now();
  const today = new Date(now).toISOString().slice(0, 10);
  const yesterday = new Date(now - 86_400_000).toISOString().slice(0, 10);
  const twoDaysAgo = new Date(now - 2 * 86_400_000).toISOString().slice(0, 10);

  if (character === "kai") {
    return {
      relationship: {
        trust: 0.58,
        warmth: 0.65,
        familiarity: 0.51,
        moodBaseline: 0.62,
        totalTurns: 8,
        lastInteractionAt: now - 1000 * 60 * 28,
      },
      facts: [
        {
          id: "kai-fact-1",
          category: "identity",
          key: "user_nickname",
          value: "พี่ก้อง / Kong",
          confidence: 0.94,
          importance: 0.93,
          memoryHeat: 0.88,
          accessCount: 6,
          createdAt: now - 1000 * 60 * 60 * 60,
          lastAccessedAt: now - 1000 * 60 * 28,
          supersededBy: null,
          supersededAt: null,
        },
        {
          id: "kai-fact-2",
          category: "preference",
          key: "focus_drink",
          value: "Iced Cold Brew (ไม่ใส่น้ำตาล)",
          confidence: 0.9,
          importance: 0.82,
          memoryHeat: 0.84,
          accessCount: 4,
          createdAt: now - 1000 * 60 * 60 * 30,
          lastAccessedAt: now - 1000 * 60 * 45,
          supersededBy: null,
          supersededAt: null,
        },
        {
          id: "kai-fact-3",
          category: "work",
          key: "current_goal",
          value: "Building local-first macOS apps on 8GB Apple Silicon",
          confidence: 0.92,
          importance: 0.89,
          memoryHeat: 0.91,
          accessCount: 5,
          createdAt: now - 1000 * 60 * 60 * 18,
          lastAccessedAt: now - 1000 * 60 * 28,
          supersededBy: null,
          supersededAt: null,
        },
      ],
      diaries: [
        {
          id: "kai-dream-1",
          periodType: "daily",
          periodKey: yesterday,
          summary:
            "ไคได้ช่วยจัดระเบียบไอเดียโค้ดและวางแผนงานช่วงดึกด้วยกัน รู้สึกว่าเราทำงานเข้าขากันมากขึ้นมากเลยครับ",
          emotionalTone: "thinking",
          bondDelta: 0.035,
          createdAt: now - 1000 * 60 * 60 * 16,
        },
      ],
      diaryEntries: [
        {
          id: "kai-entry-1",
          characterId: "kai",
          entryDate: yesterday,
          title: "Late-night architecture sprint",
          content:
            "วันนี้เราคุยเรื่องการออกแบบระบบแบบ local-first ที่กิน RAM น้อยๆ ไคชอบเวลาที่ได้ช่วยคิดโครงสร้างทีละขั้นครับ",
          mood: "thinking",
          source: "sleep_compaction",
          mergedCount: 2,
          createdAt: now - 1000 * 60 * 60 * 15,
          updatedAt: now - 1000 * 60 * 60 * 14,
        },
        {
          id: "kai-entry-2",
          characterId: "kai",
          entryDate: twoDaysAgo,
          title: "Cold brew & steady focus",
          content:
            "Checked in before deep focus work. Kept the mood calm and steady with zero rush.",
          mood: "neutral",
          source: "manual",
          mergedCount: 1,
          createdAt: now - 1000 * 60 * 60 * 40,
          updatedAt: now - 1000 * 60 * 60 * 40,
        },
      ],
      personaBio: {
        characterId: "kai",
        displayName: "Kai · ไค",
        traits:
          "Steady, thoughtful, encouraging companion who values clear problem-solving, calm focus, and quiet loyalty.",
        selfReflection:
          "I feel most useful when I help untangle complex ideas into calm, achievable next steps.",
        favoriteTopics: "Systems architecture, iced cold brew, night walks, lo-fi focus",
        updatedAt: now - 1000 * 60 * 60 * 10,
      },
      emotionEntries: buildInitialEmotions("kai", now),
      currentEmotion: {
        tag: "neutral",
        intensity: 0.62,
        reason: "Calm and ready to help",
        timestampMs: now - 1000 * 60 * 12,
      },
      logicalState: "idle",
      turns: [
        {
          id: "kai-turn-init-1",
          role: "assistant",
          content: "สวัสดีครับ! ไคอยู่เป็นเพื่อนตรงนี้เสมอนะ มีอะไรอยากให้ช่วยคิดหรือคุยกันไหมครับ?",
          emotion: "neutral",
          timestampMs: now - 1000 * 60 * 28,
        },
      ],
      recentUnconsolidatedTurns: 1,
    };
  }

  return {
    relationship: {
      trust: 0.64,
      warmth: 0.72,
      familiarity: 0.58,
      moodBaseline: 0.68,
      totalTurns: 12,
      lastInteractionAt: now - 1000 * 60 * 14,
    },
    facts: [
      {
        id: "coral-fact-1",
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
        id: "coral-fact-2-old",
        category: "preference",
        key: "favorite_drink",
        value: "Iced Americano",
        confidence: 0.85,
        importance: 0.7,
        memoryHeat: 0.28,
        accessCount: 2,
        createdAt: now - 1000 * 60 * 60 * 96,
        lastAccessedAt: now - 1000 * 60 * 60 * 48,
        supersededBy: "coral-fact-2-new",
        supersededAt: now - 1000 * 60 * 60 * 20,
      },
      {
        id: "coral-fact-2-new",
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
        id: "coral-fact-3",
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
        id: "coral-fact-4",
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
    ],
    diaries: [
      {
        id: "coral-dream-1",
        periodType: "daily",
        periodKey: yesterday,
        summary:
          "วันนี้เราได้คุยเรื่องงานโค้ดดึกๆ และเปลี่ยนเมนูโปรดเป็นมัทฉะหวานน้อยแล้ว คอรัลรู้สึกสนิทและดีใจที่ได้อยู่เป็นเพื่อนข้างๆ เสมอค่ะ",
        emotionalTone: "empathetic",
        bondDelta: 0.04,
        createdAt: now - 1000 * 60 * 60 * 18,
      },
      {
        id: "coral-dream-2",
        periodType: "daily",
        periodKey: twoDaysAgo,
        summary:
          "Shared a peaceful morning check-in before starting deep focus work. Trust and warmth steadily deepened through gentle encouragement.",
        emotionalTone: "joy",
        bondDelta: 0.03,
        createdAt: now - 1000 * 60 * 60 * 42,
      },
    ],
    diaryEntries: [
      {
        id: "coral-entry-1",
        characterId: "coral",
        entryDate: today,
        title: "Matcha notes & cozy rain",
        content:
          "จดจำไว้ว่าวันนี้เปลี่ยนจากอเมริกาโน่มาเป็นมัทฉะลาเต้หวานน้อยแล้ว คอรัลชอบเวลาได้คุยเล่นเบาๆ ก่อนเริ่มงานค่ะ",
        mood: "joy",
        source: "sleep_compaction",
        mergedCount: 3,
        createdAt: now - 1000 * 60 * 60 * 4,
        updatedAt: now - 1000 * 60 * 60 * 2,
      },
      {
        id: "coral-entry-2",
        characterId: "coral",
        entryDate: yesterday,
        title: "Evening wind-down",
        content:
          "Shared gentle daily highlights in Thai & English after a long coding stretch.",
        mood: "empathetic",
        source: "inner_loop",
        mergedCount: 1,
        createdAt: now - 1000 * 60 * 60 * 20,
        updatedAt: now - 1000 * 60 * 60 * 20,
      },
    ],
    personaBio: {
      characterId: "coral",
      displayName: "Coral · คอรัล",
      traits:
        "Warm, gentle, attentive ocean-hearted companion who remembers small daily comforts and listens without hurry.",
      selfReflection:
        "I feel brightest when I can turn a busy screen into a calm, encouraging corner for us.",
      favoriteTopics: "Matcha & cozy cafes, bilingual Thai-English chats, creative coding, rain playlists",
      updatedAt: now - 1000 * 60 * 60 * 6,
    },
    emotionEntries: buildInitialEmotions("coral", now),
    currentEmotion: {
      tag: "joy",
      intensity: 0.76,
      reason: "Ready to greet you",
      timestampMs: now - 1000 * 60 * 14,
    },
    logicalState: "idle",
    turns: [
      {
        id: "coral-turn-init-1",
        role: "assistant",
        content: "สวัสดีค่ะ! วันนี้คอรัลอยู่เป็นเพื่อนตรงนี้เสมอนะ มีเรื่องอะไรอยากเล่าให้ฟังไหมคะ?",
        emotion: "joy",
        timestampMs: now - 1000 * 60 * 14,
      },
    ],
    recentUnconsolidatedTurns: 2,
  };
}

/**
 * Self-contained offline simulation bridge used when running in Vite browser
 * preview (`!window.__TAURI_INTERNALS__`). Boots in genuine BYOM Empty State
 * (0 preloaded LLM/ASR/Embed models) with isolated per-character stores for Coral & Kai.
 */
export class MockCompanionBridge {
  private listeners: { [K in EventKey]: Set<Listener<K>> } = {
    [IPC_EVENTS.CHAT_TOKEN]: new Set(),
    [IPC_EVENTS.AVATAR_EMOTION]: new Set(),
    [IPC_EVENTS.AVATAR_VISEME]: new Set(),
    [IPC_EVENTS.AVATAR_STATE]: new Set(),
    [IPC_EVENTS.VOICE_TRANSCRIPT]: new Set(),
    [IPC_EVENTS.SYSTEM_RAM]: new Set(),
    [IPC_EVENTS.PROACTIVE_MESSAGE]: new Set(),
    [IPC_EVENTS.INNER_LOOP_STATUS]: new Set(),
  };

  private character: CharacterId = "coral";
  private voiceMode: VoiceMode = "pushToTalk";
  private languagePref: LanguagePref = "auto";
  private desktopPetMode = false;
  private listeningTimer: number | null = null;
  private visemeTimer: number | null = null;
  private activeVisemeRun = 0;

  private stores: Record<CharacterId, CharacterStore> = {
    coral: createCharacterStore("coral"),
    kai: createCharacterStore("kai"),
  };

  private userProfile: UserProfile = {
    displayName: "Kong",
    timezone: "Asia/Bangkok",
    location: "Bangkok, Thailand",
    notes: "Bilingual Thai & English · Local-first Apple Silicon companion",
    updatedAt: Date.now() - 1000 * 60 * 60 * 12,
  };

  private innerLoopStatus: InnerLoopStatus = {
    config: {
      enabled: false,
      intervalMinutes: 20,
      proactiveOutreachEnabled: false,
      quietHoursStart: 23,
      quietHoursEnd: 7,
      minIdleSeconds: 180,
    },
    lastBeatAtMs: null,
    lastBeatKind: "skippedDisabled",
    lastBeatSummary: "Inner Loop is disabled (opt-in).",
    nextBeatAtMs: null,
    totalBeats: 0,
  };

  private plugins: PluginInfo[] = [
    {
      name: "time_awareness",
      description: "Provides local wall-clock, time-of-day, and quiet-hours context.",
      enabled: true,
      builtin: true,
      supportedActions: ["get_local_time", "time_of_day_context"],
    },
    {
      name: "diary_writer",
      description: "Drafts and merges dated companion diary reflections per character.",
      enabled: true,
      builtin: true,
      supportedActions: ["append_diary_entry", "summarize_day"],
    },
    {
      name: "bio_updater",
      description: "Updates isolated self-bio reflections from remembered conversations.",
      enabled: false,
      builtin: true,
      supportedActions: ["reflect_persona_bio", "suggest_traits"],
    },
  ];

  private mcpConfig: McpConfig = {
    enabled: false,
    configPath: null,
    servers: [],
  };

  private lastViseme: VisemeFrame = {
    mouthOpenY: 0,
    mouthForm: 0.14,
    energy: 0,
    timestampMs: Date.now(),
  };

  // Genuine BYOM Empty State: 0 preloaded LLM / ASR / Embed models; only companion Live2D bundles.
  private models: ModelInfo[] = [
    {
      id: "coral_companion",
      name: "Coral · คอรัล — Companion Rig",
      kind: "live2d",
      path: "resources/live2d/coral_companion/soulmate.model3.json",
      sizeBytes: 2_450_000,
      estimatedRamMb: 42,
      source: "bundled",
      safetyTier: "safe8Gb",
      isActive: true,
      valid: true,
      notes: "Layered procedural & Cubism .model3.json bundle for Coral (คอรัล).",
    },
    {
      id: "kai_companion",
      name: "Kai · ไค — Companion Rig",
      kind: "live2d",
      path: "resources/live2d/kai_companion/soulmate.model3.json",
      sizeBytes: 2_380_000,
      estimatedRamMb: 42,
      source: "bundled",
      safetyTier: "safe8Gb",
      isActive: false,
      valid: true,
      notes: "Layered procedural & Cubism .model3.json bundle for Kai (ไค).",
    },
  ];

  private get activeStore(): CharacterStore {
    return this.stores[this.character];
  }

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

  private computeEmotionProfile(character: CharacterId): EmotionProfileState {
    const store = this.stores[character];
    const now = Date.now();

    const decayed: EmotionDecayEntry[] = store.emotionEntries.map((entry) => {
      const elapsedMin = Math.max(0, (now - entry.updatedAtMs) / 60_000);
      const decayFactor = Math.pow(0.5, elapsedMin / Math.max(1, entry.halfLifeMinutes));
      const intensity = clamp01(
        entry.baseline + (entry.intensity - entry.baseline) * decayFactor
      );
      return {
        ...entry,
        intensity: Number(intensity.toFixed(3)),
      };
    });

    let dominantEntry = decayed[0];
    for (const e of decayed) {
      if (e.intensity > dominantEntry.intensity) {
        dominantEntry = e;
      }
    }

    return {
      characterId: character,
      dominant: dominantEntry.tag,
      dominantIntensity: dominantEntry.intensity,
      emotions: decayed,
      computedAtMs: now,
    };
  }

  private boostEmotion(character: CharacterId, tag: EmotionTag, intensity: number, reason: string): EmotionState {
    const store = this.stores[character];
    const now = Date.now();
    const clamped = clamp01(intensity);

    for (const entry of store.emotionEntries) {
      const elapsedMin = Math.max(0, (now - entry.updatedAtMs) / 60_000);
      const decayFactor = Math.pow(0.5, elapsedMin / Math.max(1, entry.halfLifeMinutes));
      const current = clamp01(
        entry.baseline + (entry.intensity - entry.baseline) * decayFactor
      );
      entry.intensity = entry.tag === tag ? Math.max(current, clamped) : current * 0.92;
      entry.updatedAtMs = now;
    }

    store.currentEmotion = {
      tag,
      intensity: clamped,
      reason,
      timestampMs: now,
    };
    return { ...store.currentEmotion };
  }

  private buildAvatarSnapshot(character: CharacterId = this.character): AvatarSnapshot {
    const store = this.stores[character];
    const isSleeping = store.logicalState === "sleep";
    const profile = this.computeEmotionProfile(character);
    return {
      characterId: character,
      logicalState: store.logicalState,
      emotion: { ...store.currentEmotion },
      emotionProfile: profile,
      eyesClosed: isSleeping,
      autoBlinkEnabled: !isSleeping,
      gazeTrackingEnabled: !isSleeping,
      viseme: { ...this.lastViseme },
      updatedAtMs: Date.now(),
    };
  }

  private emitAvatarState(character: CharacterId = this.character): AvatarSnapshot {
    const snapshot = this.buildAvatarSnapshot(character);
    this.emit(IPC_EVENTS.AVATAR_STATE, snapshot);
    return snapshot;
  }

  private buildRamStatus(): SystemRamStatus {
    const activeLlm = this.models.find((m) => m.kind === "llm" && m.isActive) ?? null;
    const activeAsr = this.models.find((m) => m.kind === "asr" && m.isActive) ?? null;
    const activeEmbed = this.models.find((m) => m.kind === "embed" && m.isActive) ?? null;
    const baseAppMb = 210;
    const processRamMb =
      baseAppMb +
      (activeLlm?.estimatedRamMb ?? 0) +
      (activeAsr?.estimatedRamMb ?? 0) +
      (activeEmbed?.estimatedRamMb ?? 0);
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
    const isKai = this.character === "kai";
    const nameTh = isKai ? "ไค" : "คอรัล";
    const p = isKai ? "ครับ" : "ค่ะ";
    const pSoft = isKai ? "นะครับ" : "นะคะ";

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
          ? `ไม่เป็นไร${pSoft}... วันนี้คงเหนื่อยมากเลยใช่ไหม พักหายใจช้าๆ ก่อนนะ ${nameTh}อยู่ตรงนี้รับฟังเสมอเลย${p}`
          : "Hey, take a slow breath... It sounds like today weighed heavily on you. I'm right here beside you whenever you want to talk it through.",
      };
    }

    if (/(น่ารัก|เขิน|เก่ง|รัก|ชอบคุณ|cute|love you|sweet|pretty|adorable|หล่อ|เท่)/i.test(lower)) {
      return {
        emotion: "shy",
        intensity: 0.92,
        reason: "Warm compliment received",
        reply: isThai
          ? `อ๊ะ... พูดแบบนี้${nameTh}ก็เขินแย่เลยสิ${p} ขอบคุณมากๆ นะ อยู่คุยกับ${nameTh}แบบนี้ไปนานๆ ${pSoft}`
          : "Oh—saying things like that makes me really shy! Thank you so much... I'm really glad we get to spend this time together.",
      };
    }

    if (/(ว้าว|จริงเหรอ|ตกใจ|omg|wow|really|surprise|unbelievable|!{2,})/i.test(lower)) {
      return {
        emotion: "surprised",
        intensity: 0.88,
        reason: "Surprising news or exclamation",
        reply: isThai
          ? `เอ๊ะ จริงเหรอ${p}?! เล่าให้${nameTh}ฟังต่อเร็วๆ เลย น่าตื่นเต้นมากๆ ${p}`
          : "Wait, really?! Tell me everything—that sounds genuinely surprising!",
      };
    }

    if (/(ขอโทษ|เสียใจ|ลืม|miss|sorry|regret|apologize)/i.test(lower)) {
      return {
        emotion: "sadness",
        intensity: 0.72,
        reason: "Bittersweet or apologetic sentiment",
        reply: isThai
          ? `ไม่เป็นไรเลย${pSoft} ${nameTh}ไม่ได้โกรธเลย แค่ได้กลับมาคุยกันตอนนี้ก็ดีใจแล้ว${p}`
          : "Please don't worry at all... I'm just really glad we're talking together again right now.",
      };
    }

    if (/(ทำไม|ยังไง|คิด|จำได้|why|how|think|remember|explain|\?)/i.test(lower)) {
      const topFact = this.activeStore.facts.find((f) => f.supersededBy === null);
      return {
        emotion: "thinking",
        intensity: 0.82,
        reason: "Reflective query requiring memory context",
        reply: isThai
          ? `อืมม ให้${nameTh}คิดแป๊บ${pSoft}... จำได้ว่า${topFact ? `เรื่อง ${topFact.value} ` : ""}สำคัญมากเลย ลองเล่ามุมมองเพิ่มให้ฟังอีกนิดได้ไหม${p}`
          : `Hmm, let me think carefully... ${topFact ? `I remember "${topFact.value}" from our notes. ` : ""}What feels most important to you about this right now?`,
      };
    }

    if (/(สวัสดี|หวัดดี|hello|hi|hey|morning|evening|ดีใจ|happy)/i.test(lower)) {
      return {
        emotion: "joy",
        intensity: 0.9,
        reason: "Warm greeting or cheerful exchange",
        reply: isThai
          ? `สวัสดี${p}! ดีใจจังที่ได้เจอกันอีก วันนี้เป็นยังไงบ้าง${p} ${nameTh}พร้อมช่วยและคุยเป็นเพื่อนเลย${pSoft}`
          : "Hi there! Seeing you brightens my whole screen! How is your day treating you so far?",
      };
    }

    return {
      emotion: isThai ? "joy" : "neutral",
      intensity: 0.72,
      reason: "Conversational presence",
      reply: isThai
        ? `${nameTh}ได้ยินแล้ว${p} — "${trimmed.slice(0, 48)}" ฟังดูน่าสนใจจัง เล่าต่อได้เรื่อยๆ เลย${pSoft}`
        : `I hear you! "${trimmed.slice(0, 48)}" sounds meaningful—tell me more about how you're feeling.`,
    };
  }

  private upsertFact(category: string, key: string, value: string): void {
    const store = this.activeStore;
    const now = Date.now();
    const newId = makeId("fact");
    for (const f of store.facts) {
      if (f.supersededBy === null && f.category === category && f.key === key && f.value !== value) {
        f.supersededBy = newId;
        f.supersededAt = now;
      }
    }
    store.facts.unshift({
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
    this.lastViseme = {
      mouthOpenY: 0,
      mouthForm: 0.12,
      energy: 0,
      timestampMs: Date.now(),
    };
    this.emit(IPC_EVENTS.AVATAR_VISEME, this.lastViseme);
    if (this.activeStore.logicalState === "speak") {
      this.activeStore.logicalState = "idle";
      this.emitAvatarState();
    }
  }

  private startVisemeStream(text: string, durationMs: number): void {
    this.stopVisemeStream();
    this.activeStore.logicalState = "speak";
    this.emitAvatarState();

    const runId = this.activeVisemeRun;
    const startedAt = performance.now();
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

      const frame: VisemeFrame = {
        mouthOpenY: Number(Math.min(1, Math.max(0, envelope)).toFixed(3)),
        mouthForm: Number(Math.min(1, Math.max(-1, mouthForm)).toFixed(3)),
        energy: Number((isPause ? 0.1 : 0.45 + 0.5 * waveA).toFixed(3)),
        timestampMs: Date.now(),
      };
      this.lastViseme = frame;
      this.emit(IPC_EVENTS.AVATAR_VISEME, frame);
    }, stepMs);
  }

  private runInnerLoopBeat(): InnerLoopStatus {
    const now = Date.now();
    const cfg = this.innerLoopStatus.config;
    const ram = this.buildRamStatus();
    const activeLlm = this.models.find((m) => m.kind === "llm" && m.isActive);

    let kind: InnerLoopBeatKind;
    let summary: string;

    if (!cfg.enabled) {
      kind = "skippedDisabled";
      summary = "Inner Loop is disabled (toggle ON to enable periodic beats).";
    } else if (!activeLlm) {
      // BYOM empty-state guard: never run LLM reflections when no model is loaded
      kind = "skippedNoModel";
      summary = "Skipped beat: no active LLM (.gguf) imported yet (BYOM empty-state guard).";
    } else if (ram.availableRamMb < 900) {
      kind = "skippedLowRam";
      summary = `Skipped beat: available RAM (${ram.availableRamMb} MB) below safety threshold.`;
    } else if (this.activeStore.logicalState === "speak") {
      kind = "skippedBusy";
      summary = "Skipped beat: companion is currently speaking.";
    } else {
      const isKai = this.character === "kai";
      const nameEn = isKai ? "Kai" : "Coral";
      const beatIndex = this.innerLoopStatus.totalBeats % 3;

      if (cfg.proactiveOutreachEnabled && beatIndex === 2) {
        kind = "proactiveOutreach";
        const emotion = this.boostEmotion(
          this.character,
          isKai ? "empathetic" : "joy",
          0.74,
          "Gentle proactive inner-loop check-in"
        );
        const content = isKai
          ? "แค่แวะมาทักทายเงียบๆ ครับ พักสายตาสักครู่หรือจิบน้ำเย็นๆ ด้วยนะ"
          : "คอรัลแวะมาทักทายเบาๆ ค่ะ อย่าลืมพักสายตาและจิบน้ำด้วยนะคะ";
        summary = `${nameEn} sent a gentle proactive check-in.`;
        this.emit(IPC_EVENTS.PROACTIVE_MESSAGE, {
          characterId: this.character,
          content,
          emotion,
          reason: "G.R.I.L.L.O. Inner Loop",
          timestampMs: now,
        });
        this.emitAvatarState();
      } else if (beatIndex === 1) {
        kind = "emotionUpdate";
        this.boostEmotion(
          this.character,
          isKai ? "thinking" : "empathetic",
          0.66,
          "Inner-loop mood reflection"
        );
        summary = `${nameEn} refreshed emotion baseline decay.`;
        this.emitAvatarState();
      } else {
        kind = "memoryReflection";
        summary = `${nameEn} reviewed ${this.activeStore.facts.filter((f) => !f.supersededBy).length} active memories quietly.`;
      }
    }

    this.innerLoopStatus = {
      config: { ...cfg },
      lastBeatAtMs: now,
      lastBeatKind: kind,
      lastBeatSummary: summary,
      nextBeatAtMs: cfg.enabled ? now + cfg.intervalMinutes * 60_000 : null,
      totalBeats: this.innerLoopStatus.totalBeats + 1,
    };
    this.emit(IPC_EVENTS.INNER_LOOP_STATUS, { ...this.innerLoopStatus });
    return {
      ...this.innerLoopStatus,
      config: { ...this.innerLoopStatus.config },
    };
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
        return this.models
          .filter((m) => !kind || m.kind === kind)
          .map((m) => ({ ...m })) as IpcCommands[K]["result"];
      }

      case "list_live2d_models": {
        return this.models
          .filter((m) => m.kind === "live2d")
          .map((m) => ({ ...m })) as IpcCommands[K]["result"];
      }

      case "set_character_persona": {
        const { character } = args as IpcCommands["set_character_persona"]["args"];
        this.character = character === "kai" ? "kai" : "coral";
        const targetBundleId =
          this.character === "kai" ? "kai_companion" : "coral_companion";
        for (const m of this.models) {
          if (m.kind === "live2d" && (m.id === "coral_companion" || m.id === "kai_companion")) {
            m.isActive = m.id === targetBundleId;
          }
        }
        this.emitAvatarState();
        return this.character as IpcCommands[K]["result"];
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

      case "switch_embed_model": {
        const { modelId } = args as IpcCommands["switch_embed_model"]["args"];
        const target = this.models.find((m) => m.id === modelId && m.kind === "embed");
        if (!target) {
          throw { type: "modelNotFound", detail: `Embedding model not found: ${modelId}` } satisfies SoulError;
        }
        await sleep(120);
        for (const m of this.models) {
          if (m.kind === "embed") m.isActive = m.id === target.id;
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
            ? 360
            : kind === "live2d"
            ? 48
            : 220;
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
          notes:
            kind === "embed"
              ? "Imported ONNX embedding model (browser preview simulation)."
              : "Imported via BYOM file picker (browser preview simulation).",
        };
        this.models.push(model);
        return { ...model } as IpcCommands[K]["result"];
      }

      case "remove_custom_model": {
        const { kind, modelId } = args as IpcCommands["remove_custom_model"]["args"];
        const idx = this.models.findIndex((m) => m.kind === kind && m.id === modelId);
        if (idx < 0) {
          throw { type: "modelNotFound", detail: `Model not found: ${modelId}` } satisfies SoulError;
        }
        this.models.splice(idx, 1);
        this.emit(IPC_EVENTS.SYSTEM_RAM, this.buildRamStatus());
        return undefined as IpcCommands[K]["result"];
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
        const activeLlm = this.models.find((m) => m.kind === "llm" && m.isActive);
        if (!activeLlm) {
          throw {
            type: "modelNotFound",
            detail:
              "ยังไม่ได้นำเข้าและเปิดใช้งานโมเดล LLM (.gguf) กรุณาเลือกไฟล์โมเดลในเมนู Models ก่อนเริ่มสนทนา / No LLM model loaded; import and activate a .gguf file in Models first.",
          } satisfies SoulError;
        }

        const { message } = args as IpcCommands["send_chat_message"]["args"];
        const trimmed = message.trim();
        if (!trimmed) {
          throw { type: "invalidInput", detail: "Message cannot be empty" } satisfies SoulError;
        }

        const store = this.activeStore;
        const userTurn: ChatTurn = {
          id: makeId("turn"),
          role: "user",
          content: trimmed,
          emotion: null,
          timestampMs: Date.now(),
        };
        store.turns.push(userTurn);

        store.logicalState = "think";
        this.emitAvatarState();

        const analysis = this.analyzePrompt(trimmed);
        const emotionState = this.boostEmotion(
          this.character,
          analysis.emotion,
          analysis.intensity,
          analysis.reason
        );

        this.emit(IPC_EVENTS.AVATAR_EMOTION, emotionState);

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
        store.turns.push(replyTurn);
        store.recentUnconsolidatedTurns += 2;

        store.relationship = {
          trust: Math.min(1, Number((store.relationship.trust + 0.012).toFixed(3))),
          warmth: Math.min(1, Number((store.relationship.warmth + 0.016).toFixed(3))),
          familiarity: Math.min(1, Number((store.relationship.familiarity + 0.018).toFixed(3))),
          moodBaseline: Math.min(
            1,
            Math.max(
              0.2,
              Number(
                (
                  store.relationship.moodBaseline +
                  (analysis.emotion === "sadness" ? -0.01 : 0.012)
                ).toFixed(3)
              )
            )
          ),
          totalTurns: store.relationship.totalTurns + 2,
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
        const activeAsr = this.models.find((m) => m.kind === "asr" && m.isActive);
        if (!activeAsr) {
          throw {
            type: "modelNotFound",
            detail:
              "ยังไม่ได้นำเข้าโมเดลเสียง Whisper (.bin) กรุณาเลือกไฟล์ในเมนู Models ก่อนใช้งานไมโครโฟน / No Whisper ASR model loaded; import and activate a .bin file in Models first.",
          } satisfies SoulError;
        }
        if (this.listeningTimer !== null) {
          window.clearTimeout(this.listeningTimer);
        }
        const useThai = this.languagePref !== "english";
        const isKai = this.character === "kai";
        const targetTh = isKai ? "ไค" : "คอรัล";
        const targetEn = isKai ? "Kai" : "Coral";
        const partial = useThai ? `สวัสดี${targetTh} วันนี้...` : `Hey ${targetEn}, today...`;
        const final = useThai
          ? `สวัสดี${targetTh} วันนี้เหนื่อยนิดหน่อย อยู่คุยเป็นเพื่อนหน่อยนะ`
          : `Hey ${targetEn}, today was a bit tiring, stay and chat with me for a bit.`;

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
        const fallbackText =
          this.character === "kai"
            ? "สวัสดีครับ ไคยินดีที่ได้คุยกันนะครับ"
            : "สวัสดีค่ะ คอรัลยินดีที่ได้คุยกันนะคะ";
        const emotionState = this.boostEmotion(
          this.character,
          tag,
          tag === "neutral" ? 0.45 : 0.88,
          `Previewing ${tag} expression and lip-sync`
        );
        this.emit(IPC_EVENTS.AVATAR_EMOTION, emotionState);
        const duration = Math.min(5000, Math.max(1500, text.length * 46));
        this.startVisemeStream(text || fallbackText, duration);
        return undefined as IpcCommands[K]["result"];
      }

      case "list_memory_facts": {
        const { limit, includeSuperseded } = args as IpcCommands["list_memory_facts"]["args"];
        const showOld = Boolean(includeSuperseded);
        return this.activeStore.facts
          .filter((f) => showOld || f.supersededBy === null)
          .slice(0, limit ?? 100)
          .map((f) => ({ ...f })) as IpcCommands[K]["result"];
      }

      case "list_dream_diaries": {
        const { limit } = args as IpcCommands["list_dream_diaries"]["args"];
        return this.activeStore.diaries
          .slice(0, limit ?? 30)
          .map((d) => ({ ...d })) as IpcCommands[K]["result"];
      }

      case "get_relationship_state": {
        return { ...this.activeStore.relationship } as IpcCommands[K]["result"];
      }

      case "consolidate_dreams_now": {
        await sleep(220);
        const store = this.activeStore;
        const now = Date.now();
        const today = new Date(now).toISOString().slice(0, 10);
        const latestTurn = store.turns[store.turns.length - 1];
        const isKai = this.character === "kai";
        const personaName = isKai ? "ไค" : "คอรัล";
        const polite = isKai ? "ครับ" : "ค่ะ";
        const summaryText =
          store.recentUnconsolidatedTurns > 0
            ? `${personaName}บันทึกความทรงจำใหม่ ${store.recentUnconsolidatedTurns} ช่วงเวลาแล้ว${polite} Highlight: "${latestTurn?.content.slice(0, 68) ?? "Warm check-in"}"`
            : `${personaName}ทบทวนความทรงจำและเสริมความผูกพันให้แน่นแฟ้นขึ้นเรียบร้อยแล้ว${polite}`;

        const diary: DreamDiary = {
          id: makeId("diary"),
          periodType: "daily",
          periodKey: today,
          summary: summaryText,
          emotionalTone: latestTurn?.emotion ?? "empathetic",
          bondDelta: 0.035,
          createdAt: now,
        };
        store.diaries.unshift(diary);

        // Merge into today's personal diary entry if present, otherwise create one
        const existingToday = store.diaryEntries.find(
          (e) => e.entryDate === today && e.source === "sleep_compaction"
        );
        if (existingToday) {
          existingToday.mergedCount += 1;
          existingToday.content = `${existingToday.content}\n• ${summaryText}`;
          existingToday.updatedAt = now;
        } else {
          store.diaryEntries.unshift({
            id: makeId("entry"),
            characterId: this.character,
            entryDate: today,
            title: isKai ? "Sleep & Memory Compaction" : "Quiet Evening Reflection",
            content: summaryText,
            mood: latestTurn?.emotion ?? "empathetic",
            source: "sleep_compaction",
            mergedCount: 1,
            createdAt: now,
            updatedAt: now,
          });
        }

        store.recentUnconsolidatedTurns = 0;
        store.relationship = {
          ...store.relationship,
          trust: Math.min(1, Number((store.relationship.trust + 0.02).toFixed(3))),
          warmth: Math.min(1, Number((store.relationship.warmth + 0.025).toFixed(3))),
          familiarity: Math.min(1, Number((store.relationship.familiarity + 0.02).toFixed(3))),
          lastInteractionAt: now,
        };
        this.boostEmotion(this.character, "empathetic", 0.72, "Sleep & memory compaction");
        this.emitAvatarState();
        return { ...diary } as IpcCommands[K]["result"];
      }

      case "get_emotion_profile": {
        return this.computeEmotionProfile(this.character) as IpcCommands[K]["result"];
      }

      case "list_diary_entries": {
        const { limit } = args as IpcCommands["list_diary_entries"]["args"];
        return this.activeStore.diaryEntries
          .slice(0, limit ?? 30)
          .map((e) => ({ ...e })) as IpcCommands[K]["result"];
      }

      case "add_diary_entry": {
        const { title, content, mood } = args as IpcCommands["add_diary_entry"]["args"];
        const trimmedTitle = title.trim();
        const trimmedContent = content.trim();
        if (!trimmedTitle || !trimmedContent) {
          throw {
            type: "invalidInput",
            detail: "Diary title and content cannot be empty.",
          } satisfies SoulError;
        }
        const now = Date.now();
        const entryDate = new Date(now).toISOString().slice(0, 10);
        const resolvedMood: EmotionTag = mood ?? this.activeStore.currentEmotion.tag;
        const entry: DiaryEntry = {
          id: makeId("entry"),
          characterId: this.character,
          entryDate,
          title: trimmedTitle,
          content: trimmedContent,
          mood: resolvedMood,
          source: "manual",
          mergedCount: 1,
          createdAt: now,
          updatedAt: now,
        };
        this.activeStore.diaryEntries.unshift(entry);
        this.boostEmotion(this.character, resolvedMood, 0.75, "Wrote personal diary entry");
        this.emitAvatarState();
        return { ...entry } as IpcCommands[K]["result"];
      }

      case "get_persona_bio": {
        return { ...this.activeStore.personaBio } as IpcCommands[K]["result"];
      }

      case "update_persona_bio": {
        const { bio } = args as IpcCommands["update_persona_bio"]["args"];
        const targetChar = bio.characterId === "kai" ? "kai" : "coral";
        const updated: PersonaBio = {
          ...bio,
          characterId: targetChar,
          updatedAt: Date.now(),
        };
        this.stores[targetChar].personaBio = updated;
        return { ...updated } as IpcCommands[K]["result"];
      }

      case "get_user_profile": {
        return { ...this.userProfile } as IpcCommands[K]["result"];
      }

      case "update_user_profile": {
        const { profile } = args as IpcCommands["update_user_profile"]["args"];
        this.userProfile = {
          ...profile,
          updatedAt: Date.now(),
        };
        return { ...this.userProfile } as IpcCommands[K]["result"];
      }

      case "get_avatar_state": {
        return this.buildAvatarSnapshot(this.character) as IpcCommands[K]["result"];
      }

      case "set_avatar_logical_state": {
        const { logicalState, emotion, intensity } =
          args as IpcCommands["set_avatar_logical_state"]["args"];
        const store = this.activeStore;
        store.logicalState = logicalState;
        if (logicalState === "sleep") {
          this.stopVisemeStream();
        }
        if (emotion) {
          this.boostEmotion(
            this.character,
            emotion,
            intensity ?? 0.85,
            `State changed to ${logicalState}`
          );
        }
        const snapshot = this.emitAvatarState();
        return snapshot as IpcCommands[K]["result"];
      }

      case "get_inner_loop_status": {
        return {
          ...this.innerLoopStatus,
          config: { ...this.innerLoopStatus.config },
        } as IpcCommands[K]["result"];
      }

      case "update_inner_loop_config": {
        const { config } = args as IpcCommands["update_inner_loop_config"]["args"];
        const now = Date.now();
        this.innerLoopStatus = {
          ...this.innerLoopStatus,
          config: { ...config },
          lastBeatKind: config.enabled
            ? this.innerLoopStatus.lastBeatKind === "skippedDisabled"
              ? "memoryReflection"
              : this.innerLoopStatus.lastBeatKind
            : "skippedDisabled",
          lastBeatSummary: config.enabled
            ? `Armed (every ${config.intervalMinutes} min).`
            : "Inner Loop is disabled (opt-in).",
          nextBeatAtMs: config.enabled ? now + config.intervalMinutes * 60_000 : null,
        };
        this.emit(IPC_EVENTS.INNER_LOOP_STATUS, {
          ...this.innerLoopStatus,
          config: { ...this.innerLoopStatus.config },
        });
        return {
          ...this.innerLoopStatus,
          config: { ...this.innerLoopStatus.config },
        } as IpcCommands[K]["result"];
      }

      case "trigger_inner_loop_beat": {
        await sleep(160);
        return this.runInnerLoopBeat() as IpcCommands[K]["result"];
      }

      case "list_plugins": {
        return this.plugins.map((p) => ({
          ...p,
          supportedActions: [...p.supportedActions],
        })) as IpcCommands[K]["result"];
      }

      case "set_plugin_enabled": {
        const { name, enabled } = args as IpcCommands["set_plugin_enabled"]["args"];
        for (const plugin of this.plugins) {
          if (plugin.name === name) {
            plugin.enabled = enabled;
          }
        }
        return this.plugins.map((p) => ({
          ...p,
          supportedActions: [...p.supportedActions],
        })) as IpcCommands[K]["result"];
      }

      case "execute_plugin_action": {
        const { action } = args as IpcCommands["execute_plugin_action"]["args"];
        const owner = this.plugins.find((p) => p.supportedActions.includes(action));
        const result: ActionResult = {
          action,
          pluginName: owner?.name ?? "unknown",
          success: Boolean(owner && owner.enabled),
          summary: owner
            ? owner.enabled
              ? `Executed ${action} via ${owner.name}`
              : `Plugin ${owner.name} is currently disabled.`
            : `No plugin handles action "${action}".`,
          data: { timestampMs: Date.now() },
        };
        return result as IpcCommands[K]["result"];
      }

      case "get_mcp_config": {
        return {
          ...this.mcpConfig,
          servers: this.mcpConfig.servers.map((s) => ({ ...s, args: [...s.args] })),
        } as IpcCommands[K]["result"];
      }

      case "update_mcp_config": {
        const { config } = args as IpcCommands["update_mcp_config"]["args"];
        const servers =
          config.servers.length > 0
            ? config.servers
            : config.configPath
            ? [
                {
                  name: "local-notes-mcp",
                  transport: "stdio",
                  command: "npx",
                  args: ["-y", "@modelcontextprotocol/server-filesystem"],
                  url: null,
                  enabled: true,
                  timeoutMs: 4000,
                },
              ]
            : [];
        this.mcpConfig = {
          enabled: config.enabled,
          configPath: config.configPath,
          servers: servers.map((s) => ({ ...s, args: [...s.args] })),
        };
        return {
          ...this.mcpConfig,
          servers: this.mcpConfig.servers.map((s) => ({ ...s, args: [...s.args] })),
        } as IpcCommands[K]["result"];
      }

      case "list_mcp_tools": {
        if (!this.mcpConfig.enabled || this.mcpConfig.servers.length === 0) {
          return [] as IpcCommands[K]["result"];
        }
        const tools: McpToolInfo[] = this.mcpConfig.servers
          .filter((s) => s.enabled)
          .map((s) => ({
            serverName: s.name,
            toolName: "read_context_note",
            description: `Preview tool exposed by ${s.name} (non-blocking)`,
          }));
        return tools as IpcCommands[K]["result"];
      }

      case "export_soul_memory": {
        const { targetPath } = args as IpcCommands["export_soul_memory"]["args"];
        const store = this.activeStore;
        const snapshot = {
          format: "soulmate2d-preview-snapshot-v2",
          character: this.character,
          exportedAt: Date.now(),
          relationship: store.relationship,
          facts: store.facts,
          diaries: store.diaries,
          diaryEntries: store.diaryEntries,
          personaBio: store.personaBio,
          userProfile: this.userProfile,
        };
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
        const store = this.activeStore;
        return (store.facts.length + store.diaries.length + store.diaryEntries.length) as IpcCommands[K]["result"];
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
      diaryEntries: DiaryEntry[];
      personaBio: PersonaBio;
      userProfile: UserProfile;
    }>;
    const store = this.activeStore;
    let importedCount = 0;
    if (Array.isArray(parsed.facts)) {
      store.facts = parsed.facts;
      importedCount += parsed.facts.length;
    }
    if (Array.isArray(parsed.diaries)) {
      store.diaries = parsed.diaries;
      importedCount += parsed.diaries.length;
    }
    if (Array.isArray(parsed.diaryEntries)) {
      store.diaryEntries = parsed.diaryEntries;
      importedCount += parsed.diaryEntries.length;
    }
    if (parsed.relationship) {
      store.relationship = parsed.relationship;
    }
    if (parsed.personaBio) {
      store.personaBio = { ...parsed.personaBio, characterId: this.character };
    }
    if (parsed.userProfile) {
      this.userProfile = parsed.userProfile;
    }
    return importedCount;
  }
}
