<script setup lang="ts">
import { t } from "../i18n";
import { computed, onMounted, ref } from "vue";
import Icon from "../components/Icon.vue";
import Modal from "../components/Modal.vue";
import { basename, formatTime, routeLabel } from "../composables/format";
import { api } from "../services/ipc";
import { useUi } from "../stores/ui";
import type { HistoryRecord, JobRecord } from "../types";

const ui = useUi();
const records = ref<HistoryRecord[]>([]);
const jobs = ref<JobRecord[]>([]);
const confirmClear = ref(false);
const q = ref("");

async function load() {
  [records.value, jobs.value] = await Promise.all([api.getHistory(500), api.recentJobs()]);
}
onMounted(load);

const filtered = computed(() => {
  const s = q.value.trim().toLowerCase();
  return s ? records.value.filter((r) => r.input.toLowerCase().includes(s)) : records.value;
});

const STATUS = computed<Record<string, { label: string; cls: string }>>(() => ({
  completed: { label: t("history.stCompleted"), cls: "ok" },
  needs_review: { label: t("history.stReview"), cls: "warn" },
  failed: { label: t("history.stFailed"), cls: "bad" },
  other: { label: "—", cls: "" },
}));
const JOB_STATE = computed<Record<string, string>>(() => ({
  completed: t("history.jobCompleted"),
  cancelled: t("history.jobCancelled"),
  processing: t("history.jobProcessing"),
  paused: t("history.jobPaused"),
}));

async function clear() {
  await api.clearHistory();
  confirmClear.value = false;
  await load();
  ui.toast({ tone: "success", title: t("history.cleared") });
}

async function reveal(p: string) {
  try {
    await api.revealPath(p);
  } catch {
    ui.toast({ tone: "warning", title: t("history.missing") });
  }
}
</script>

<template>
  <div class="history">
    <div class="head row">
      <div class="grow">
        <h2>{{ t("history.title") }}</h2>
        <p class="muted">{{ t("history.sub") }}</p>
      </div>
      <label class="search"><Icon name="search" :size="14" /><input v-model="q" :placeholder="t('history.search')" /></label>
      <button class="btn danger" :disabled="!records.length" @click="confirmClear = true"><Icon name="trash" :size="14" />{{ t("history.clear") }}</button>
    </div>

    <div class="jobs">
      <div v-for="j in jobs.slice(0, 6)" :key="j.id" class="job card">
        <div class="row">
          <Icon name="layers" :size="15" />
          <b class="grow">{{ formatTime(j.createdAt) }}</b>
          <span class="state" :class="j.state">{{ JOB_STATE[j.state] ?? j.state }}</span>
        </div>
        <div class="nums">
          <span><b>{{ j.completed }}</b> {{ t("history.done") }}</span>
          <span><b>{{ j.needsReview }}</b> {{ t("history.review") }}</span>
          <span><b>{{ j.failed }}</b> {{ t("history.failed") }}</span>
          <span class="subtle">{{ t("history.total", { n: j.total }) }}</span>
        </div>
      </div>
    </div>

    <div class="table card">
      <div class="tr th">
        <span>{{ t("history.colFile") }}</span>
        <span>{{ t("history.colMethod") }}</span>
        <span>{{ t("history.colQuality") }}</span>
        <span>{{ t("history.colStatus") }}</span>
        <span>{{ t("history.colTime") }}</span>
        <span />
      </div>
      <div class="tbody">
        <div v-for="r in filtered" :key="r.id" class="tr">
          <span class="file">
            <Icon :name="r.kind === 'pdf' ? 'file-text' : 'image'" :size="14" />
            <span class="truncate" :title="r.input">{{ basename(r.input) }}</span>
          </span>
          <span class="truncate muted">{{ r.route ? routeLabel(r.route) : "—" }}</span>
          <span class="mono">{{ r.quality !== null ? Math.round(r.quality * 100) : "—" }}</span>
          <span class="st" :class="STATUS[r.status]?.cls">{{ STATUS[r.status]?.label ?? r.status }}</span>
          <span class="muted mono">{{ formatTime(r.createdAt) }}</span>
          <span>
            <button v-if="r.output" class="btn ghost sm" @click="reveal(r.output)"><Icon name="folder-open" :size="13" />{{ t("common.show") }}</button>
          </span>
        </div>
        <div v-if="!filtered.length" class="empty subtle">{{ t("history.empty") }}</div>
      </div>
    </div>

    <Modal v-if="confirmClear" :title="t('history.confirmTitle')" icon="trash" tone="danger" @close="confirmClear = false">
      {{ t("history.confirmBody") }}
      <template #footer>
        <button class="btn" @click="confirmClear = false">{{ t("common.cancel") }}</button>
        <button class="btn danger solid" @click="clear">{{ t("history.clear") }}</button>
      </template>
    </Modal>
  </div>
</template>

<style scoped>
.history {
  flex: 1;
  overflow: auto;
  padding: 28px;
  max-width: 1100px;
  width: 100%;
  margin: 0 auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
h2 {
  margin: 0;
  font-size: var(--fs-20);
}
.head p {
  margin: 4px 0 0;
}
.search {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 32px;
  padding: 0 10px;
  border-radius: var(--r-8);
  background: var(--surface);
  border: 1px solid var(--border);
  color: var(--text-3);
}
.search input {
  border: none;
  outline: none;
  background: transparent;
  color: var(--text);
  font: inherit;
  width: 180px;
}
.jobs {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
}
.job {
  padding: 12px 14px;
}
.state {
  font-size: var(--fs-11);
  font-weight: 600;
  color: var(--text-2);
}
.state.completed {
  color: var(--success);
}
.state.processing,
.state.paused {
  color: var(--warning);
}
.nums {
  display: flex;
  gap: 12px;
  margin-top: 8px;
  font-size: var(--fs-12);
  color: var(--text-2);
}
.nums b {
  color: var(--text);
}
.table {
  overflow: hidden;
}
.tr {
  display: grid;
  grid-template-columns: 2.2fr 1.4fr 0.6fr 0.8fr 1.2fr 0.8fr;
  gap: 12px;
  align-items: center;
  padding: 8px 16px;
  border-bottom: 1px solid var(--border);
  min-height: 42px;
}
.th {
  font-size: var(--fs-11);
  font-weight: 600;
  color: var(--text-3);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  background: var(--surface-2);
}
.file {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.st {
  font-size: var(--fs-12);
  font-weight: 600;
}
.st.ok {
  color: var(--success);
}
.st.warn {
  color: var(--warning);
}
.st.bad {
  color: var(--danger);
}
.empty {
  padding: 32px;
  text-align: center;
}
</style>
