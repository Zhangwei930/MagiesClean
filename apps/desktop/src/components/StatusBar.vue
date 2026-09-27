<script setup lang="ts">
import { t } from "../i18n";
import { computed } from "vue";
import { AUTO_MODES, QUALITY_MODES, autoModeHint, autoModeLabel, qualityModeHint, qualityModeLabel } from "../composables/format";
import { useSettings } from "../stores/settings";
import { useTask } from "../stores/task";
import { useUi } from "../stores/ui";
import { useWorkspace } from "../stores/workspace";
import type { AutoMode, QualityMode } from "../types";
import Icon from "./Icon.vue";
import Segmented from "./Segmented.vue";

const ws = useWorkspace();
const task = useTask();
const ui = useUi();
const st = useSettings();

const s = computed(() => ws.summary);
// 批量处理对象：有勾选时为勾选的文件，否则为全部文件
const batchCount = computed(() => ws.batch.ready.length + ws.batch.pending.length);
const pendingCount = computed(() => ws.batch.pending.reduce((n, f) => n + f.summary.needsReview, 0));
const autoModes = computed(() => AUTO_MODES.map((k) => ({ value: k, label: autoModeLabel(k), hint: autoModeHint(k) })));
const qualityModes = computed(() => QUALITY_MODES.map((k) => ({ value: k, label: qualityModeLabel(k), hint: qualityModeHint(k) })));
</script>

<template>
  <footer class="statusbar">
    <div class="stats">
      <span>{{ t("sb.files", { n: s?.total ?? 0 }) }}</span>
      <span class="dot" />
      <span>{{ t("sb.withWm", { n: s?.withWatermark ?? 0 }) }}</span>
      <template v-if="s?.needsReview">
        <span class="dot" />
        <button class="link warn" @click="ws.filter = 'needs_review'">{{ t("sb.review", { n: s.needsReview }) }}</button>
      </template>
      <template v-if="s?.completed">
        <span class="dot" />
        <span class="ok">{{ t("sb.done", { n: s.completed }) }}</span>
      </template>
    </div>
    <span class="grow" />
    <template v-if="st.settings">
      <label class="mode">
        <span class="subtle">{{ t("sb.autoMode") }}</span>
        <Segmented
          size="sm"
          :model-value="st.settings.autoMode"
          :options="autoModes"
          @update:model-value="(v: AutoMode) => st.patch((x) => (x.autoMode = v))"
        />
      </label>
      <label class="mode">
        <span class="subtle">{{ t("sb.quality") }}</span>
        <Segmented
          size="sm"
          :model-value="st.settings.qualityMode"
          :options="qualityModes"
          @update:model-value="(v: QualityMode) => st.patch((x) => (x.qualityMode = v))"
        />
      </label>
    </template>
    <button
      class="btn brand lg go"
      :disabled="task.busy || batchCount === 0"
      :title="pendingCount ? t('sb.pendingTip', { n: pendingCount }) : ''"
      @click="ui.exportDialog = true"
    >
      <Icon name="wand" :size="16" />
      {{ ws.batch.onlyChecked ? t("sb.removeChecked") : t("sb.removeAll") }}
      <span v-if="batchCount" class="count">{{ batchCount }}</span>
    </button>
  </footer>
</template>

<style scoped>
.statusbar {
  height: var(--statusbar-h);
  display: flex;
  align-items: center;
  gap: 18px;
  padding: 0 16px;
  border-top: 1px solid var(--border);
  background: var(--bg-elev);
}
.stats {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}
.stats b {
  color: var(--text);
  font-weight: 650;
}
.dot {
  width: 3px;
  height: 3px;
  border-radius: 50%;
  background: var(--text-3);
}
.link {
  border: none;
  background: none;
  padding: 0;
  cursor: pointer;
}
.warn,
.warn b {
  color: var(--warning) !important;
}
.ok,
.ok b {
  color: var(--success) !important;
}
.mode {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: var(--fs-12);
}
.go {
  min-width: 150px;
}
.count {
  min-width: 22px;
  height: 20px;
  padding: 0 6px;
  border-radius: 10px;
  background: rgba(255, 255, 255, 0.22);
  font-size: var(--fs-12);
  display: grid;
  place-items: center;
}
</style>
