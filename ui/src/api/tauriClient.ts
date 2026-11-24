import { Channel, convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  IPC_EVENTS,
  type AvatarSnapshot,
  type DownloadProgress,
  type EmotionState,
  type InnerLoopStatus,
  type IpcCommandName,
  type IpcCommands,
  type ModelInfo,
  type ModelKind,
  type ProactiveMessageEvent,
  type SoulError,
  type SystemRamStatus,
  type TokenChunk,
  type TranscriptEvent,
  type VisemeFrame,
} from "../types/ipc";
import { MockCompanionBridge } from "./mockEngine";

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

export type EventKey = keyof EventMap;

export interface Live2dFolderFiles {
  folderName: string;
  files: File[];
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

const mockBridge = new MockCompanionBridge();

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

export async function invokeCommand<K extends IpcCommandName>(
  command: K,
  ...args: Record<string, never> extends IpcCommands[K]["args"]
    ? [args?: IpcCommands[K]["args"]]
    : [args: IpcCommands[K]["args"]]
): Promise<IpcCommands[K]["result"]> {
  const payload = (args[0] ?? {}) as IpcCommands[K]["args"];
  if (isTauriRuntime()) {
    return invoke<IpcCommands[K]["result"]>(command, payload);
  }
  return mockBridge.invoke(command, payload);
}

export async function downloadStarterAsset(
  assetId: string,
  onProgress: (p: DownloadProgress) => void
): Promise<ModelInfo> {
  if (isTauriRuntime()) {
    const channel = new Channel<DownloadProgress>();
    channel.onmessage = onProgress;
    return invoke<ModelInfo>("download_starter_asset", {
      assetId,
      onProgress: channel,
    });
  }
  return mockBridge.downloadStarterAsset(assetId, onProgress);
}

export async function listenEvent<K extends EventKey>(
  eventName: K,
  handler: (payload: EventMap[K]) => void
): Promise<UnlistenFn> {
  if (isTauriRuntime()) {
    return listen<EventMap[K]>(eventName, (evt) => {
      handler(evt.payload);
    });
  }
  return mockBridge.listen(eventName, handler);
}

export function resolveAssetUrl(filePath: string): string {
  if (isTauriRuntime()) {
    return convertFileSrc(filePath);
  }
  return filePath;
}

export function formatSoulError(err: unknown): string {
  if (!err) return "Unknown error occurred.";

  let candidate: unknown = err;
  if (typeof err === "string") {
    try {
      candidate = JSON.parse(err);
    } catch {
      return err;
    }
  }

  if (typeof candidate === "object" && candidate !== null && "type" in candidate) {
    const soulErr = candidate as SoulError;
    if (soulErr.type === "insufficientRam") {
      const { neededMb, availableMb } = soulErr.detail;
      return `Insufficient RAM: requires ${neededMb.toLocaleString()} MB, only ${availableMb.toLocaleString()} MB available in 8GB budget.`;
    }
    return `${soulErr.type}: ${String(soulErr.detail)}`;
  }

  if (err instanceof Error) return err.message;
  return String(err);
}

function openBrowserFilePicker(options: {
  accept?: string;
  directory?: boolean;
}): Promise<FileList | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.style.display = "none";
    if (options.accept) input.accept = options.accept;
    if (options.directory) {
      input.setAttribute("webkitdirectory", "");
      input.setAttribute("directory", "");
      input.multiple = true;
    }

    let settled = false;
    const finish = (files: FileList | null) => {
      if (settled) return;
      settled = true;
      input.remove();
      resolve(files && files.length > 0 ? files : null);
    };

    input.addEventListener("change", () => finish(input.files), { once: true });
    window.addEventListener(
      "focus",
      () => {
        window.setTimeout(() => {
          if (!settled) finish(input.files);
        }, 350);
      },
      { once: true }
    );

    document.body.appendChild(input);
    input.click();
  });
}

export async function pickAndRegisterModel(
  kind: ModelKind
): Promise<ModelInfo | null> {
  if (isTauriRuntime()) {
    const extensions =
      kind === "llm"
        ? ["gguf"]
        : kind === "asr"
        ? ["bin"]
        : kind === "embed"
        ? ["onnx"]
        : ["json"];
    const filterName =
      kind === "llm"
        ? "GGUF Language Model"
        : kind === "asr"
        ? "Whisper GGML Model"
        : kind === "embed"
        ? "ONNX Embedding Model"
        : "Live2D Model";
    const selected = await open({
      title: `Select ${filterName} File`,
      multiple: false,
      directory: false,
      filters: [{ name: filterName, extensions }],
    });
    if (!selected || Array.isArray(selected)) return null;
    return invokeCommand("register_custom_model", { kind, path: selected });
  }

  const accept =
    kind === "llm"
      ? ".gguf"
      : kind === "asr"
      ? ".bin"
      : kind === "embed"
      ? ".onnx"
      : ".json";
  const files = await openBrowserFilePicker({ accept });
  if (!files || files.length === 0) return null;
  const file = files[0];
  return invokeCommand("register_custom_model", {
    kind,
    path: `/Users/local/Models/${file.name}`,
    name: file.name,
  });
}

export async function pickLive2dFolder(): Promise<{
  model: ModelInfo | null;
  localFiles: Live2dFolderFiles | null;
}> {
  if (isTauriRuntime()) {
    const selected = await open({
      title: "Select Live2D (.model3.json) Character Folder",
      directory: true,
      multiple: false,
    });
    if (!selected || Array.isArray(selected)) {
      return { model: null, localFiles: null };
    }
    const model = await invokeCommand("load_live2d_folder", { folderPath: selected });
    return { model, localFiles: null };
  }

  const files = await openBrowserFilePicker({ directory: true });
  if (!files || files.length === 0) {
    return { model: null, localFiles: null };
  }
  const list = Array.from(files);
  const firstRel = list[0]?.webkitRelativePath || list[0]?.name || "CustomLive2D";
  const folderName = firstRel.split("/")[0] || "CustomLive2D";
  const model = await invokeCommand("load_live2d_folder", {
    folderPath: `/local/live2d/${folderName}`,
  });
  return {
    model,
    localFiles: { folderName, files: list },
  };
}

export async function exportSoulMemoryWithPicker(): Promise<string | null> {
  if (isTauriRuntime()) {
    const targetPath = await save({
      title: "Export Soul Memory Snapshot",
      defaultPath: "soul_memory.sqlite",
      filters: [{ name: "SQLite Database", extensions: ["sqlite", "db"] }],
    });
    if (!targetPath) return null;
    return invokeCommand("export_soul_memory", { targetPath });
  }

  return invokeCommand("export_soul_memory", {
    targetPath: "soul_memory.sqlite.json",
  });
}

export async function importSoulMemoryWithPicker(): Promise<number | null> {
  if (isTauriRuntime()) {
    const sourcePath = await open({
      title: "Import Soul Memory Snapshot",
      multiple: false,
      directory: false,
      filters: [{ name: "SQLite Database", extensions: ["sqlite", "db"] }],
    });
    if (!sourcePath || Array.isArray(sourcePath)) return null;
    return invokeCommand("import_soul_memory", { sourcePath });
  }

  const files = await openBrowserFilePicker({ accept: ".sqlite,.db,.json" });
  if (!files || files.length === 0) return null;
  const file = files[0];
  if (file.name.endsWith(".json")) {
    const text = await file.text();
    const count = mockBridge.importPreviewSnapshotJson(text);
    return count;
  }
  return invokeCommand("import_soul_memory", { sourcePath: file.name });
}

export async function pickMcpConfigFile(): Promise<string | null> {
  if (isTauriRuntime()) {
    const selected = await open({
      title: "Select MCP Configuration (.json)",
      multiple: false,
      directory: false,
      filters: [{ name: "MCP Config JSON", extensions: ["json"] }],
    });
    if (!selected || Array.isArray(selected)) return null;
    return selected;
  }

  const files = await openBrowserFilePicker({ accept: ".json" });
  if (!files || files.length === 0) return null;
  const file = files[0];
  return `/Users/local/.config/mcp/${file.name}`;
}
