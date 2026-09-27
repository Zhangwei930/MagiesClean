<script setup lang="ts">
import { t, tx } from "../i18n";
import { open } from "@tauri-apps/plugin-dialog";
import { computed, ref } from "vue";
import { api } from "../services/ipc";
import { useSettings } from "../stores/settings";
import { useTask } from "../stores/task";
import { useUi } from "../stores/ui";
import { useWorkspace } from "../stores/workspace";
import type { OutputFormat } from "../types";
import Icon from "./Icon.vue";
import Modal from "./Modal.vue";
import Toggle from "./Toggle.vue";

const ws = useWorkspace();
const ui = useUi();
const st = useSettings();
const task = useTask();

const s = computed(() => ws.summary);
// 处理对象：有勾选时为勾选的文件，否则为全部文件
const b = computed(() => ws.batch);
const pendingCount = computed(() => b.value.pending.reduce((n, f) => n + f.summary.needsReview, 0));
// 有待复核候选时的选择：一起去除（默认）或跳过这些文件
const includePending = ref(true);
const withPending = computed(() => pendingCount.value > 0 && includePending.value);
const fileCount = computed(() => b.value.ready.length + (withPending.value ? b.value.pending.length : 0));
const wmCount = computed(
  () =>
    b.value.ready.reduce((n, f) => n + f.summary.toRemove, 0) +
    (withPending.value ? b.value.pending.reduce((n, f) => n + f.summary.toRemove + f.summary.needsReview, 0) : 0),
);
const out = computed(() => st.settings?.output);
const FORMATS: OutputFormat[] = ["same", "jpeg", "png", "webp", "tiff"];

async function chooseDir() {
  const r = await open({ directory: true, multiple: false });
  if (typeof r === "string") await st.patch((x) => (x.output.outputDir = r));
}

async function start() {
  try {
    task.beginProcessing(fileCount.value);
    await api.startBatch(b.value.onlyChecked ? b.value.ids : undefined, withPending.value);
    await task.refresh();
    ui.exportDialog = false;
    ui.go("batch");
  } catch (e) {
    task.processing = null;
    ws.handleError(e, t("ex.startFailed"));
  }
}
</script>

<template>
  <Modal :title="b.onlyChecked ? t('ex.titleChecked') : t('ex.title')" icon="wand" :width="520" @close="ui.exportDialog = false">
    <div v-if="s" class="summary">
      <div class="big">
        <div>
          <b>{{ fileCount }}</b>
          <span>{{ t("ex.files") }}</span>
        </div>
        <div>
          <b>{{ wmCount }}</b>
          <span>{{ t("ex.wm") }}</span>
        </div>
      </div>
      <div v-if="pendingCount" class="pending">
        <div class="pending-head">
          <Icon name="alert" :size="15" />
          <span>{{ t("ex.pendingTitle", { n: pendingCount, f: b.pending.length }) }}</span>
        </div>
        <div class="choices" role="radiogroup">
          <button type="button" role="radio" class="choice" :class="{ on: includePending }" :aria-checked="includePending" @click="includePending = true">
            <span class="radio" />
            <span class="grow">
              <b>{{ t("ex.includePending") }}</b>
              <small>{{ t("ex.includePendingHint") }}</small>
            </span>
          </button>
          <button type="button" role="radio" class="choice" :class="{ on: !includePending }" :aria-checked="!includePending" @click="includePending = false">
            <span class="radio" />
            <span class="grow">
              <b>{{ t("ex.skipPending") }}</b>
              <small>{{ t("ex.skipPendingHint") }}</small>
            </span>
          </button>
        </div>
      </div>
      <div class="note">
        <Icon name="shield" :size="15" />
        <span>{{ t("ex.safe") }}</span>
      </div>
    </div>

    <div v-if="out" class="form">
      <div class="field">
        <span class="label">{{ t("ex.location") }}</span>
        <div class="row">
          <div class="path grow truncate" :title="out.outputDir ?? ''">
            <Icon name="folder" :size="14" />
            {{ out.outputDir ?? t("ex.beside") }}
          </div>
          <button class="btn sm" @click="chooseDir">{{ t("common.change") }}</button>
          <button v-if="out.outputDir" class="btn ghost sm" @click="st.patch((x) => (x.output.outputDir = null))">{{ t("common.reset") }}</button>
        </div>
      </div>
      <div class="grid">
        <label class="field">
          <span class="label">{{ t("ex.suffix") }}</span>
          <input class="input" :value="out.suffix" spellcheck="false" @change="st.patch((x) => (x.output.suffix = ($event.target as HTMLInputElement).value))" />
        </label>
        <label class="field">
          <span class="label">{{ t("ex.format") }}</span>
          <select class="select" :value="out.format" @change="st.patch((x) => (x.output.format = ($event.target as HTMLSelectElement).value as OutputFormat))">
            <option v-for="f in FORMATS" :key="f" :value="f">{{ tx(`fmt.${f}`) }}</option>
          </select>
        </label>
      </div>
      <div class="toggles">
        <label class="row"><Toggle :model-value="out.preserveStructure" @update:model-value="(v) => st.patch((x) => (x.output.preserveStructure = v))" />{{ t("ex.structure") }}</label>
        <label class="row"><Toggle :model-value="out.keepMetadata" @update:model-value="(v) => st.patch((x) => (x.output.keepMetadata = v))" />{{ t("ex.metadata") }}</label>
      </div>
    </div>

    <template #footer>
      <button class="btn" @click="ui.exportDialog = false">{{ t("common.cancel") }}</button>
      <button class="btn brand" :disabled="!fileCount" @click="start"><Icon name="play" :size="14" />{{ t("ex.start") }}</button>
    </template>
  </Modal>
</template>

<style scoped>
.pending {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border-radius: var(--r-12);
  background: var(--warning-soft, color-mix(in srgb, var(--warning) 10%, transparent));
  border: 1px solid color-mix(in srgb, var(--warning) 30%, transparent);
}
.pending-head {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--warning);
  font-size: var(--fs-13);
  font-weight: 600;
}
.choices {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.choice {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  padding: 10px 12px;
  border-radius: var(--r-8);
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text);
  text-align: left;
  font: inherit;
  cursor: pointer;
}
.choice.on {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent);
}
.choice b {
  display: block;
  font-size: var(--fs-13);
}
.choice small {
  display: block;
  margin-top: 2px;
  color: var(--text-2);
  font-size: var(--fs-12);
}
.radio {
  flex: none;
  width: 16px;
  height: 16px;
  margin-top: 1px;
  border-radius: 50%;
  border: 1.5px solid var(--border-strong, var(--text-3));
}
.choice.on .radio {
  border: 5px solid var(--accent);
}
.summary {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.big {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
}
.big > div {
  padding: 14px;
  border-radius: var(--r-12);
  background: var(--surface-2);
  border: 1px solid var(--border);
  display: flex;
  flex-direction: column;
}
.big b {
  font-size: var(--fs-28);
  color: var(--text);
  font-weight: 700;
  line-height: 1.1;
  font-variant-numeric: tabular-nums;
}
.note {
  display: flex;
  gap: 8px;
  padding: 10px 12px;
  border-radius: var(--r-10);
  background: var(--surface-2);
  font-size: var(--fs-12);
}
.note svg {
  flex: none;
  margin-top: 1px;
  color: var(--success);
}
.note.warn {
  background: var(--warning-soft);
}
.note.warn svg {
  color: var(--warning);
}
.form {
  margin-top: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.label {
  font-size: var(--fs-12);
  font-weight: 600;
  color: var(--text);
}
.path {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 10px;
  border-radius: var(--r-8);
  background: var(--surface-2);
  border: 1px solid var(--border);
  font-size: var(--fs-12);
}
.grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}
.toggles {
  display: flex;
  gap: 20px;
  color: var(--text);
}
</style>
