import { invoke } from "@tauri-apps/api/core";

export const IPC_EVENTS = {
  CHAT_TOKEN: "chat://token",
  AVATAR_EMOTION: "avatar://emotion",
  AVATAR_VISEME: "avatar://viseme",
  AVATAR_STATE: "avatar://state",
  VOICE_TRANSCRIPT: "voice://transcript",
  SYSTEM_RAM: "system://ram",
  PROACTIVE_MESSAGE: "companion://proactive",
  INNER_LOOP_STATUS: "companion://inner-loop",
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

export type CharacterId = "coral" | "kai";

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

export interface StarterAssetInfo {
  id: string;
  name: string;
  descriptionEn: string;
  descriptionTh: string;
  kind: ModelKind;
  repo: string;
  filename: string;
  approxSizeBytes: number;
  estimatedRamMb: number;
  safetyTier: RamSafetyTier;
  isPrimary: boolean;
  installed: boolean;
  installedModelId: string | null;
  installedPath: string | null;
  license: string;
}

export type DownloadStatus =
  | "resolving"
  | "downloading"
  | "verifying"
  | "completed"
  | "cancelled"
  | "error";

export interface DownloadProgress {
  assetId: string;
  downloadedBytes: number;
  totalBytes: number;
  speedMbps: number;
  status: DownloadStatus;
  message: string | null;
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

export interface EmotionDecayEntry {
  tag: EmotionTag;
  intensity: number;
  baseline: number;
  halfLifeMinutes: number;
  updatedAtMs: number;
}

export interface EmotionProfileState {
  characterId: CharacterId;
  dominant: EmotionTag;
  dominantIntensity: number;
  emotions: EmotionDecayEntry[];
  computedAtMs: number;
}

export interface DiaryEntry {
  id: string;
  characterId: CharacterId;
  entryDate: string;
  title: string;
  content: string;
  mood: EmotionTag;
  source: string;
  mergedCount: number;
  createdAt: number;
  updatedAt: number;
}

export interface PersonaBio {
  characterId: CharacterId;
  displayName: string;
  traits: string;
  selfReflection: string;
  favoriteTopics: string;
  updatedAt: number;
}

export interface UserProfile {
  displayName: string;
  timezone: string;
  location: string;
  notes: string;
  updatedAt: number;
}

export type AvatarLogicalState = "idle" | "think" | "write" | "speak" | "sleep";

export interface AvatarSnapshot {
  characterId: CharacterId;
  logicalState: AvatarLogicalState;
  emotion: EmotionState;
  emotionProfile: EmotionProfileState;
  eyesClosed: boolean;
  autoBlinkEnabled: boolean;
  gazeTrackingEnabled: boolean;
  viseme: VisemeFrame;
  updatedAtMs: number;
}

export interface InnerLoopConfig {
  enabled: boolean;
  intervalMinutes: number;
  proactiveOutreachEnabled: boolean;
  quietHoursStart: number | null;
  quietHoursEnd: number | null;
  minIdleSeconds: number;
}

export type InnerLoopBeatKind =
  | "memoryReflection"
  | "emotionUpdate"
  | "proactiveOutreach"
  | "skippedNoModel"
  | "skippedBusy"
  | "skippedLowRam"
  | "skippedDisabled";

export interface InnerLoopStatus {
  config: InnerLoopConfig;
  lastBeatAtMs: number | null;
  lastBeatKind: InnerLoopBeatKind | null;
  lastBeatSummary: string | null;
  nextBeatAtMs: number | null;
  totalBeats: number;
}

export interface ProactiveMessageEvent {
  characterId: CharacterId;
  content: string;
  emotion: EmotionState;
  reason: string;
  timestampMs: number;
}

export interface ActionInvocation {
  action: string;
  params: Record<string, unknown>;
}

export interface ActionResult {
  action: string;
  pluginName: string;
  success: boolean;
  summary: string;
  data: unknown | null;
}

export interface PluginInfo {
  name: string;
  description: string;
  enabled: boolean;
  builtin: boolean;
  supportedActions: string[];
}

export interface McpServerConfig {
  name: string;
  transport: string;
  command: string | null;
  args: string[];
  url: string | null;
  enabled: boolean;
  timeoutMs: number;
}

export interface McpConfig {
  enabled: boolean;
  configPath: string | null;
  servers: McpServerConfig[];
}

export interface McpToolInfo {
  serverName: string;
  toolName: string;
  description: string;
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
  list_starter_catalog: {
    args: Record<string, never>;
    result: StarterAssetInfo[];
  };
  cancel_download: {
    args: { assetId: string };
    result: boolean;
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
  set_character_persona: {
    args: { character: CharacterId };
    result: CharacterId;
  };
  switch_embed_model: {
    args: { modelId: string };
    result: ModelInfo;
  };
  remove_custom_model: {
    args: { kind: ModelKind; modelId: string };
    result: void;
  };
  get_emotion_profile: {
    args: Record<string, never>;
    result: EmotionProfileState;
  };
  list_diary_entries: {
    args: { limit?: number | null };
    result: DiaryEntry[];
  };
  add_diary_entry: {
    args: { title: string; content: string; mood?: EmotionTag | null };
    result: DiaryEntry;
  };
  get_persona_bio: {
    args: Record<string, never>;
    result: PersonaBio;
  };
  update_persona_bio: {
    args: { bio: PersonaBio };
    result: PersonaBio;
  };
  get_user_profile: {
    args: Record<string, never>;
    result: UserProfile;
  };
  update_user_profile: {
    args: { profile: UserProfile };
    result: UserProfile;
  };
  get_avatar_state: {
    args: Record<string, never>;
    result: AvatarSnapshot;
  };
  set_avatar_logical_state: {
    args: {
      logicalState: AvatarLogicalState;
      emotion?: EmotionTag | null;
      intensity?: number | null;
    };
    result: AvatarSnapshot;
  };
  get_inner_loop_status: {
    args: Record<string, never>;
    result: InnerLoopStatus;
  };
  update_inner_loop_config: {
    args: { config: InnerLoopConfig };
    result: InnerLoopStatus;
  };
  trigger_inner_loop_beat: {
    args: Record<string, never>;
    result: InnerLoopStatus;
  };
  list_plugins: {
    args: Record<string, never>;
    result: PluginInfo[];
  };
  set_plugin_enabled: {
    args: { name: string; enabled: boolean };
    result: PluginInfo[];
  };
  execute_plugin_action: {
    args: { action: string; params?: Record<string, unknown> | null };
    result: ActionResult;
  };
  get_mcp_config: {
    args: Record<string, never>;
    result: McpConfig;
  };
  update_mcp_config: {
    args: { config: McpConfig };
    result: McpConfig;
  };
  list_mcp_tools: {
    args: Record<string, never>;
    result: McpToolInfo[];
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
