<script setup lang="ts">
import { t } from "../i18n";
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import Icon from "../components/Icon.vue";
import StatusPill from "../components/StatusPill.vue";
import { basename, dirname, formatDuration } from "../composables/format";
import { api, assetUrl } from "../services/ipc";
import { useSettings } from "../stores/settings";
import { useTask } from "../stores/task";
import { useUi } from "../stores/ui";
import { useWorkspace } from "../stores/workspace";

const task = useTask();
const ui = useUi();
const ws = useWorkspace();
const st = useSettings();
const now = ref(Date.now());
let timer: number | undefined;
onMounted(() => (timer = window.setInterval(() => (now.value = Date.now()), 500)));
onBeforeUnmount(() => clearInterval(timer));

const p = computed(() => task.processing);
const done = computed(() => task.lastSummary);
const ratio = computed(() => (p.value && p.value.total ? p.value.done / p.value.total : done.value ? 1 : 0));
const elapsed = computed(() => (p.value ? now.value - p.value.startedAt : done.value?.elapsedMs ?? 0));
const eta = computed(() => {
  if (!p.value || !p.value.done) return null;
  const per = elapsed.value / p.value.done;
  return per * (p.value.total - p.value.done);
});

const outputDir = computed(() => {
  if (done.value?.outputDir) return done.value.outputDir;
  const first = task.recent.find((f) => f.output);
  return first?.output ? dirname(first.output) : st.settings?.output.outputDir ?? null;
});

const R = 54;
const C = 2 * Math.PI * R;
</script>

<template>
  <div class="batch">
    <div class="hero card">
      <div class="ring">
        <svg width="132" height="132" viewBox="0 0 132 132">
          <defs>
            <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
              <stop offset="0" stop-color="#5b4cf6" />
              <stop offset="1" stop-color="#2e90fa" />
            </linearGradient>
          </defs>
          <circle cx="66" cy="66" :r="R" fill="none" stroke="var(--surface-3)" stroke-width="10" />
          <circle
            cx="66"
            cy="66"
            :r="R"
            fill="none"
            stroke="url(#g)"
            stroke-width="10"
            stroke-linecap="round"
            :stroke-dasharray="C"
            :stroke-dashoffset="C * (1 - ratio)"
            transform="rotate(-90 66 66)"
            style="transition: stroke-dashoffset 0.4s ease"
          />
        </svg>
        <div class="pct">
          <b>{{ Math.round(ratio * 100) }}%</b>
          <span v-if="p">{{ p.done }} / {{ p.total }}</span>
          <span v-else-if="done">{{ t("batch.files", { n: done.total }) }}</span>
        </div>
      </div>
      <div class="grow info">
        <h2 v-if="p">{{ task.paused ? t("batch.paused") : t("batch.running") }}</h2>
        <h2 v-else-if="done">{{ done.cancelled ? t("batch.cancelled") : t("batch.done") }}</h2>
        <h2 v-else>{{ t("batch.idle") }}</h2>
        <p class="muted">
          <template v-if="p">
            {{ t("batch.elapsed", { t: formatDuration(elapsed) }) }}<template v-if="eta !== null && !task.paused">{{ t("batch.eta", { t: formatDuration(Math.round(eta)) }) }}</template>
            <template v-if="task.paused">{{ t("batch.pausedNote") }}</template>
          </template>
          <template v-else-if="done">{{ t("batch.doneNote", { t: formatDuration(done.elapsedMs) }) }}</template>
        </p>
        <div class="tiles">
          <div class="tile ok">
            <b>{{ done ? done.completed : task.recent.filter((f) => f.status === "completed").length }}</b>
            <span>{{ t("batch.completed") }}</span>
          </div>
          <div class="tile warn">
            <b>{{ done ? done.needsReview : task.recent.filter((f) => f.status === "needs_review").length }}</b>
            <span>{{ t("batch.needsReview") }}</span>
          </div>
          <div class="tile bad">
            <b>{{ done ? done.failed : p?.failed ?? 0 }}</b>
            <span>{{ t("batch.failed") }}</span>
          </div>
          <div class="tile">
            <b>{{ p ? p.total - p.done : done?.skipped ?? 0 }}</b>
            <span>{{ p ? t("batch.remaining") : t("batch.notProcessed") }}</span>
          </div>
        </div>
        <div class="row actions">
          <template v-if="p">
            <button v-if="!task.paused" class="btn" @click="task.pause"><Icon name="pause" :size="14" />{{ t("batch.pause") }}</button>
            <button v-else class="btn primary" @click="task.resume"><Icon name="play" :size="14" />{{ t("batch.resume") }}</button>
            <button class="btn danger" @click="task.cancel"><Icon name="stop" :size="13" />{{ t("batch.cancel") }}</button>
          </template>
          <template v-else>
            <button v-if="outputDir" class="btn primary" @click="api.revealPath(outputDir!)"><Icon name="folder-open" :size="14" />{{ t("batch.openOutput") }}</button>
            <button v-if="done?.needsReview" class="btn" @click="ws.filter = 'needs_review'; ui.go('workspace')"><Icon name="alert" :size="14" />{{ t("batch.viewReview") }}</button>
            <button class="btn ghost" @click="ui.go('workspace')">{{ t("batch.back") }}</button>
          </template>
        </div>
      </div>
    </div>

    <div class="list card">
      <div class="list-head row">
        <span class="section-title">{{ t("batch.recent") }}</span>
      </div>
      <div class="rows">
        <div v-for="f in task.recent" :key="f.id" class="r" @click="ws.select(f.id); ui.go('workspace')">
          <img v-if="f.thumb" :src="assetUrl(f.thumb)" alt="" />
          <span v-else class="ph"><Icon :name="f.kind === 'pdf' ? 'file-text' : 'image'" :size="14" /></span>
          <span class="grow truncate">{{ f.name }}</span>
          <span class="route subtle truncate">{{ f.route?.reason ?? f.error?.message ?? "" }}</span>
          <span v-if="f.output" class="out mono truncate" :title="f.output">{{ basename(f.output) }}</span>
          <StatusPill :status="f.status" />
        </div>
        <div v-if="!task.recent.length" class="empty subtle">{{ t("batch.empty") }}</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.batch {
  flex: 1;
  overflow: auto;
  padding: 28px;
  display: flex;
  flex-direction: column;
  gap: 18px;
  max-width: 1100px;
  width: 100%;
  margin: 0 auto;
}
.hero {
  display: flex;
  gap: 28px;
  padding: 26px 28px;
  align-items: center;
}
.ring {
  position: relative;
  width: 132px;
  height: 132px;
  flex: none;
}
.pct {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
}
.pct b {
  font-size: 26px;
  font-weight: 700;
  font-variant-numeric: tabular-nums;
}
.pct span {
  font-size: var(--fs-12);
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
h2 {
  margin: 0;
  font-size: var(--fs-20);
}
.info p {
  margin: 4px 0 14px;
}
.tiles {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 10px;
}
.tile {
  padding: 10px 12px;
  border-radius: var(--r-10);
  background: var(--surface-2);
  border: 1px solid var(--border);
  display: flex;
  flex-direction: column;
}
.tile b {
  font-size: var(--fs-20);
  font-variant-numeric: tabular-nums;
}
.tile span {
  font-size: var(--fs-12);
  color: var(--text-2);
}
.tile.ok b {
  color: var(--success);
}
.tile.warn b {
  color: var(--warning);
}
.tile.bad b {
  color: var(--danger);
}
.actions {
  margin-top: 16px;
}
.list {
  flex: 1;
  min-height: 240px;
  display: flex;
  flex-direction: column;
}
.list-head {
  padding: 12px 16px;
  border-bottom: 1px solid var(--border);
}
.rows {
  overflow: auto;
}
.r {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 16px;
  border-bottom: 1px solid var(--border);
  cursor: pointer;
}
.r:hover {
  background: var(--surface-2);
}
.r img,
.ph {
  width: 32px;
  height: 32px;
  border-radius: 6px;
  object-fit: cover;
  flex: none;
  display: grid;
  place-items: center;
  background: var(--surface-3);
  color: var(--text-3);
}
.route {
  max-width: 280px;
  font-size: var(--fs-12);
}
.out {
  max-width: 200px;
  font-size: var(--fs-12);
  color: var(--text-2);
}
.empty {
  padding: 32px;
  text-align: center;
}
</style>
