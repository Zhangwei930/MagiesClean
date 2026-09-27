import { t, type MessageKey } from "../i18n";
import type { AutoMode, FileStatus, QualityMode, RemovalRoute, WatermarkType } from "../types";

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}

export const pct = (v: number) => `${Math.round(v * 100)}%`;

export function formatDuration(ms: number): string {
  if (ms < 1000) return t("time.ms", { n: ms });
  const s = ms / 1000;
  if (s < 60) return t("time.s", { n: s.toFixed(1) });
  return t("time.ms2", { m: Math.floor(s / 60), s: Math.round(s % 60) });
}

export function formatTime(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

export function dirname(p: string): string {
  const parts = p.split(/[\\/]/);
  parts.pop();
  return parts.join(p.includes("\\") ? "\\" : "/");
}

export type Tone = "neutral" | "info" | "accent" | "success" | "warning" | "danger";

export const STATUS_TONE: Record<FileStatus, Tone> = {
  waiting: "neutral",
  scanning: "info",
  detected: "accent",
  clean: "neutral",
  needs_review: "warning",
  processing: "info",
  completed: "success",
  failed: "danger",
};

export const statusLabel = (s: FileStatus) => t(`status.${s}` as MessageKey);

export const TYPE_ICON: Record<WatermarkType, string> = {
  text: "type",
  logo: "badge",
  transparent: "droplet",
  repeated: "grid",
  pdf_native: "file-text",
  info_stamp: "clock",
  unknown: "help",
};

export const AUTO_MODES: AutoMode[] = ["conservative", "standard", "aggressive"];
export const QUALITY_MODES: QualityMode[] = ["fast", "balanced", "best"];

export const autoModeLabel = (m: AutoMode) => t(`autoMode.${m}` as MessageKey);
export const autoModeHint = (m: AutoMode) => t(`autoMode.${m}Hint` as MessageKey);
export const qualityModeLabel = (m: QualityMode) => t(`qualityMode.${m}` as MessageKey);
export const qualityModeHint = (m: QualityMode) => t(`qualityMode.${m}Hint` as MessageKey);
export const routeLabel = (r: RemovalRoute | string) => t(`route.${r}` as MessageKey);

export function evidenceLabel(name: string): string {
  const key = `evidence.${name}` as MessageKey;
  const s = t(key);
  return s === key ? name : s;
}
