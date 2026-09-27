import { defineStore } from "pinia";
import { computed, ref, shallowRef, triggerRef, watch } from "vue";
import { api, IpcError } from "../services/ipc";
import type { EngineEvent, FileStatus, FileView, UserAction, WorkspaceSummary } from "../types";
import { useTask } from "./task";
import { useUi } from "./ui";
import { t } from "../i18n";

export type Filter = "all" | "needs_review" | "detected" | "clean" | "completed" | "failed" | "waiting";

export const FILTERS: { key: Filter; match: (s: FileStatus) => boolean }[] = [
  { key: "all", match: () => true },
  { key: "needs_review", match: (s) => s === "needs_review" },
  { key: "detected", match: (s) => s === "detected" },
  { key: "completed", match: (s) => s === "completed" },
  { key: "failed", match: (s) => s === "failed" },
  { key: "clean", match: (s) => s === "clean" },
  { key: "waiting", match: (s) => s === "waiting" || s === "scanning" || s === "processing" },
];

export const useWorkspace = defineStore("workspace", () => {
  // 大批量文件：用 shallowRef + Map 避免深度响应带来的开销
  const files = shallowRef(new Map<string, FileView>());
  const order = ref<string[]>([]);
  const selectedId = ref<string | null>(null);
  const filter = ref<Filter>("all");
  const search = ref("");
  const summary = ref<WorkspaceSummary | null>(null);
  const previewBusy = ref<string | null>(null);

  const list = computed(() => {
    const f = FILTERS.find((x) => x.key === filter.value)!;
    const q = search.value.trim().toLowerCase();
    return order.value
      .map((id) => files.value.get(id))
      .filter((x): x is FileView => !!x && f.match(x.status) && (!q || x.name.toLowerCase().includes(q)));
  });

  const counts = computed(() => {
    const c: Record<Filter, number> = { all: 0, needs_review: 0, detected: 0, clean: 0, completed: 0, failed: 0, waiting: 0 };
    for (const id of order.value) {
      const f = files.value.get(id);
      if (!f) continue;
      for (const flt of FILTERS) if (flt.match(f.status)) c[flt.key]++;
    }
    return c;
  });

  const selected = computed(() => (selectedId.value ? files.value.get(selectedId.value) ?? null : null));

  // 勾选（批量操作的对象）：与“当前查看的文件” selectedId 相互独立
  const checked = ref(new Set<string>());
  const checkedIds = computed(() => order.value.filter((id) => checked.value.has(id)));
  function toggleChecked(id: string, on?: boolean) {
    const next = new Set(checked.value);
    if (on ?? !next.has(id)) next.add(id);
    else next.delete(id);
    checked.value = next;
  }
  function setChecked(ids: string[], on: boolean) {
    const next = new Set(checked.value);
    for (const id of ids) {
      if (on) next.add(id);
      else next.delete(id);
    }
    checked.value = next;
  }
  function clearChecked() {
    checked.value = new Set();
  }

  // 批量处理的对象：有勾选时只处理勾选的文件，否则处理全部。
  // ready：可以直接处理；pending：有待复核候选（用户选择“一起去除”后也会处理）
  const batch = computed(() => {
    const ids = checked.value.size ? checkedIds.value : order.value;
    const ready: FileView[] = [];
    const pending: FileView[] = [];
    for (const id of ids) {
      const f = files.value.get(id);
      if (!f || f.state !== "ready" || f.needsPassword) continue;
      if (f.summary.needsReview > 0) pending.push(f);
      else if (f.review !== "needs_review" && (f.summary.toRemove > 0 || f.hasManualMask)) ready.push(f);
    }
    return { ids, ready, pending, onlyChecked: checked.value.size > 0 };
  });

  function upsert(f: FileView) {
    const isNew = !files.value.has(f.id);
    files.value.set(f.id, f);
    if (isNew && !order.value.includes(f.id)) order.value.push(f.id);
    triggerRef(files);
  }

  let summaryTimer: number | undefined;
  function refreshSummarySoon() {
    clearTimeout(summaryTimer);
    summaryTimer = window.setTimeout(async () => {
      summary.value = await api.workspaceSummary();
    }, 150);
  }

  async function load() {
    const all = await api.listFiles();
    files.value = new Map(all.map((f) => [f.id, f]));
    order.value = all.map((f) => f.id);
    if (!selectedId.value || !files.value.has(selectedId.value)) selectedId.value = pickFirst(all);
    summary.value = await api.workspaceSummary();
  }

  /** 默认选中的文件：先看失败的，再看待复核的，否则第一个。 */
  function pickFirst(list: FileView[]): string | null {
    const f = list.find((x) => x.status === "failed") ?? list.find((x) => x.status === "needs_review") ?? list[0];
    return f?.id ?? null;
  }

  function handleError(e: unknown, title: string) {
    const ui = useUi();
    if (e instanceof IpcError) ui.toast({ tone: "danger", title, body: `${e.view.message}. ${e.view.nextStep}` }, 7000);
    else ui.toast({ tone: "danger", title, body: String(e) });
  }

  /** 导入并立即开始自动扫描。 */
  async function importPaths(paths: string[]) {
    const ui = useUi();
    const task = useTask();
    if (!paths.length) return;
    try {
      const r = await api.importFiles(paths);
      r.added.forEach(upsert);
      if (r.unsupported.length) {
        ui.toast({ tone: "warning", title: t("toast.unsupported", { n: r.unsupported.length }), body: t("toast.unsupportedBody") });
      }
      if (r.blocked.length) {
        const mac = navigator.userAgent.includes("Mac");
        ui.toast(
          {
            tone: "danger",
            title: t("toast.blocked", { n: r.blocked.length }),
            body: t("toast.blockedBody"),
            action: mac ? { label: t("toast.privacyAction"), run: () => void api.openPrivacySettings().catch(() => undefined) } : undefined,
          },
          12000,
        );
      }
      if (r.duplicates) ui.toast({ tone: "info", title: t("toast.duplicates", { n: r.duplicates }) });
      if (!r.added.length) return;
      ui.go("workspace");
      if (!selectedId.value || !files.value.has(selectedId.value)) selectedId.value = pickFirst(r.added);
      // 无法读取的文件导入时已标记为失败，不再送去扫描
      const scannable = r.added.filter((f) => f.status !== "failed");
      if (!scannable.length) return;
      if (task.busy) {
        ui.toast({ tone: "info", title: t("toast.added"), body: t("toast.addedBody") });
        return;
      }
      task.beginScan(scannable.length);
      await api.scanFiles(scannable.map((f) => f.id));
      await task.refresh();
    } catch (e) {
      handleError(e, t("toast.importFailed"));
    }
    refreshSummarySoon();
  }

  async function rescan(ids?: string[]) {
    const task = useTask();
    try {
      task.beginScan(ids?.length ?? order.value.length);
      await api.scanFiles(ids);
      await task.refresh();
    } catch (e) {
      handleError(e, t("toast.scanFailed"));
    }
  }

  async function setAction(fileId: string, candidateId: string, action: UserAction) {
    try {
      upsert(await api.setCandidateAction(fileId, candidateId, action));
      refreshSummarySoon();
      schedulePreview(fileId);
    } catch (e) {
      handleError(e, t("panel.actionFailed"));
    }
  }

  async function resolvePending(fileId: string, action: UserAction) {
    try {
      upsert(await api.resolvePending(fileId, action));
      refreshSummarySoon();
      schedulePreview(fileId);
    } catch (e) {
      handleError(e, t("panel.actionFailed"));
    }
  }

  // 自动预览：候选决定变化后稍等片刻再生成，连续点击只生成一次；
  // 生成期间决定又变了，结束后按最新决定重新生成。
  let previewTimer: number | undefined;
  const previewAgain = new Set<string>();
  function schedulePreview(fileId: string) {
    clearTimeout(previewTimer);
    previewTimer = window.setTimeout(() => {
      const f = files.value.get(fileId);
      if (!f || f.kind !== "image" || f.resultCurrent || f.state === "processing") return;
      if (!f.summary.toRemove && !f.hasManualMask) return;
      if (previewBusy.value === fileId) {
        previewAgain.add(fileId);
        return;
      }
      if (previewBusy.value) return;
      void preview(fileId);
    }, 400);
  }

  async function preview(fileId: string) {
    previewBusy.value = fileId;
    try {
      upsert(await api.generatePreview(fileId));
    } catch (e) {
      handleError(e, t("toast.previewFailed"));
    } finally {
      previewBusy.value = null;
      if (previewAgain.delete(fileId)) schedulePreview(fileId);
    }
  }

  // 后台预计算：选中已扫描、有候选的图片时，趁用户复核的空档提前算好 AI 修复结果
  const WARM_STATUSES: FileStatus[] = ["needs_review", "detected"];
  function warm(fileId: string | null) {
    const f = fileId ? files.value.get(fileId) : undefined;
    if (!f || f.kind !== "image" || !WARM_STATUSES.includes(f.status) || useTask().processing) return;
    api.warmPreview(f.id).catch(() => {
      /* 预计算只是加速，失败不影响正常预览 */
    });
  }
  watch(
    () => {
      const f = selected.value;
      return f ? `${f.id}:${f.status}:${f.maskVersion}` : "";
    },
    () => warm(selectedId.value),
  );

  async function exportOne(fileId: string) {
    const ui = useUi();
    previewBusy.value = fileId;
    try {
      const f = await api.exportResult(fileId);
      upsert(f);
      if (f.output) {
        ui.toast({ tone: "success", title: t("toast.exported"), body: f.output, action: { label: t("toast.revealAction"), run: () => api.revealPath(f.output!) } });
      }
    } catch (e) {
      handleError(e, t("toast.exportFailed"));
    } finally {
      previewBusy.value = null;
      refreshSummarySoon();
    }
  }

  async function remove(ids: string[]) {
    try {
      await api.removeFiles(ids);
      for (const id of ids) files.value.delete(id);
      order.value = order.value.filter((x) => !ids.includes(x));
      setChecked(ids, false);
      triggerRef(files);
      if (selectedId.value && ids.includes(selectedId.value)) selectedId.value = order.value[0] ?? null;
      refreshSummarySoon();
    } catch (e) {
      handleError(e, t("toast.removeFailed"));
    }
  }

  async function clear() {
    try {
      await api.clearWorkspace();
      files.value = new Map();
      order.value = [];
      selectedId.value = null;
      clearChecked();
      summary.value = null;
      useUi().go("home");
    } catch (e) {
      handleError(e, t("toast.clearFailed"));
    }
  }

  function select(id: string | null) {
    selectedId.value = id;
  }

  function selectRelative(delta: number) {
    const l = list.value;
    if (!l.length) return;
    const i = l.findIndex((f) => f.id === selectedId.value);
    const n = Math.max(0, Math.min(l.length - 1, (i < 0 ? 0 : i) + delta));
    selectedId.value = l[n].id;
  }

  /** 处理后端事件。 */
  function onEvent(e: EngineEvent) {
    const task = useTask();
    const ui = useUi();
    switch (e.event) {
      case "file-updated":
        upsert(e.file);
        refreshSummarySoon();
        break;
      case "scan-progress":
        task.scan = { ...(task.scan ?? { failed: 0, startedAt: Date.now() }), done: e.done, total: e.total, phase: e.phase };
        break;
      case "scan-completed":
        task.scan = null;
        task.refresh();
        refreshSummarySoon();
        if (!e.cancelled && e.total > 0 && e.profiles !== 1) {
          ui.toast({
            tone: "success",
            title: t("toast.scanDone"),
            body:
              t("toast.scanBody", { w: e.withWatermark, n: e.total }) +
              (e.needsReview ? t("toast.scanReview", { n: e.needsReview }) : "") +
              (e.profiles ? t("toast.scanProfiles", { n: e.profiles }) : ""),
          });
        }
        break;
      case "processing-progress":
        task.processing = { ...(task.processing ?? { startedAt: Date.now() }), done: e.done, total: e.total, failed: e.failed };
        task.paused = e.paused;
        break;
      case "item-completed":
      case "item-failed":
        upsert(e.file);
        task.pushRecent(e.file);
        break;
      case "batch-completed":
        task.lastSummary = e.summary;
        task.processing = null;
        task.refresh();
        refreshSummarySoon();
        break;
      case "warning":
        ui.toast({ tone: "warning", title: e.message });
        break;
      default:
        break;
    }
  }

  return {
    files,
    order,
    selectedId,
    selected,
    checked,
    checkedIds,
    toggleChecked,
    setChecked,
    clearChecked,
    batch,
    filter,
    search,
    summary,
    list,
    counts,
    previewBusy,
    load,
    upsert,
    importPaths,
    rescan,
    setAction,
    resolvePending,
    preview,
    exportOne,
    remove,
    clear,
    select,
    selectRelative,
    onEvent,
    refreshSummarySoon,
    handleError,
  };
});
