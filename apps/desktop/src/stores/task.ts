import { defineStore } from "pinia";
import { computed, ref } from "vue";
import { api } from "../services/ipc";
import type { BatchSummary, FileView, TaskStatus } from "../types";

export interface Progress {
  done: number;
  total: number;
  failed: number;
  phase?: string;
  startedAt: number;
}

export const useTask = defineStore("task", () => {
  const active = ref<TaskStatus | null>(null);
  const scan = ref<Progress | null>(null);
  const processing = ref<Progress | null>(null);
  const paused = ref(false);
  const lastSummary = ref<BatchSummary | null>(null);
  const recent = ref<FileView[]>([]);

  const busy = computed(() => active.value !== null);

  async function refresh() {
    active.value = await api.activeTask();
    paused.value = active.value?.paused ?? false;
  }

  function beginScan(total: number) {
    scan.value = { done: 0, total, failed: 0, phase: "learn", startedAt: Date.now() };
  }

  function beginProcessing(total: number) {
    processing.value = { done: 0, total, failed: 0, startedAt: Date.now() };
    lastSummary.value = null;
    recent.value = [];
    paused.value = false;
  }

  function pushRecent(f: FileView) {
    recent.value = [f, ...recent.value.filter((x) => x.id !== f.id)].slice(0, 40);
  }

  async function pause() {
    await api.pauseBatch();
    paused.value = true;
  }
  async function resume() {
    await api.resumeBatch();
    paused.value = false;
  }
  async function cancel() {
    await api.cancelBatch();
  }

  return { active, scan, processing, paused, lastSummary, recent, busy, refresh, beginScan, beginProcessing, pushRecent, pause, resume, cancel };
});
