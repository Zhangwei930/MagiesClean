<script setup lang="ts">
import { open } from "@tauri-apps/plugin-dialog";
import { computed, onMounted, ref } from "vue";
import Icon from "../components/Icon.vue";
import Modal from "../components/Modal.vue";
import ModelStatus from "../components/ModelStatus.vue";
import Segmented from "../components/Segmented.vue";
import Toggle from "../components/Toggle.vue";
import { AUTO_MODES, QUALITY_MODES, autoModeHint, autoModeLabel, formatSize, qualityModeHint, qualityModeLabel } from "../composables/format";
import { t, tx, type MessageKey } from "../i18n";
import { api } from "../services/ipc";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import type { AutoMode, ConflictPolicy, JpegQuality, Language, OutputFormat, QualityMode, Theme } from "../types";

const st = useSettings();
const ui = useUi();
const s = computed(() => st.settings!);
const section = ref("general");
const confirmOverwrite = ref(false);
const newPreset = ref("");

onMounted(() => st.refreshInfo().catch(() => {}));

const SECTIONS: { key: string; label: MessageKey; icon: string }[] = [
  { key: "general", label: "settings.general", icon: "settings" },
  { key: "detect", label: "settings.detect", icon: "target" },
  { key: "quality", label: "settings.quality", icon: "wand" },
  { key: "output", label: "settings.output", icon: "export" },
  { key: "presets", label: "settings.presets", icon: "layers" },
  { key: "performance", label: "settings.performance", icon: "cpu" },
  { key: "models", label: "settings.models", icon: "box" },
  { key: "about", label: "settings.about", icon: "shield" },
];

const autoModes = computed(() => AUTO_MODES.map((k) => ({ value: k, label: autoModeLabel(k) })));
const qualityModes = computed(() => QUALITY_MODES.map((k) => ({ value: k, label: qualityModeLabel(k) })));
const themes = computed<{ value: Theme; label: string; icon: string }[]>(() => [
  { value: "system", label: t("settings.themeSystem"), icon: "monitor" },
  { value: "light", label: t("settings.themeLight"), icon: "sun" },
  { value: "dark", label: t("settings.themeDark"), icon: "moon" },
]);
// 语言名称始终以其自身语言显示
const languages: { value: Language; label: string }[] = [
  { value: "en", label: "English" },
  { value: "zh-CN", label: "简体中文" },
];

const th = computed(() => s.value.thresholds[s.value.autoMode]);
const MASK_KEYS = ["maskThreshold", "maskDilation", "maskFeather", "minComponentSize"] as const;
const MASK_META: Record<(typeof MASK_KEYS)[number], { id: string; hint: MessageKey }> = {
  maskThreshold: { id: "mask_threshold", hint: "settings.maskThreshold" },
  maskDilation: { id: "mask_dilation", hint: "settings.maskDilation" },
  maskFeather: { id: "mask_feather", hint: "settings.maskFeather" },
  minComponentSize: { id: "min_component_size", hint: "settings.minComponent" },
};

function setThreshold(key: "auto" | "review", v: number) {
  st.patch((x) => {
    x.thresholds[x.autoMode][key] = v;
  });
}

function numOrNull(e: Event): number | null {
  const v = (e.target as HTMLInputElement).value;
  return v ? +v : null;
}

async function chooseDir() {
  const r = await open({ directory: true, multiple: false });
  if (typeof r === "string") await st.patch((x) => (x.output.outputDir = r));
}

function toggleOverwrite(v: boolean) {
  if (v) confirmOverwrite.value = true;
  else st.patch((x) => (x.output.overwriteOriginals = false));
}

function enableOverwrite() {
  st.patch((x) => (x.output.overwriteOriginals = true));
  confirmOverwrite.value = false;
}

async function addPreset() {
  const name = newPreset.value.trim();
  if (!name) return;
  await st.savePreset({
    id: crypto.randomUUID(),
    name,
    detectionMode: s.value.autoMode,
    confidenceThreshold: th.value.auto,
    removalQuality: s.value.qualityMode,
    outputFormat: s.value.output.format,
    builtin: false,
  });
  newPreset.value = "";
  ui.toast({ tone: "success", title: t("settings.presetSaved", { name }) });
}

async function applyPreset(id: string, name: string) {
  await st.applyPreset(id);
  ui.toast({ tone: "success", title: t("settings.presetApplied", { name }), body: t("settings.presetAppliedBody") });
}

async function clearCache() {
  const freed = await api.clearCache();
  await st.refreshInfo();
  ui.toast({ tone: "success", title: t("settings.cacheCleared", { size: formatSize(freed) }) });
}
</script>

<template>
  <div v-if="st.settings" class="settings">
    <nav class="side">
      <button v-for="x in SECTIONS" :key="x.key" :class="{ on: section === x.key }" @click="section = x.key">
        <Icon :name="x.icon" :size="15" />{{ t(x.label) }}
      </button>
    </nav>

    <div class="content">
      <!-- 通用 -->
      <template v-if="section === 'general'">
        <h2>{{ t("settings.general") }}</h2>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.language") }}</b>
              <p>English · 简体中文</p>
            </div>
            <Segmented :model-value="s.language" :options="languages" @update:model-value="(v: Language) => st.setLanguage(v)" />
          </div>
          <div class="field">
            <div class="grow"><b>{{ t("settings.appearance") }}</b></div>
            <Segmented :model-value="s.theme" :options="themes" @update:model-value="(v: Theme) => st.patch((x) => (x.theme = v))" />
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.advanced") }}</b>
              <p>{{ t("settings.advancedHint") }}</p>
            </div>
            <Toggle :model-value="s.advancedMode" @update:model-value="(v) => st.patch((x) => (x.advancedMode = v))" />
          </div>
        </div>
        <div class="card group">
          <div class="section-title pad">{{ t("settings.shortcuts") }}</div>
          <div class="keys">
            <span><kbd class="kbd">Space</kbd> {{ t("settings.kPan") }}</span>
            <span><kbd class="kbd">+</kbd> / <kbd class="kbd">-</kbd> {{ t("settings.kZoom") }}</span>
            <span><kbd class="kbd">0</kbd> {{ t("settings.kFit", { k: "1" }) }}</span>
            <span><kbd class="kbd">B</kbd> {{ t("settings.kToggle") }}</span>
            <span><kbd class="kbd">M</kbd> {{ t("settings.kMask") }}</span>
            <span><kbd class="kbd">Delete</kbd> {{ t("settings.kIgnore") }}</span>
            <span><kbd class="kbd">⌘Z</kbd> / <kbd class="kbd">⇧⌘Z</kbd> {{ t("settings.kUndo") }}</span>
            <span><kbd class="kbd">↑</kbd> / <kbd class="kbd">↓</kbd> {{ t("settings.kNav") }}</span>
          </div>
        </div>
      </template>

      <!-- 识别与复核 -->
      <template v-else-if="section === 'detect'">
        <h2>{{ t("settings.detect") }}</h2>
        <p class="lead">{{ t("settings.detectLead") }}</p>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.autoMode") }}</b>
              <p>{{ autoModeHint(s.autoMode) }}</p>
            </div>
            <Segmented :model-value="s.autoMode" :options="autoModes" @update:model-value="(v: AutoMode) => st.patch((x) => (x.autoMode = v))" />
          </div>
          <div v-if="s.autoMode === 'aggressive'" class="warn-box"><Icon name="alert" :size="15" />{{ t("settings.aggressiveWarn") }}</div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.autoThreshold") }}</b>
              <p>{{ t("settings.autoThresholdHint") }}</p>
            </div>
            <input type="range" min="0.5" max="1" step="0.01" :value="th.auto" @change="setThreshold('auto', +($event.target as HTMLInputElement).value)" />
            <span class="val mono">{{ Math.round(th.auto * 100) }}%</span>
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.reviewThreshold") }}</b>
              <p>{{ t("settings.reviewThresholdHint") }}</p>
            </div>
            <input type="range" min="0.3" :max="th.auto" step="0.01" :value="th.review" @change="setThreshold('review', +($event.target as HTMLInputElement).value)" />
            <span class="val mono">{{ Math.round(th.review * 100) }}%</span>
          </div>
          <div class="scale">
            <span class="ig" :style="{ width: `${th.review * 100}%` }">{{ t("settings.scaleIgnore") }}</span>
            <span class="rv" :style="{ width: `${(th.auto - th.review) * 100}%` }">{{ t("settings.scaleReview") }}</span>
            <span class="au" :style="{ width: `${(1 - th.auto) * 100}%` }">{{ t("settings.scaleAuto") }}</span>
          </div>
        </div>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.batchLearning") }}</b>
              <p>{{ t("settings.batchLearningHint") }}</p>
            </div>
            <Toggle :model-value="s.batch.enabled" @update:model-value="(v) => st.patch((x) => (x.batch.enabled = v))" />
          </div>
          <div v-if="s.advancedMode" class="field">
            <div class="grow">
              <b>{{ t("settings.sampleCount") }}</b>
              <p>{{ t("settings.sampleCountHint") }}</p>
            </div>
            <input
              class="input num"
              type="number"
              min="3"
              max="200"
              :value="s.batch.sampleOverride ?? ''"
              :placeholder="t('settings.default')"
              @change="st.patch((x) => (x.batch.sampleOverride = numOrNull($event)))"
            />
          </div>
        </div>
        <div v-if="s.advancedMode" class="card group">
          <div class="section-title pad">{{ t("settings.maskParams") }}</div>
          <div v-for="k in MASK_KEYS" :key="k" class="field">
            <div class="grow">
              <b class="mono">{{ MASK_META[k].id }}</b>
              <p>{{ t(MASK_META[k].hint) }}</p>
            </div>
            <input class="input num" type="number" step="0.1" :value="s.mask[k]" @change="st.patch((x) => (x.mask[k] = +($event.target as HTMLInputElement).value))" />
          </div>
        </div>
      </template>

      <!-- 修复质量 -->
      <template v-else-if="section === 'quality'">
        <h2>{{ t("settings.quality") }}</h2>
        <p class="lead">{{ t("settings.qualityLead") }}</p>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.qualityMode") }}</b>
              <p>{{ qualityModeHint(s.qualityMode) }}</p>
            </div>
            <Segmented :model-value="s.qualityMode" :options="qualityModes" @update:model-value="(v: QualityMode) => st.patch((x) => (x.qualityMode = v))" />
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.qualityThreshold") }}</b>
              <p>{{ t("settings.qualityThresholdHint") }}</p>
            </div>
            <input
              type="range"
              min="0.3"
              max="0.95"
              step="0.01"
              :value="s.router.qualityReviewThreshold"
              @change="st.patch((x) => (x.router.qualityReviewThreshold = +($event.target as HTMLInputElement).value))"
            />
            <span class="val mono">{{ Math.round(s.router.qualityReviewThreshold * 100) }}</span>
          </div>
        </div>
        <div class="card group routes">
          <div class="section-title pad">{{ t("settings.routesTitle") }}</div>
          <div class="route">
            <Icon name="file-text" :size="15" />
            <div><b>{{ t("route.pdf_object_remove") }}</b><p>{{ t("settings.routePdf") }}</p></div>
          </div>
          <div class="route">
            <Icon name="droplet" :size="15" />
            <div><b>{{ t("route.alpha_restore") }}</b><p>{{ t("settings.routeAlpha") }}</p></div>
          </div>
          <div class="route">
            <Icon name="bolt" :size="15" />
            <div><b>{{ t("route.fast_inpaint") }}</b><p>{{ t("settings.routeFast") }}</p></div>
          </div>
          <div class="route">
            <Icon name="grid" :size="15" />
            <div><b>{{ t("settings.routeTexture") }}</b><p>{{ t("settings.routeTextureHint") }}</p></div>
          </div>
        </div>
      </template>

      <!-- 导出 -->
      <template v-else-if="section === 'output'">
        <h2>{{ t("settings.output") }}</h2>
        <p class="lead">{{ t("settings.outputLead") }}</p>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.location") }}</b>
              <p class="truncate">{{ s.output.outputDir ?? t("settings.beside") }}</p>
            </div>
            <button class="btn sm" @click="chooseDir"><Icon name="folder" :size="13" />{{ t("common.choose") }}</button>
            <button v-if="s.output.outputDir" class="btn ghost sm" @click="st.patch((x) => (x.output.outputDir = null))">{{ t("common.reset") }}</button>
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.suffix") }}</b>
              <p>{{ t("settings.suffixHint", { s: s.output.suffix }) }}</p>
            </div>
            <input class="input" :value="s.output.suffix" spellcheck="false" @change="st.patch((x) => (x.output.suffix = ($event.target as HTMLInputElement).value))" />
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.structure") }}</b>
              <p>{{ t("settings.structureHint") }}</p>
            </div>
            <Toggle :model-value="s.output.preserveStructure" @update:model-value="(v) => st.patch((x) => (x.output.preserveStructure = v))" />
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.conflict") }}</b>
              <p>{{ t("settings.conflictHint") }}</p>
            </div>
            <select class="select" :value="s.output.conflict" @change="st.patch((x) => (x.output.conflict = ($event.target as HTMLSelectElement).value as ConflictPolicy))">
              <option value="number">{{ t("settings.conflictNumber") }}</option>
              <option value="skip">{{ t("settings.conflictSkip") }}</option>
              <option value="replace_output">{{ t("settings.conflictReplace") }}</option>
            </select>
          </div>
        </div>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.format") }}</b>
              <p>{{ t("settings.formatHint") }}</p>
            </div>
            <select class="select" :value="s.output.format" @change="st.patch((x) => (x.output.format = ($event.target as HTMLSelectElement).value as OutputFormat))">
              <option v-for="f in ['same', 'jpeg', 'png', 'webp', 'tiff', 'bmp']" :key="f" :value="f">{{ tx(`fmt.${f}`) }}</option>
            </select>
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.jpegQuality") }}</b>
              <p>{{ t("settings.jpegQualityHint") }}</p>
            </div>
            <select class="select" :value="s.output.jpegQuality" @change="st.patch((x) => (x.output.jpegQuality = ($event.target as HTMLSelectElement).value as JpegQuality))">
              <option value="preserve">{{ t("settings.jpegPreserve") }}</option>
              <option value="q90">90</option>
              <option value="q95">{{ t("settings.jpeg95") }}</option>
              <option value="q100">100</option>
            </select>
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.metadata") }}</b>
              <p>{{ t("settings.metadataHint") }}</p>
            </div>
            <Toggle :model-value="s.output.keepMetadata" @update:model-value="(v) => st.patch((x) => (x.output.keepMetadata = v))" />
          </div>
        </div>
        <div class="card group danger-zone">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.overwrite") }}</b>
              <p>{{ t("settings.overwriteHint") }}</p>
            </div>
            <Toggle :model-value="s.output.overwriteOriginals" @update:model-value="toggleOverwrite" />
          </div>
        </div>
      </template>

      <!-- 预设 -->
      <template v-else-if="section === 'presets'">
        <h2>{{ t("settings.presets") }}</h2>
        <p class="lead">{{ t("settings.presetsLead") }}</p>
        <div class="card group">
          <div v-for="p in st.presets" :key="p.id" class="field">
            <div class="grow">
              <b>{{ p.name }}</b>
              <p>
                {{
                  t("settings.presetLine", {
                    mode: autoModeLabel(p.detectionMode),
                    t: Math.round(p.confidenceThreshold * 100),
                    q: qualityModeLabel(p.removalQuality),
                    fmt: p.outputFormat === "same" ? t("settings.originalFormat") : p.outputFormat.toUpperCase(),
                  })
                }}
              </p>
            </div>
            <span v-if="p.builtin" class="tag">{{ t("common.builtin") }}</span>
            <button class="btn sm" @click="applyPreset(p.id, p.name)">{{ t("common.apply") }}</button>
            <button v-if="!p.builtin" class="btn ghost sm icon" :title="t('common.delete')" @click="st.deletePreset(p.id)"><Icon name="trash" :size="13" /></button>
          </div>
          <form class="field" @submit.prevent="addPreset">
            <input v-model="newPreset" class="input grow" :placeholder="t('settings.newPreset')" />
            <button class="btn primary sm" type="submit" :disabled="!newPreset.trim()">{{ t("common.save") }}</button>
          </form>
        </div>
      </template>

      <!-- 性能 -->
      <template v-else-if="section === 'performance'">
        <h2>{{ t("settings.performance") }}</h2>
        <p class="lead">{{ t("settings.perfLead") }}</p>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.workers") }}</b>
              <p>{{ t("settings.workersHint") }}</p>
            </div>
            <input
              class="input num"
              type="number"
              min="1"
              max="64"
              :placeholder="t('settings.auto')"
              :value="s.performance.cpuWorkers ?? ''"
              @change="st.patch((x) => (x.performance.cpuWorkers = numOrNull($event)))"
            />
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.memory") }}</b>
              <p>{{ t("settings.memoryHint") }}</p>
            </div>
            <input
              class="input num"
              type="number"
              min="512"
              step="256"
              placeholder="3072"
              :value="s.performance.memoryBudgetMb ?? ''"
              @change="st.patch((x) => (x.performance.memoryBudgetMb = numOrNull($event)))"
            />
          </div>
        </div>
        <div class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.cache") }}</b>
              <p>{{ t("settings.cacheHint", { size: st.info ? formatSize(st.info.cacheBytes) : "—" }) }}</p>
            </div>
            <button class="btn sm" @click="clearCache"><Icon name="trash" :size="13" />{{ t("settings.clearCache") }}</button>
          </div>
        </div>
      </template>

      <!-- 模型 -->
      <template v-else-if="section === 'models'">
        <h2>{{ t("settings.models") }}</h2>
        <p class="lead">{{ t("settings.modelsLead") }}</p>
        <div class="card group"><ModelStatus /></div>
      </template>

      <!-- 关于 -->
      <template v-else>
        <h2>{{ t("settings.about") }}</h2>
        <div class="card group about">
          <div class="row">
            <span class="logo"><Icon name="droplet" :size="20" /></span>
            <div>
              <b>Magies Clean</b>
              <p class="muted">{{ t("settings.version", { v: st.info?.version ?? "", p: st.info?.platform ?? "", a: st.info?.arch ?? "" }) }}</p>
            </div>
          </div>
          <ul>
            <li>{{ t("settings.privacy1") }}</li>
            <li>{{ t("settings.privacy2") }}</li>
            <li>{{ t("settings.privacy3") }}</li>
            <li>{{ t("settings.privacy4") }}</li>
          </ul>
        </div>
        <div v-if="st.info" class="card group">
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.dataDir") }}</b>
              <p class="mono truncate">{{ st.info.dataDir }}</p>
            </div>
            <button class="btn ghost sm" @click="api.revealPath(st.info.dataDir)">{{ t("common.show") }}</button>
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.logDir") }}</b>
              <p class="mono truncate">{{ st.info.logDir }}</p>
            </div>
          </div>
          <div class="field">
            <div class="grow">
              <b>{{ t("settings.modelsDir") }}</b>
              <p class="mono truncate">{{ st.info.modelsDir }}</p>
            </div>
          </div>
        </div>
      </template>
    </div>

    <Modal v-if="confirmOverwrite" :title="t('settings.overwriteTitle')" icon="alert" tone="danger" @close="confirmOverwrite = false">
      <p>{{ t("settings.overwriteBody1") }}</p>
      <p>{{ t("settings.overwriteBody2") }}</p>
      <template #footer>
        <button class="btn" @click="confirmOverwrite = false">{{ t("settings.keepOff") }}</button>
        <button class="btn danger solid" @click="enableOverwrite">{{ t("settings.overwriteConfirm") }}</button>
      </template>
    </Modal>
  </div>
</template>

<style scoped>
.settings {
  flex: 1;
  display: flex;
  min-height: 0;
}
.side {
  width: 220px;
  flex: none;
  padding: 18px 10px;
  border-right: 1px solid var(--border);
  background: var(--bg-elev);
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.side button {
  display: flex;
  align-items: center;
  gap: 10px;
  height: 34px;
  padding: 0 12px;
  border: none;
  border-radius: var(--r-8);
  background: transparent;
  color: var(--text-2);
  font-weight: 500;
  text-align: left;
  cursor: pointer;
}
.side button:hover {
  background: var(--surface-2);
  color: var(--text);
}
.side button.on {
  background: var(--accent-soft);
  color: var(--accent-text);
}
.content {
  flex: 1;
  overflow: auto;
  padding: 28px 36px 48px;
  max-width: 820px;
}
h2 {
  margin: 0 0 14px;
  font-size: var(--fs-20);
}
.lead {
  margin: -8px 0 18px;
  color: var(--text-2);
}
.group {
  padding: 4px 18px;
  margin-bottom: 14px;
}
.field {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 0;
  border-bottom: 1px solid var(--border);
}
.field:last-child {
  border-bottom: none;
}
.field p {
  margin: 2px 0 0;
  color: var(--text-2);
  font-size: var(--fs-12);
}
.field input[type="range"] {
  width: 160px;
}
.val {
  min-width: 40px;
  text-align: right;
  font-weight: 600;
}
.num {
  width: 96px;
}
.pad {
  padding: 14px 0 4px;
}
.warn-box {
  display: flex;
  gap: 8px;
  margin: 4px 0 8px;
  padding: 10px 12px;
  border-radius: var(--r-10);
  background: var(--warning-soft);
  color: var(--warning);
  font-size: var(--fs-12);
}
.scale {
  display: flex;
  height: 22px;
  margin: 4px 0 14px;
  border-radius: 6px;
  overflow: hidden;
  font-size: 10px;
  font-weight: 700;
  color: #fff;
}
.scale span {
  display: grid;
  place-items: center;
  overflow: hidden;
  white-space: nowrap;
}
.ig {
  background: var(--text-3);
}
.rv {
  background: var(--warning);
}
.au {
  background: var(--success);
}
.routes .route {
  display: flex;
  gap: 12px;
  padding: 10px 0;
  border-bottom: 1px solid var(--border);
}
.routes .route:last-child {
  border-bottom: none;
}
.routes svg {
  margin-top: 2px;
  color: var(--accent-text);
}
.routes p {
  margin: 2px 0 0;
  color: var(--text-2);
  font-size: var(--fs-12);
}
.danger-zone {
  border-color: color-mix(in srgb, var(--danger) 30%, transparent);
}
.tag {
  font-size: var(--fs-11);
  padding: 2px 8px;
  border-radius: var(--r-pill);
  background: var(--surface-3);
  color: var(--text-2);
}
.keys {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px 20px;
  padding: 8px 0 16px;
  color: var(--text-2);
}
.about {
  padding: 18px;
}
.about .logo {
  width: 40px;
  height: 40px;
  border-radius: 12px;
  display: grid;
  place-items: center;
  background: var(--brand-gradient);
  color: #fff;
}
.about p {
  margin: 2px 0 0;
}
.about ul {
  margin: 14px 0 0;
  padding-left: 18px;
  color: var(--text-2);
  line-height: 1.8;
}
</style>
