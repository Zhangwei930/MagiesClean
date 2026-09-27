<script setup lang="ts">
import { t } from "../i18n";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import DetectionPanel from "../components/DetectionPanel.vue";
import FileList from "../components/FileList.vue";
import Icon from "../components/Icon.vue";
import PdfViewer from "../components/PdfViewer.vue";
import PreviewCanvas from "../components/PreviewCanvas.vue";
import StatusBar from "../components/StatusBar.vue";
import { api } from "../services/ipc";
import { useTask } from "../stores/task";
import { useWorkspace } from "../stores/workspace";
import type { MaskOp } from "../types";

const ws = useWorkspace();
const task = useTask();
const canvas = ref<InstanceType<typeof PreviewCanvas> | null>(null);
const activeId = ref<string | null>(null);
const passwords = ref<Record<string, string>>({});

const file = computed(() => ws.selected);
const busy = computed(() => !!file.value && ws.previewBusy === file.value.id);

watch(
  () => ws.selectedId,
  () => {
    activeId.value = null;
  },
);

async function maskOp(op: MaskOp) {
  if (!file.value) return;
  try {
    ws.upsert(await api.updateMask(file.value.id, [op]));
    ws.refreshSummarySoon();
  } catch (e) {
    ws.handleError(e, t("toast.maskFailed"));
  }
}

async function undo(redo: boolean) {
  if (!file.value) return;
  try {
    ws.upsert(await api.undoMask(file.value.id, redo));
  } catch {
    /* 没有可撤销的内容 */
  }
}

async function unlock(pw: string) {
  if (!file.value) return;
  passwords.value[file.value.id] = pw;
  try {
    task.beginScan(1);
    await api.setPdfPassword(file.value.id, pw);
    await task.refresh();
  } catch (e) {
    ws.handleError(e, t("panel.unlockFailed"));
  }
}

function editMask() {
  canvas.value?.edit();
}

// 快捷键：Space 平移、+/- 缩放、B 原图/结果、M Mask、Delete 忽略候选、⌘Z/⇧⌘Z 撤销/重做、↑/↓ 切换文件
function onKey(e: KeyboardEvent) {
  const t = e.target as HTMLElement;
  if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t instanceof HTMLSelectElement) return;
  const mod = e.metaKey || e.ctrlKey;
  if (mod && e.key.toLowerCase() === "z") {
    e.preventDefault();
    undo(e.shiftKey);
    return;
  }
  if (mod) return;
  switch (e.key) {
    case "+":
    case "=":
      canvas.value?.zoomIn();
      break;
    case "-":
    case "_":
      canvas.value?.zoomOut();
      break;
    case "0":
      canvas.value?.fit();
      break;
    case "1":
      canvas.value?.actual();
      break;
    case "b":
    case "B":
      canvas.value?.toggleBefore();
      break;
    case "m":
    case "M":
      canvas.value?.toggleMask();
      break;
    case "Delete":
    case "Backspace":
      if (file.value && activeId.value) ws.setAction(file.value.id, activeId.value, "ignore");
      break;
    case "ArrowDown":
    case "j":
      e.preventDefault();
      ws.selectRelative(1);
      break;
    case "ArrowUp":
    case "k":
      e.preventDefault();
      ws.selectRelative(-1);
      break;
  }
}
onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));
</script>

<template>
  <div class="workspace">
    <div class="main">
      <FileList />
      <template v-if="file">
        <PdfViewer
          v-if="file.kind === 'pdf'"
          :file="file"
          :active-id="activeId"
          :busy="busy"
          :password="passwords[file.id]"
          @select="activeId = $event"
          @generate="ws.preview(file.id)"
        />
        <PreviewCanvas
          v-else-if="file.preview"
          ref="canvas"
          :file="file"
          :active-id="activeId"
          :busy="busy"
          @select="activeId = $event"
          @generate="ws.preview(file.id)"
          @mask-op="maskOp"
          @undo="undo"
        />
        <div v-else class="placeholder">
          <Icon :name="file.status === 'failed' ? 'x-circle' : file.status === 'waiting' && !task.scan ? 'image' : 'loader'" :size="22" :class="{ spin: file.status !== 'failed' && !(file.status === 'waiting' && !task.scan) }" />
          <span>{{ file.status === "failed" ? file.error?.message ?? t("preview.failed") : file.status === "waiting" && !task.scan ? t("preview.waiting") : t("preview.reading") }}</span>
        </div>
        <DetectionPanel :file="file" :active-id="activeId" @select="activeId = $event" @edit-mask="editMask" @generate="ws.preview(file.id)" @password="unlock" />
      </template>
      <div v-else class="placeholder">
        <Icon name="image" :size="22" />
        <span>{{ t("preview.select") }}</span>
      </div>
    </div>
    <StatusBar />
  </div>
</template>

<style scoped>
.workspace {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}
.main {
  flex: 1;
  min-height: 0;
  display: flex;
}
.placeholder {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--text-2);
  background: var(--canvas);
}
</style>
