// 与 Rust 端 serde 序列化保持一致的 typed IPC 契约（wm-runtime::model / wm-core）。

export type FileKind = "image" | "pdf";
export type FileStatus = "waiting" | "scanning" | "detected" | "clean" | "needs_review" | "processing" | "completed" | "failed";
export type JobState = "queued" | "scanning" | "ready" | "processing" | "completed" | "failed" | "cancelled";
export type ReviewStatus = "none" | "needs_review" | "approved";
export type ProcessingStage = "queued" | "decode" | "analyze" | "detect" | "segment" | "remove" | "evaluate" | "encode" | "commit" | "done";
export type WatermarkType = "text" | "logo" | "transparent" | "repeated" | "pdf_native" | "info_stamp" | "unknown";
export type CandidateDecision = "auto" | "review" | "ignore";
export type UserAction = "pending" | "remove" | "ignore";
export type RemovalRoute = "pdf_object_remove" | "alpha_restore" | "fast_inpaint" | "texture_synthesis" | "ai_inpaint";

export interface BoundingBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Evidence {
  source: string;
  name: string;
  value: number;
}

export interface CandidateView {
  id: string;
  watermarkType: WatermarkType;
  typeLabel: string;
  confidence: number;
  bbox: BoundingBox;
  rotation: number;
  opacity: number | null;
  text: string | null;
  sources: string[];
  evidence: Evidence[];
  decision: CandidateDecision;
  userAction: UserAction;
  willRemove: boolean;
  awaitingReview: boolean;
  page: number | null;
  batchProfileId: string | null;
}

export type MediaInfo =
  | { type: "image"; width: number; height: number; format: string; hasAlpha: boolean }
  | { type: "pdf"; pageCount: number; pdfKind: string; encrypted: boolean; signed: boolean; pages: [number, number][] };

export interface DecisionSummary {
  total: number;
  toRemove: number;
  needsReview: number;
  ignored: number;
}

export interface RouteDecision {
  route: RemovalRoute;
  reason: string;
}

export interface QualityIssue {
  kind: "residual" | "blur" | "color_shift" | "edge_artifact" | "outside_change" | "structure";
  severity: number;
  message: string;
}

export interface QualityReport {
  score: number;
  passed: boolean;
  issues: QualityIssue[];
}

export interface ErrorView {
  code: string;
  kind: string;
  message: string;
  retryable: boolean;
  nextStep: string;
}

export interface FileView {
  id: string;
  path: string;
  name: string;
  kind: FileKind;
  size: number;
  info: MediaInfo | null;
  status: FileStatus;
  state: JobState;
  review: ReviewStatus;
  stage: ProcessingStage;
  progress: number;
  candidates: CandidateView[];
  summary: DecisionSummary;
  thumb: string | null;
  preview: string | null;
  previewSize: [number, number] | null;
  maskOverlay: string | null;
  resultPreview: string | null;
  resultFile: string | null;
  resultCurrent: boolean;
  route: RouteDecision | null;
  quality: QualityReport | null;
  output: string | null;
  error: ErrorView | null;
  notes: string[];
  needsPassword: boolean;
  signatureConfirmed: boolean;
  maskVersion: number;
  batchMatched: boolean;
  hasManualMask: boolean;
  canUndoMask: boolean;
  canRedoMask: boolean;
}

export interface WorkspaceSummary {
  total: number;
  withWatermark: number;
  needsReview: number;
  readyToProcess: number;
  completed: number;
  failed: number;
  candidatesToRemove: number;
  candidatesPending: number;
}

export interface ImportResult {
  added: FileView[];
  duplicates: number;
  unsupported: string[];
  changed: string[];
  /** 导入时就无法读取的文件（例如 macOS 上其它应用的受保护目录） */
  blocked: string[];
}

export interface BatchSummary {
  jobId: string;
  total: number;
  completed: number;
  failed: number;
  needsReview: number;
  skipped: number;
  cancelled: boolean;
  outputDir: string | null;
  elapsedMs: number;
}

export interface TaskStatus {
  id: string;
  kind: "scan" | "process" | "similar";
  paused: boolean;
}

export type Point = { x: number; y: number };
export type MaskOp =
  | { op: "brush"; points: Point[]; radius: number; erase: boolean }
  | { op: "rect"; x: number; y: number; width: number; height: number; erase: boolean }
  | { op: "polygon"; points: Point[]; erase: boolean }
  | { op: "clear" };

// ── 设置 ──
export type AutoMode = "conservative" | "standard" | "aggressive";
export type QualityMode = "fast" | "balanced" | "best";
export type OutputFormat = "same" | "jpeg" | "png" | "webp" | "bmp" | "tiff";
export type JpegQuality = "preserve" | "q90" | "q95" | "q100";
export type ConflictPolicy = "number" | "skip" | "replace_output";
export type Theme = "system" | "light" | "dark";
export type Language = "en" | "zh-CN";

export interface AutoThresholds {
  auto: number;
  review: number;
}

export interface AppSettings {
  autoMode: AutoMode;
  qualityMode: QualityMode;
  thresholds: { conservative: AutoThresholds; standard: AutoThresholds; aggressive: AutoThresholds };
  mask: { maskThreshold: number; maskDilation: number; maskFeather: number; minComponentSize: number };
  router: { smallMaskAreaRatio: number; simpleSceneComplexity: number; alphaQualityMin: number; qualityReviewThreshold: number };
  output: {
    format: OutputFormat;
    jpegQuality: JpegQuality;
    suffix: string;
    outputDir: string | null;
    preserveStructure: boolean;
    conflict: ConflictPolicy;
    keepMetadata: boolean;
    overwriteOriginals: boolean;
  };
  performance: { cpuWorkers: number | null; gpuWorkers: number; memoryBudgetMb: number | null };
  batch: { sampleOverride: number | null; enabled: boolean };
  theme: Theme;
  language: Language;
  advancedMode: boolean;
}

export interface Preset {
  id: string;
  name: string;
  detectionMode: AutoMode;
  confidenceThreshold: number;
  removalQuality: QualityMode;
  outputFormat: OutputFormat;
  builtin: boolean;
}

export type ModelStateKind = "ready" | "missing" | "not_packaged" | "checksum_mismatch" | "load_failed" | "runtime_unavailable";

export interface ModelStatus {
  id: string;
  role: "detector" | "segmenter" | "ocr_detector" | "ocr_recognizer" | "inpainting";
  version: string;
  file: string;
  state: { state: ModelStateKind; reason?: string };
  provider: string;
}

export interface HistoryRecord {
  id: string;
  jobId: string | null;
  input: string;
  output: string | null;
  kind: string;
  route: string | null;
  quality: number | null;
  candidates: number;
  status: string;
  createdAt: number;
}

export interface JobRecord {
  id: string;
  createdAt: number;
  updatedAt: number;
  state: string;
  total: number;
  completed: number;
  failed: number;
  needsReview: number;
  outputDir: string | null;
}

export interface AppInfo {
  version: string;
  dataDir: string;
  cacheDir: string;
  modelsDir: string;
  logDir: string;
  cacheBytes: number;
  platform: string;
  arch: string;
}

// ── 事件 ──
export type EngineEvent =
  | { event: "scan-progress"; taskId: string; done: number; total: number; phase: string }
  | { event: "scan-completed"; taskId: string; total: number; withWatermark: number; needsReview: number; profiles: number; cancelled: boolean }
  | { event: "processing-progress"; taskId: string; done: number; total: number; failed: number; paused: boolean }
  | { event: "item-completed"; taskId: string; file: FileView }
  | { event: "item-failed"; taskId: string; file: FileView }
  | { event: "batch-completed"; summary: BatchSummary }
  | { event: "file-updated"; file: FileView }
  | { event: "model-loading" }
  | { event: "model-ready" }
  | { event: "warning"; message: string };
