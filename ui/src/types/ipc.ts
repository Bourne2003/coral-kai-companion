import { invoke } from "@tauri-apps/api/core";

export const IPC_EVENTS = {
  CHAT_TOKEN: "chat://token",
  AVATAR_EMOTION: "avatar://emotion",
  AVATAR_VISEME: "avatar://viseme",
  VOICE_TRANSCRIPT: "voice://transcript",
  SYSTEM_RAM: "system://ram",
} as const;

export type SoulError =
  | { type: "memory"; detail: string }
  | { type: "llm"; detail: string }
  | { type: "voice"; detail: string }
  | { type: "modelNotFound"; detail: string }
  | { type: "insufficientRam"; detail: { neededMb: number; availableMb: number } }
  | { type: "invalidInput"; detail: string }
  | { type: "io"; detail: string }
  | { type: "serialization"; detail: string }
  | { type: "notImplemented"; detail: string }
  | { type: "internal"; detail: string };

export type EmotionTag =
  | "joy"
  | "sadness"
  | "shy"
  | "surprised"
  | "thinking"
  | "empathetic"
  | "neutral";

export interface EmotionState {
  tag: EmotionTag;
  intensity: number;
  reason: string;
  timestampMs: number;
}

export interface VisemeFrame {
  mouthOpenY: number;
  mouthForm: number;
  energy: number;
  timestampMs: number;
}

export type ChatRole = "user" | "assistant" | "system";

export interface ChatTurn {
  id: string;
  role: ChatRole;
  content: string;
  emotion: EmotionTag | null;
  timestampMs: number;
}

export interface BrainReply {
  turn: ChatTurn;
  emotion: EmotionState;
}

export interface MemoryFact {
  id: string;
  category: string;
  key: string;
  value: string;
  confidence: number;
  importance: number;
  memoryHeat: number;
  accessCount: number;
  createdAt: number;
  lastAccessedAt: number;
  supersededBy: string | null;
  supersededAt: number | null;
}

export interface EpisodeRecord {
  id: string;
  summary: string;
  rawTurnCount: number;
  emotionalValence: number;
  memoryHeat: number;
  createdAt: number;
  lastAccessedAt: number;
}

export interface DreamDiary {
  id: string;
  periodType: string;
  periodKey: string;
  summary: string;
  emotionalTone: string;
  bondDelta: number;
  createdAt: number;
}

export interface RelationshipState {
  trust: number;
  warmth: number;
  familiarity: number;
  moodBaseline: number;
  totalTurns: number;
  lastInteractionAt: number;
}

export interface MemoryRetrievalContext {
  facts: MemoryFact[];
  episodes: EpisodeRecord[];
  recentDiaries: DreamDiary[];
  relationship: RelationshipState;
}

export type ModelKind = "llm" | "asr" | "embed" | "live2d";

export type ModelSource = "bundled" | "userData" | "custom";

export type RamSafetyTier = "safe8Gb" | "tight8Gb" | "recommended16GbPlus";

export interface ModelInfo {
  id: string;
  name: string;
  kind: ModelKind;
  path: string;
  sizeBytes: number;
  estimatedRamMb: number;
  source: ModelSource;
  safetyTier: RamSafetyTier;
  isActive: boolean;
  valid: boolean;
  notes: string | null;
}

export interface SystemRamStatus {
  totalRamMb: number;
  availableRamMb: number;
  processRamMb: number;
  budgetCeilingMb: number;
  is8gbClass: boolean;
  activeLlmId: string | null;
  activeAsrId: string | null;
}

export type VoiceMode = "pushToTalk" | "handsFree" | "muted";

export type LanguagePref = "auto" | "thai" | "english";

export interface TokenChunk {
  text: string;
  isFinal: boolean;
  fullText: string;
}

export interface TranscriptEvent {
  text: string;
  language: string;
  isFinal: boolean;
  timestampMs: number;
}

export interface IpcCommands {
  send_chat_message: {
    args: { message: string };
    result: BrainReply;
  };
  get_ram_status: {
    args: Record<string, never>;
    result: SystemRamStatus;
  };
  list_models: {
    args: { kind?: ModelKind | null };
    result: ModelInfo[];
  };
  hot_swap_llm: {
    args: { modelId: string };
    result: ModelInfo;
  };
  switch_asr_model: {
    args: { modelId: string };
    result: ModelInfo;
  };
  register_custom_model: {
    args: { kind: ModelKind; path: string; name?: string | null };
    result: ModelInfo;
  };
  set_voice_mode: {
    args: { mode: VoiceMode };
    result: void;
  };
  set_language_pref: {
    args: { pref: LanguagePref };
    result: void;
  };
  start_listening: {
    args: Record<string, never>;
    result: void;
  };
  stop_listening: {
    args: Record<string, never>;
    result: void;
  };
  stop_speaking: {
    args: Record<string, never>;
    result: void;
  };
  preview_speech_and_visemes: {
    args: { text: string; emotion?: EmotionTag | null };
    result: void;
  };
  list_memory_facts: {
    args: { limit?: number | null; includeSuperseded?: boolean | null };
    result: MemoryFact[];
  };
  list_dream_diaries: {
    args: { limit?: number | null };
    result: DreamDiary[];
  };
  get_relationship_state: {
    args: Record<string, never>;
    result: RelationshipState;
  };
  consolidate_dreams_now: {
    args: Record<string, never>;
    result: DreamDiary | null;
  };
  export_soul_memory: {
    args: { targetPath: string };
    result: string;
  };
  import_soul_memory: {
    args: { sourcePath: string };
    result: number;
  };
  set_desktop_pet_mode: {
    args: { enabled: boolean };
    result: boolean;
  };
  list_live2d_models: {
    args: Record<string, never>;
    result: ModelInfo[];
  };
  load_live2d_folder: {
    args: { folderPath: string };
    result: ModelInfo;
  };
}

export type IpcCommandName = keyof IpcCommands;

export function invokeIpc<K extends IpcCommandName>(
  command: K,
  ...args: Record<string, never> extends IpcCommands[K]["args"]
    ? [args?: IpcCommands[K]["args"]]
    : [args: IpcCommands[K]["args"]]
): Promise<IpcCommands[K]["result"]> {
  return invoke<IpcCommands[K]["result"]>(command, args[0] ?? {});
}
