<script setup lang="ts">
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { onBeforeUnmount, onMounted, ref } from "vue";
import ExportDialog from "./components/ExportDialog.vue";
import Icon from "./components/Icon.vue";
import Modal from "./components/Modal.vue";
import Toasts from "./components/Toasts.vue";
import TopBar from "./components/TopBar.vue";
import { t } from "./i18n";
import { onEngineEvent } from "./services/ipc";
import { useSettings } from "./stores/settings";
import { useTask } from "./stores/task";
import { useUi } from "./stores/ui";
import { useWorkspace } from "./stores/workspace";
import BatchView from "./views/BatchView.vue";
import HistoryView from "./views/HistoryView.vue";
import HomeView from "./views/HomeView.vue";
import SettingsView from "./views/SettingsView.vue";
import WorkspaceView from "./views/WorkspaceView.vue";

const ui = useUi();
const ws = useWorkspace();
const st = useSettings();
const task = useTask();
const confirmClose = ref(false);
const offs: (() => void)[] = [];

const media = window.matchMedia("(prefers-color-scheme: dark)");
const onScheme = () => st.settings && ui.applyTheme(st.settings.theme);

onMounted(async () => {
  media.addEventListener("change", onScheme);
  offs.push(await onEngineEvent(ws.onEvent));
  await st.load();
  await ws.load();
  await task.refresh();
  if (ws.order.length) ui.go("workspace");

  // 拖放导入（Tauri 提供真实文件路径）
  offs.push(
    await getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "enter" || p.type === "over") ui.dragging = true;
      else if (p.type === "leave") ui.dragging = false;
      else if (p.type === "drop") {
        ui.dragging = false;
        ws.importPaths(p.paths);
      }
    }),
  );

  // 有任务在运行时关闭窗口需要确认
  const win = getCurrentWindow();
  offs.push(
    await win.onCloseRequested(async (e) => {
      if (task.busy && !confirmClose.value) {
        e.preventDefault();
        confirmClose.value = true;
      }
    }),
  );
});

onBeforeUnmount(() => {
  media.removeEventListener("change", onScheme);
  offs.forEach((f) => f());
});

async function quit() {
  await task.cancel().catch(() => {});
  await getCurrentWindow().destroy();
}
</script>

<template>
  <div class="app">
    <TopBar />
    <main class="view">
      <HomeView v-if="ui.view === 'home'" />
      <WorkspaceView v-else-if="ui.view === 'workspace'" />
      <BatchView v-else-if="ui.view === 'batch'" />
      <HistoryView v-else-if="ui.view === 'history'" />
      <SettingsView v-else-if="ui.view === 'settings'" />
    </main>

    <Transition name="fade">
      <div v-if="ui.dragging && ui.view !== 'home'" class="drop-overlay">
        <div class="drop-card">
          <Icon name="upload" :size="30" :stroke="2" />
          <b>{{ t("drop.release") }}</b>
          <span>{{ t("drop.releaseHint") }}</span>
        </div>
      </div>
    </Transition>

    <ExportDialog v-if="ui.exportDialog" />
    <Toasts />

    <Modal v-if="confirmClose" :title="t('toast.closeTitle')" icon="alert" tone="warning" @close="confirmClose = false">
      {{ t("toast.closeBody") }}
      <template #footer>
        <button class="btn" @click="confirmClose = false">{{ t("toast.keepRunning") }}</button>
        <button class="btn danger solid" @click="quit">{{ t("toast.closeConfirm") }}</button>
      </template>
    </Modal>
  </div>
</template>

<style scoped>
.app {
  height: 100%;
  display: flex;
  flex-direction: column;
}
.view {
  flex: 1;
  min-height: 0;
  display: flex;
}
.drop-overlay {
  position: fixed;
  inset: 0;
  z-index: 150;
  display: grid;
  place-items: center;
  background: color-mix(in srgb, var(--accent) 12%, var(--overlay));
  backdrop-filter: blur(4px);
  pointer-events: none;
}
.drop-card {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 36px 56px;
  border-radius: 24px;
  border: 2px dashed var(--accent);
  background: var(--surface);
  color: var(--accent-text);
  box-shadow: var(--shadow-lg);
}
.drop-card b {
  font-size: var(--fs-20);
  color: var(--text);
}
.drop-card span {
  color: var(--text-2);
}
</style>
