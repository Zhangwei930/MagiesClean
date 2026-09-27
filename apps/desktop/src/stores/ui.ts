import { defineStore } from "pinia";
import { ref } from "vue";
import type { Theme } from "../types";

export type View = "home" | "workspace" | "batch" | "history" | "settings";

export interface Toast {
  id: number;
  tone: "info" | "success" | "warning" | "danger";
  title: string;
  body?: string;
  action?: { label: string; run: () => void };
}

let seq = 0;

export const useUi = defineStore("ui", () => {
  const view = ref<View>("home");
  const toasts = ref<Toast[]>([]);
  const dragging = ref(false);
  const exportDialog = ref(false);

  function go(v: View) {
    view.value = v;
  }

  function toast(t: Omit<Toast, "id">, ms = 4200) {
    const id = ++seq;
    toasts.value.push({ ...t, id });
    if (ms > 0) setTimeout(() => dismiss(id), ms);
    return id;
  }

  function dismiss(id: number) {
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }

  function applyTheme(theme: Theme) {
    const root = document.documentElement;
    const dark = theme === "dark" || (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
    root.dataset.theme = dark ? "dark" : "light";
  }

  return { view, toasts, dragging, exportDialog, go, toast, dismiss, applyTheme };
});
