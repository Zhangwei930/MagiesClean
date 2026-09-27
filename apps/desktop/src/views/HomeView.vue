<script setup lang="ts">
import { t } from "../i18n";
import { computed, onMounted, ref } from "vue";
import DropZone from "../components/DropZone.vue";
import Icon from "../components/Icon.vue";
import { formatTime } from "../composables/format";
import { api } from "../services/ipc";
import { useTask } from "../stores/task";
import { useUi } from "../stores/ui";
import { useWorkspace } from "../stores/workspace";
import type { JobRecord } from "../types";

const ws = useWorkspace();
const ui = useUi();
const task = useTask();
const recoverable = ref<JobRecord[]>([]);

onMounted(async () => {
  recoverable.value = await api.recoverableJobs().catch(() => []);
});

async function resume(j: JobRecord) {
  try {
    const r = await api.resumeJob(j.id);
    r.added.forEach(ws.upsert);
    recoverable.value = recoverable.value.filter((x) => x.id !== j.id);
    if (r.changed.length) ui.toast({ tone: "warning", title: t("home.changed", { n: r.changed.length }) });
    if (r.added.length) {
      ui.go("workspace");
      task.beginScan(r.added.length);
      await api.scanFiles(r.added.map((f) => f.id));
      await task.refresh();
    } else {
      ui.toast({ tone: "info", title: t("home.allDone") });
    }
  } catch (e) {
    ws.handleError(e, t("home.resumeFailed"));
  }
}

async function discard(j: JobRecord) {
  await api.discardJob(j.id);
  recoverable.value = recoverable.value.filter((x) => x.id !== j.id);
}

const FEATURES = computed(() => [
  { icon: "sparkles", title: t("home.f1t"), body: t("home.f1b") },
  { icon: "layers", title: t("home.f2t"), body: t("home.f2b") },
  { icon: "file-text", title: t("home.f3t"), body: t("home.f3b") },
  { icon: "shield", title: t("home.f4t"), body: t("home.f4b") },
]);
</script>

<template>
  <div class="home">
    <div v-for="j in recoverable" :key="j.id" class="recover card">
      <Icon name="history" :size="18" />
      <div class="grow">
        <b>{{ t("home.recoverTitle") }}</b>
        <div class="muted">{{ t("home.recoverBody", { time: formatTime(j.createdAt), total: j.total, completed: j.completed }) }}</div>
      </div>
      <button class="btn sm" @click="discard(j)">{{ t("home.dismiss") }}</button>
      <button class="btn primary sm" @click="resume(j)">{{ t("home.resume") }}</button>
    </div>
    <DropZone />
    <div class="features">
      <div v-for="f in FEATURES" :key="f.title" class="feat">
        <span class="ic"><Icon :name="f.icon" :size="16" /></span>
        <div>
          <b>{{ f.title }}</b>
          <p>{{ f.body }}</p>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.home {
  flex: 1;
  overflow: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 28px;
  padding: 32px;
  background: radial-gradient(80% 60% at 50% -10%, color-mix(in srgb, var(--brand-1) 10%, transparent), transparent 70%), var(--bg);
}
.recover {
  width: min(680px, 100%);
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 14px;
  border-color: color-mix(in srgb, var(--accent) 35%, transparent);
}
.recover > svg {
  color: var(--accent-text);
}
.recover .muted {
  font-size: var(--fs-12);
  margin-top: 2px;
}
.features {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 14px;
  width: min(880px, 100%);
}
.feat {
  display: flex;
  gap: 10px;
}
.ic {
  width: 30px;
  height: 30px;
  border-radius: 9px;
  flex: none;
  display: grid;
  place-items: center;
  background: var(--accent-soft);
  color: var(--accent-text);
}
.feat b {
  font-size: var(--fs-13);
}
.feat p {
  margin: 2px 0 0;
  font-size: var(--fs-12);
  color: var(--text-2);
}
</style>
