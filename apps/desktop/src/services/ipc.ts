// Typed IPC client：前端只通过这里调用后端命令。
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { t } from "../i18n";
import type {
  AppInfo,
  AppSettings,
  EngineEvent,
  ErrorView,
  FileView,
  HistoryRecord,
  ImportResult,
  JobRecord,
  MaskOp,
  ModelStatus,
  Preset,
  TaskStatus,
  UserAction,
  WorkspaceSummary,
} from "../types";

export class IpcError extends Error {
  constructor(public view: ErrorView) {
    super(view.message);
  }
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    if (e && typeof e === "object" && "code" in (e as object)) throw new IpcError(e as ErrorView);
    throw new IpcError({ code: "E_INTERNAL", kind: "internal", message: String(e), retryable: true, nextStep: t("common.tryAgain") });
  }
}

export const api = {
  importFiles: (paths: string[]) => call<ImportResult>("import_files", { paths }),
  importFolder: (path: string) => call<ImportResult>("import_folder", { path }),
  scanFiles: (ids?: string[]) => call<string>("scan_files", { ids: ids ?? null }),
  scanFile: (id: string) => call<string>("scan_file", { id }),
  listFiles: () => call<FileView[]>("list_files"),
  getFileInfo: (id: string) => call<FileView>("get_file_info", { id }),
  workspaceSummary: () => call<WorkspaceSummary>("workspace_summary"),
  generatePreview: (id: string) => call<FileView>("generate_preview", { id }),
  warmPreview: (id: string) => call<void>("warm_preview", { id }),
  updateMask: (id: string, ops: MaskOp[]) => call<FileView>("update_mask", { id, ops }),
  undoMask: (id: string, redo: boolean) => call<FileView>("undo_mask", { id, redo }),
  setCandidateAction: (id: string, candidateId: string, action: UserAction) => call<FileView>("set_candidate_action", { id, candidateId, action }),
  resolvePending: (id: string, action: UserAction) => call<FileView>("resolve_pending", { id, action }),
  applyToSimilar: (id: string, candidateId: string) => call<string>("apply_to_similar", { id, candidateId }),
  setPdfPassword: (id: string, password: string) => call<string>("set_pdf_password", { id, password }),
  confirmSignature: (id: string) => call<FileView>("confirm_signature", { id }),
  removeFiles: (ids: string[]) => call<void>("remove_files", { ids }),
  clearWorkspace: () => call<void>("clear_workspace"),
  startBatch: (ids?: string[], includePending = false) => call<string>("start_batch", { ids: ids ?? null, includePending }),
  pauseBatch: () => call<void>("pause_batch"),
  resumeBatch: () => call<void>("resume_batch"),
  cancelBatch: () => call<void>("cancel_batch"),
  activeTask: () => call<TaskStatus | null>("active_task"),
  exportResult: (id: string) => call<FileView>("export_result", { id }),
  getSettings: () => call<AppSettings>("get_settings"),
  updateSettings: (settings: AppSettings) => call<AppSettings>("update_settings", { settings }),
  getPresets: () => call<Preset[]>("get_presets"),
  savePreset: (preset: Preset) => call<Preset[]>("save_preset", { preset }),
  deletePreset: (id: string) => call<Preset[]>("delete_preset", { id }),
  applyPreset: (id: string) => call<AppSettings>("apply_preset", { id }),
  getModelStatus: () => call<ModelStatus[]>("get_model_status"),
  reloadModels: () => call<ModelStatus[]>("reload_models"),
  getHistory: (limit = 200, offset = 0) => call<HistoryRecord[]>("get_history", { limit, offset }),
  clearHistory: () => call<void>("clear_history"),
  recentJobs: () => call<JobRecord[]>("recent_jobs"),
  recoverableJobs: () => call<JobRecord[]>("recoverable_jobs"),
  resumeJob: (id: string) => call<ImportResult>("resume_job", { id }),
  discardJob: (id: string) => call<void>("discard_job", { id }),
  appInfo: () => call<AppInfo>("app_info"),
  clearCache: () => call<number>("clear_cache"),
  revealPath: (path: string) => call<void>("reveal_path", { path }),
  openPath: (path: string) => call<void>("open_path", { path }),
  openPrivacySettings: () => call<void>("open_privacy_settings"),
};

const EVENTS: EngineEvent["event"][] = [
  "scan-progress",
  "scan-completed",
  "processing-progress",
  "item-completed",
  "item-failed",
  "batch-completed",
  "file-updated",
  "model-loading",
  "model-ready",
  "warning",
];

/** 订阅全部后端事件。 */
export async function onEngineEvent(handler: (e: EngineEvent) => void): Promise<UnlistenFn> {
  const offs = await Promise.all(EVENTS.map((name) => listen<EngineEvent>(name, (ev) => handler(ev.payload))));
  return () => offs.forEach((f) => f());
}

/** 缓存文件 → asset URL（WebView 只能读取缓存目录与用户导入的 PDF）。 */
export function assetUrl(path: string | null | undefined): string | undefined {
  return path ? convertFileSrc(path) : undefined;
}
