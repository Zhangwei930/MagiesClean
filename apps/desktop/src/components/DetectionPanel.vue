<script setup lang="ts">
import { computed, ref } from "vue";
import { TYPE_ICON, evidenceLabel, formatSize, pct, routeLabel } from "../composables/format";
import { t } from "../i18n";
import { api } from "../services/ipc";
import { useSettings } from "../stores/settings";
import { useTask } from "../stores/task";
import { useWorkspace } from "../stores/workspace";
import type { CandidateView, FileView } from "../types";
import ConfidenceBadge from "./ConfidenceBadge.vue";
import Icon from "./Icon.vue";
import StatusPill from "./StatusPill.vue";

const props = defineProps<{ file: FileView; activeId: string | null }>();
const emit = defineEmits<{ select: [id: string]; editMask: []; generate: []; password: [pw: string] }>();

const ws = useWorkspace();
const task = useTask();
const settings = useSettings();
const password = ref("");
const expanded = ref<string | null>(null);

const info = computed(() => {
  const i = props.file.info;
  if (!i) return formatSize(props.file.size);
  if (i.type === "image") return `${i.format} · ${i.width} × ${i.height} · ${formatSize(props.file.size)}${i.hasAlpha ? t("panel.transparency") : ""}`;
  return `${i.pdfKind} · ${t("panel.pages", { n: i.pageCount })} · ${formatSize(props.file.size)}${i.encrypted ? t("panel.encrypted") : ""}`;
});

const busy = computed(() => ws.previewBusy === props.file.id || props.file.state === "processing");
const editable = computed(() => props.file.state !== "processing" && !task.processing);
const isSigned = computed(() => props.file.info?.type === "pdf" && props.file.info.signed);

function decisionLabel(c: CandidateView) {
  if (c.userAction === "remove") return { text: t("decision.confirmed"), tone: "success" };
  if (c.userAction === "ignore") return { text: t("decision.ignored"), tone: "neutral" };
  if (c.decision === "auto") return { text: t("decision.auto"), tone: "success" };
  if (c.decision === "review") return { text: t("decision.review"), tone: "warning" };
  return { text: t("decision.low"), tone: "neutral" };
}

async function similar(c: CandidateView) {
  try {
    await api.applyToSimilar(props.file.id, c.id);
    task.beginScan(ws.order.length - 1);
    await task.refresh();
  } catch (e) {
    ws.handleError(e, t("panel.similarFailed"));
  }
}

async function confirmSignature() {
  try {
    ws.upsert(await api.confirmSignature(props.file.id));
  } catch (e) {
    ws.handleError(e, t("panel.actionFailed"));
  }
}

function submitPassword() {
  if (!password.value) return;
  emit("password", password.value);
  password.value = "";
}
</script>

<template>
  <aside class="panel">
    <div class="head">
      <div class="row">
        <div class="grow">
          <div class="name truncate" :title="file.path">{{ file.name }}</div>
          <div class="meta">{{ info }}</div>
        </div>
        <StatusPill :status="file.status" />
      </div>
    </div>

    <div class="scroll">
      <!-- 需要用户动作的提示 -->
      <div v-if="file.needsPassword" class="alert warning">
        <Icon name="lock" :size="16" />
        <div class="grow">
          <b>{{ t("panel.needPwTitle") }}</b>
          <p>{{ t("panel.needPwBody") }}</p>
          <form class="row" @submit.prevent="submitPassword">
            <input v-model="password" class="input grow" type="password" :placeholder="t('panel.pwPlaceholder')" autocomplete="off" />
            <button class="btn primary sm" type="submit">{{ t("panel.unlock") }}</button>
          </form>
        </div>
      </div>
      <div v-if="isSigned && !file.signatureConfirmed" class="alert warning">
        <Icon name="signature" :size="16" />
        <div class="grow">
          <b>{{ t("panel.signedTitle") }}</b>
          <p>{{ t("panel.signedBody") }}</p>
          <button class="btn sm" @click="confirmSignature">{{ t("panel.signedConfirm") }}</button>
        </div>
      </div>
      <div v-if="file.error && !file.needsPassword" class="alert danger">
        <Icon name="x-circle" :size="16" />
        <div class="grow">
          <b>{{ file.error.message }}</b>
          <p>{{ file.error.nextStep }}</p>
          <button v-if="file.error.retryable" class="btn sm" :disabled="task.busy" @click="ws.rescan([file.id])"><Icon name="refresh" :size="13" />{{ t("common.retry") }}</button>
        </div>
      </div>

      <!-- 检测结果 -->
      <section>
        <div class="row sec-head">
          <span class="section-title">{{ t("panel.detections") }}</span>
          <span class="grow" />
          <span v-if="file.summary.total" class="sum">
            <span class="ok">{{ t("panel.sumRemove", { n: file.summary.toRemove }) }}</span>
            <span v-if="file.summary.needsReview" class="warn">{{ t("panel.sumReview", { n: file.summary.needsReview }) }}</span>
            <span v-if="file.summary.ignored" class="subtle">{{ t("panel.sumIgnore", { n: file.summary.ignored }) }}</span>
          </span>
        </div>

        <div v-if="file.summary.needsReview" class="bulk">
          <span class="grow">{{ t("panel.pending", { n: file.summary.needsReview }) }}</span>
          <button class="btn sm" :disabled="!editable" @click="ws.resolvePending(file.id, 'ignore')">{{ t("panel.ignoreAll") }}</button>
          <button class="btn primary sm" :disabled="!editable" @click="ws.resolvePending(file.id, 'remove')">{{ t("panel.removeAll") }}</button>
        </div>

        <div v-if="file.status === 'waiting' || file.status === 'scanning'" class="empty">
          <Icon name="loader" :size="18" class="spin" />
          <span>{{ t("panel.waitingScan") }}</span>
        </div>
        <div v-else-if="!file.candidates.length && !file.needsPassword" class="empty">
          <Icon name="check-circle" :size="22" />
          <span>{{ t("panel.noWatermark") }}</span>
          <small class="subtle">{{ t("panel.noWatermarkHint") }}</small>
        </div>

        <div
          v-for="c in file.candidates"
          :key="c.id"
          class="cand"
          :class="{ active: c.id === activeId, dim: !c.willRemove && !c.awaitingReview }"
          @click="emit('select', c.id)"
        >
          <div class="row">
            <span class="type-ic"><Icon :name="TYPE_ICON[c.watermarkType]" :size="15" /></span>
            <div class="grow">
              <div class="row">
                <b>{{ c.typeLabel }}</b>
                <span v-if="c.page !== null" class="subtle">{{ t("panel.page", { n: c.page + 1 }) }}</span>
              </div>
              <div class="sources">
                <span v-for="s in c.sources" :key="s" class="src">{{ s }}</span>
              </div>
            </div>
            <ConfidenceBadge :value="c.confidence" :decision="c.decision" />
          </div>
          <div v-if="c.text" class="text truncate" :title="c.text">“{{ c.text }}”</div>
          <div class="row foot">
            <span class="decision" :class="decisionLabel(c).tone">{{ decisionLabel(c).text }}</span>
            <span class="grow" />
            <button
              class="btn sm"
              :class="{ primary: c.userAction === 'remove' }"
              :disabled="!editable"
              :title="t('panel.removeTip')"
              @click.stop="ws.setAction(file.id, c.id, c.userAction === 'remove' ? 'pending' : 'remove')"
            >
              <Icon name="check" :size="13" />{{ t("panel.remove") }}
            </button>
            <button
              class="btn sm"
              :class="{ pressed: c.userAction === 'ignore' }"
              :disabled="!editable"
              :title="t('panel.ignoreTip')"
              @click.stop="ws.setAction(file.id, c.id, c.userAction === 'ignore' ? 'pending' : 'ignore')"
            >
              <Icon name="x" :size="13" />{{ t("panel.ignore") }}
            </button>
          </div>
          <div v-if="c.id === activeId" class="row more">
            <button v-if="file.kind === 'image'" class="btn ghost sm" :disabled="!editable" @click.stop="emit('editMask')"><Icon name="brush" :size="13" />{{ t("panel.editMask") }}</button>
            <button v-if="file.kind === 'image'" class="btn ghost sm" :disabled="task.busy" :title="t('panel.applySimilarTip')" @click.stop="similar(c)">
              <Icon name="copy" :size="13" />{{ t("panel.applySimilar") }}
            </button>
            <button class="btn ghost sm" @click.stop="expanded = expanded === c.id ? null : c.id"><Icon name="info" :size="13" />{{ t("panel.evidence") }}</button>
          </div>
          <div v-if="expanded === c.id" class="evidence">
            <div v-for="e in c.evidence" :key="e.name + e.source" class="ev">
              <span class="grow">{{ evidenceLabel(e.name) }}</span>
              <span class="evbar"><span :style="{ width: pct(e.value) }" /></span>
              <span class="mono">{{ Math.round(e.value * 100) }}</span>
            </div>
            <div v-if="c.opacity !== null" class="ev subtle">{{ t("panel.opacity", { v: pct(c.opacity) }) }}</div>
          </div>
        </div>
      </section>

      <!-- 修复结果 -->
      <section v-if="file.summary.toRemove || file.hasManualMask || file.resultFile">
        <div class="section-title sec-head">{{ t("panel.repair") }}</div>
        <div v-if="file.route" class="route card">
          <div class="row">
            <Icon name="wand" :size="15" />
            <b>{{ routeLabel(file.route.route) }}</b>
          </div>
          <p>{{ file.route.reason }}</p>
        </div>
        <div v-if="file.quality" class="quality card" :class="{ bad: !file.quality.passed }">
          <div class="row">
            <Icon :name="file.quality.passed ? 'check-circle' : 'alert'" :size="15" />
            <b>{{ file.quality.passed ? t("panel.qualityPassed") : t("panel.qualityFailed") }}</b>
            <span class="grow" />
            <span class="mono">{{ Math.round(file.quality.score * 100) }}</span>
          </div>
          <ul v-if="file.quality.issues.length">
            <li v-for="i in file.quality.issues" :key="i.kind">{{ i.message }}</li>
          </ul>
          <p class="subtle small">{{ t("panel.qualityNote") }}</p>
        </div>
        <div class="row actions">
          <button class="btn" :disabled="busy || !editable" @click="emit('generate')">
            <Icon :name="busy ? 'loader' : 'eye'" :size="14" :class="{ spin: busy }" />{{ file.resultCurrent ? t("panel.viewResult") : t("panel.generatePreview") }}
          </button>
          <button class="btn primary grow" :disabled="busy || !editable || file.summary.needsReview > 0 || (isSigned && !file.signatureConfirmed)" @click="ws.exportOne(file.id)">
            <Icon name="export" :size="14" />{{ file.quality && !file.quality.passed ? t("panel.acceptExport") : t("panel.exportFile") }}
          </button>
        </div>
        <div v-if="file.output" class="output">
          <Icon name="check-circle" :size="14" />
          <span class="grow truncate mono" :title="file.output">{{ file.output }}</span>
          <button class="btn ghost sm" @click="api.revealPath(file.output!)">{{ t("common.show") }}</button>
          <button class="btn ghost sm" @click="api.openPath(file.output!)">{{ t("common.open") }}</button>
        </div>
      </section>

      <section v-if="file.notes.length">
        <div class="section-title sec-head">{{ t("panel.notes") }}</div>
        <ul class="notes">
          <li v-for="n in file.notes" :key="n">{{ n }}</li>
        </ul>
      </section>

      <section v-if="settings.settings?.advancedMode" class="adv">
        <div class="section-title sec-head">{{ t("panel.diagnostics") }}</div>
        <div class="kv"><span>{{ t("panel.fileId") }}</span><span class="mono truncate">{{ file.id }}</span></div>
        <div class="kv"><span>{{ t("panel.stateStage") }}</span><span class="mono">{{ file.state }} / {{ file.stage }}</span></div>
        <div class="kv"><span>{{ t("panel.maskVersion") }}</span><span class="mono">{{ file.maskVersion }}</span></div>
        <div class="kv"><span>{{ t("panel.batchMatched") }}</span><span>{{ file.batchMatched ? t("common.yes") : t("common.no") }}</span></div>
      </section>
    </div>
  </aside>
</template>

<style scoped>
.panel {
  width: var(--panel-w);
  flex: none;
  display: flex;
  flex-direction: column;
  border-left: 1px solid var(--border);
  background: var(--bg-elev);
  min-height: 0;
}
.head {
  padding: 14px 16px 12px;
  border-bottom: 1px solid var(--border);
}
.name {
  font-weight: 650;
  font-size: var(--fs-14);
}
.meta {
  font-size: var(--fs-12);
  color: var(--text-3);
  margin-top: 2px;
}
.scroll {
  flex: 1;
  overflow-y: auto;
  padding: 4px 14px 24px;
}
section {
  margin-top: 14px;
}
.sec-head {
  margin-bottom: 8px;
}
.sum {
  display: flex;
  gap: 8px;
  font-size: var(--fs-11);
  font-weight: 600;
}
.ok {
  color: var(--success);
}
.warn {
  color: var(--warning);
}
.bulk {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 10px;
  margin-bottom: 8px;
  border-radius: var(--r-10);
  background: var(--warning-soft);
  color: var(--warning);
  font-size: var(--fs-12);
  font-weight: 500;
}
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 22px 12px;
  border-radius: var(--r-12);
  border: 1px dashed var(--border);
  color: var(--text-2);
}
.cand {
  padding: 10px 10px 8px;
  margin-bottom: 8px;
  border-radius: var(--r-12);
  border: 1px solid var(--border);
  background: var(--surface);
  cursor: pointer;
  transition: border-color var(--dur-fast), box-shadow var(--dur-fast), opacity var(--dur-fast);
}
.cand:hover {
  border-color: var(--border-strong);
}
.cand.active {
  border-color: var(--accent);
  box-shadow: 0 0 0 3px var(--accent-soft);
}
.cand.dim {
  opacity: 0.72;
}
.type-ic {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  display: grid;
  place-items: center;
  background: var(--accent-soft);
  color: var(--accent-text);
  flex: none;
}
.sources {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin-top: 3px;
}
.src {
  font-size: 10px;
  padding: 1px 6px;
  border-radius: 4px;
  background: var(--surface-3);
  color: var(--text-2);
}
.text {
  margin-top: 6px;
  font-size: var(--fs-12);
  color: var(--text-2);
}
.foot {
  margin-top: 8px;
  gap: 5px;
}
.decision {
  font-size: var(--fs-11);
  font-weight: 600;
}
.decision.success {
  color: var(--success);
}
.decision.warning {
  color: var(--warning);
}
.decision.neutral {
  color: var(--text-3);
}
.pressed {
  background: var(--surface-3);
}
.more {
  margin-top: 6px;
  padding-top: 6px;
  border-top: 1px solid var(--border);
  gap: 2px;
  flex-wrap: wrap;
}
.evidence {
  margin-top: 6px;
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: var(--fs-12);
}
.ev {
  display: flex;
  align-items: center;
  gap: 8px;
}
.evbar {
  width: 60px;
  height: 4px;
  border-radius: 4px;
  background: var(--surface-3);
  overflow: hidden;
}
.evbar span {
  display: block;
  height: 100%;
  background: var(--accent);
}
.alert {
  display: flex;
  gap: 10px;
  padding: 12px;
  margin-top: 12px;
  border-radius: var(--r-12);
  font-size: var(--fs-12);
}
.alert p {
  margin: 4px 0 8px;
  color: var(--text-2);
}
.alert.warning {
  background: var(--warning-soft);
}
.alert.warning > svg {
  color: var(--warning);
}
.alert.danger {
  background: var(--danger-soft);
}
.alert.danger > svg {
  color: var(--danger);
}
.route,
.quality {
  padding: 10px 12px;
  margin-bottom: 8px;
  font-size: var(--fs-12);
}
.route p,
.quality p {
  margin: 6px 0 0;
  color: var(--text-2);
}
.route svg {
  color: var(--accent-text);
}
.quality svg {
  color: var(--success);
}
.quality.bad {
  border-color: color-mix(in srgb, var(--warning) 45%, transparent);
  background: var(--warning-soft);
}
.quality.bad svg {
  color: var(--warning);
}
.quality ul {
  margin: 6px 0 0;
  padding-left: 18px;
  color: var(--text);
}
.small {
  font-size: var(--fs-11);
}
.actions {
  margin-top: 4px;
}
.output {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 10px;
  padding: 8px 10px;
  border-radius: var(--r-10);
  background: var(--success-soft);
  color: var(--success);
  font-size: var(--fs-12);
}
.output .mono {
  color: var(--text);
}
.notes {
  margin: 0;
  padding-left: 18px;
  color: var(--text-2);
  font-size: var(--fs-12);
}
.kv {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  font-size: var(--fs-12);
  padding: 3px 0;
  color: var(--text-2);
}
</style>
