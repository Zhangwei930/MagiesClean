<script setup lang="ts">
import { t } from "../i18n";
import { computed } from "vue";
import { useTask } from "../stores/task";
import { useUi, type View } from "../stores/ui";
import { useWorkspace } from "../stores/workspace";
import Icon from "./Icon.vue";

const ui = useUi();
const ws = useWorkspace();
const task = useTask();
const isMac = navigator.userAgent.includes("Mac");

const tabs = computed<{ key: View; label: string; icon: string; badge?: number; disabled?: boolean }[]>(() => [
  { key: ws.order.length ? "workspace" : "home", label: t("top.workspace"), icon: "grid", badge: ws.counts.needs_review || undefined },
  { key: "batch", label: t("top.batch"), icon: "layers", disabled: !task.processing && !task.lastSummary },
  { key: "history", label: t("top.history"), icon: "history" },
  { key: "settings", label: t("top.settings"), icon: "settings" },
]);

const isActive = (k: View) => ui.view === k || (k === "workspace" && ui.view === "home") || (k === "home" && ui.view === "workspace");
</script>

<template>
  <header class="topbar" :class="{ mac: isMac }" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <span class="logo" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="16" height="16"><path d="M12 3.5s5.5 5.8 5.5 10a5.5 5.5 0 0 1-11 0c0-4.2 5.5-10 5.5-10z" fill="#fff" /></svg>
      </span>
      <span class="name" data-tauri-drag-region>Magies Clean</span>
    </div>
    <nav class="tabs">
      <button
        v-for="tab in tabs"
        :key="tab.icon"
        class="tab"
        :class="{ on: isActive(tab.key) }"
        :disabled="tab.disabled"
        @click="ui.go(tab.key)"
      >
        <Icon :name="tab.icon" :size="15" />
        {{ tab.label }}
        <span v-if="tab.badge" class="badge">{{ tab.badge }}</span>
      </button>
    </nav>
    <div class="right" data-tauri-drag-region>
      <div v-if="task.scan" class="activity" @click="ui.go('workspace')">
        <Icon name="loader" :size="14" class="spin" />
        {{ task.scan.phase === "learn" ? t("top.learning") : task.scan.phase === "similar" ? t("top.matching") : t("top.scanning", { done: task.scan.done, total: task.scan.total }) }}
      </div>
      <div v-else-if="task.processing" class="activity" @click="ui.go('batch')">
        <Icon :name="task.paused ? 'pause' : 'loader'" :size="14" :class="{ spin: !task.paused }" />
        {{ task.paused ? t("top.paused") : t("top.processing") }} {{ task.processing.done }}/{{ task.processing.total }}
      </div>
      <span class="local" :title="t('top.localTip')"><Icon name="shield" :size="14" />{{ t("top.local") }}</span>
    </div>
  </header>
</template>

<style scoped>
.topbar {
  height: var(--topbar-h);
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 0 14px;
  background: var(--bg-elev);
  border-bottom: 1px solid var(--border);
  position: relative;
  z-index: 10;
}
.topbar.mac {
  padding-left: 84px;
}
.brand {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 150px;
}
.logo {
  width: 24px;
  height: 24px;
  border-radius: 7px;
  display: grid;
  place-items: center;
  background: var(--brand-gradient);
  box-shadow: 0 2px 6px rgba(84, 70, 240, 0.35);
}
.name {
  font-weight: 700;
  letter-spacing: -0.01em;
  font-size: var(--fs-14);
}
.tabs {
  display: flex;
  gap: 2px;
  padding: 3px;
  border-radius: var(--r-10);
  background: var(--surface-3);
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 28px;
  padding: 0 12px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text-2);
  font-weight: 500;
  cursor: pointer;
  transition: all var(--dur-fast);
}
.tab:hover:not(:disabled) {
  color: var(--text);
}
.tab.on {
  background: var(--surface);
  color: var(--text);
  box-shadow: var(--shadow-sm);
}
.tab:disabled {
  opacity: 0.4;
  cursor: default;
}
.badge {
  min-width: 18px;
  height: 18px;
  padding: 0 5px;
  border-radius: 9px;
  background: var(--warning);
  color: #fff;
  font-size: 10px;
  font-weight: 700;
  display: grid;
  place-items: center;
}
.right {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: 12px;
}
.activity {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 26px;
  padding: 0 10px;
  border-radius: var(--r-pill);
  background: var(--accent-soft);
  color: var(--accent-text);
  font-size: var(--fs-12);
  font-weight: 600;
  cursor: pointer;
  font-variant-numeric: tabular-nums;
}
.local {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: var(--fs-12);
  color: var(--text-3);
}
</style>
