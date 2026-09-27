<script setup lang="ts">
import { t, tx, type MessageKey } from "../i18n";
import { computed, ref } from "vue";
import { useSettings } from "../stores/settings";
import type { ModelStatus } from "../types";
import Icon from "./Icon.vue";

const st = useSettings();
const loading = ref(false);
// 只列出已随应用提供的模型；没有提供的（缺失 / 未打包）不显示，对应能力由内置算法完成
const shown = computed(() => st.models.filter((m) => m.state.state !== "missing" && m.state.state !== "not_packaged"));

const FALLBACK: Record<ModelStatus["role"], MessageKey> = {
  detector: "models.detectorFallback",
  segmenter: "models.segmenterFallback",
  ocr_detector: "models.ocrFallback",
  ocr_recognizer: "models.ocrFallback",
  inpainting: "models.inpaintingFallback",
};
const STATE: Record<string, { label: MessageKey; cls: string }> = {
  ready: { label: "models.ready", cls: "ok" },
  missing: { label: "models.missing", cls: "off" },
  not_packaged: { label: "models.missing", cls: "off" },
  checksum_mismatch: { label: "models.checksum", cls: "bad" },
  load_failed: { label: "models.loadFailed", cls: "bad" },
  runtime_unavailable: { label: "models.runtime", cls: "bad" },
};

async function reload() {
  loading.value = true;
  try {
    await st.reloadModels();
  } finally {
    loading.value = false;
  }
}
</script>

<template>
  <div class="models">
    <div v-for="m in shown" :key="m.id" class="m">
      <span class="ic" :class="STATE[m.state.state]?.cls"><Icon :name="m.state.state === 'ready' ? 'check' : 'box'" :size="14" /></span>
      <div class="grow">
        <div class="row">
          <b>{{ tx(`models.${m.role}`) }}</b>
          <span class="ver mono">v{{ m.version }}</span>
        </div>
        <div class="subtle small">
          {{ m.state.state === "ready" ? t("models.cpu", { file: m.file }) : t(FALLBACK[m.role]) }}
          <template v-if="m.state.reason"> · {{ m.state.reason }}</template>
        </div>
      </div>
      <span class="st" :class="STATE[m.state.state]?.cls">{{ STATE[m.state.state] ? t(STATE[m.state.state].label) : m.state.state }}</span>
    </div>
    <div class="row foot">
      <span class="subtle small grow">{{ t("models.footer") }}</span>
      <button class="btn sm" :disabled="loading" @click="reload"><Icon name="refresh" :size="13" :class="{ spin: loading }" />{{ t("models.recheck") }}</button>
    </div>
  </div>
</template>

<style scoped>
.models {
  display: flex;
  flex-direction: column;
}
.m {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 0;
  border-bottom: 1px solid var(--border);
}
.ic {
  width: 28px;
  height: 28px;
  border-radius: 8px;
  display: grid;
  place-items: center;
  background: var(--surface-3);
  color: var(--text-3);
}
.ic.ok {
  background: var(--success-soft);
  color: var(--success);
}
.ic.bad {
  background: var(--danger-soft);
  color: var(--danger);
}
.ver {
  font-size: var(--fs-11);
  color: var(--text-3);
}
.small {
  font-size: var(--fs-12);
}
.st {
  font-size: var(--fs-12);
  font-weight: 600;
}
.st.ok {
  color: var(--success);
}
.st.off {
  color: var(--text-3);
}
.st.bad {
  color: var(--danger);
}
.foot {
  padding-top: 10px;
}
</style>
