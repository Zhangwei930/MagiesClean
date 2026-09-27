<script setup lang="ts">
import { t } from "../i18n";
// PDF 预览：pdf.js 在本地渲染原件或处理后的 PDF（均为本地 asset），叠加原生水印候选框。
import * as pdfjs from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { assetUrl } from "../services/ipc";
import type { FileView } from "../types";
import Icon from "./Icon.vue";
import Segmented from "./Segmented.vue";
import WatermarkOverlay from "./WatermarkOverlay.vue";

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

const props = defineProps<{ file: FileView; activeId: string | null; busy: boolean; password?: string }>();
const emit = defineEmits<{ select: [id: string]; generate: [] }>();

type Mode = "original" | "result";
const mode = ref<Mode>("original");
const page = ref(0);
const pageCount = ref(0);
const canvas = ref<HTMLCanvasElement | null>(null);
const wrap = ref<HTMLElement | null>(null);
const cssSize = ref<[number, number]>([600, 800]);
const error = ref<string | null>(null);
const loading = ref(false);
const doc = shallowRef<pdfjs.PDFDocumentProxy | null>(null);
let renderTask: pdfjs.RenderTask | null = null;

const hasResult = computed(() => !!props.file.resultFile && props.file.resultCurrent);
const source = computed(() => (mode.value === "result" && hasResult.value ? props.file.resultFile : props.file.path));
const pageCandidates = computed(() => props.file.candidates.filter((c) => (c.page ?? 0) === page.value));

async function load() {
  error.value = null;
  loading.value = true;
  try {
    await doc.value?.destroy();
    doc.value = null;
    if (props.file.needsPassword && !props.password) {
      error.value = t("pdf.needsPassword");
      return;
    }
    const url = assetUrl(source.value);
    if (!url) return;
    doc.value = await pdfjs.getDocument({ url, password: props.password, isEvalSupported: false }).promise;
    pageCount.value = doc.value.numPages;
    page.value = Math.min(page.value, pageCount.value - 1);
    await render();
  } catch (e) {
    error.value = t("pdf.cannotPreview", { msg: (e as Error).message });
  } finally {
    loading.value = false;
  }
}

async function render() {
  if (!doc.value || !canvas.value || !wrap.value) return;
  renderTask?.cancel();
  const p = await doc.value.getPage(page.value + 1);
  const base = p.getViewport({ scale: 1 });
  const avail = wrap.value.clientWidth - 64;
  const availH = wrap.value.clientHeight - 64;
  const s = Math.min(avail / base.width, availH / base.height);
  const dpr = window.devicePixelRatio || 1;
  const vp = p.getViewport({ scale: s * dpr });
  canvas.value.width = vp.width;
  canvas.value.height = vp.height;
  cssSize.value = [vp.width / dpr, vp.height / dpr];
  renderTask = p.render({ canvasContext: canvas.value.getContext("2d")!, viewport: vp });
  try {
    await renderTask.promise;
  } catch {
    /* 取消渲染 */
  }
}

function setMode(m: Mode) {
  if (m === "result" && !hasResult.value) emit("generate");
  mode.value = m;
}

watch(() => [props.file.id, source.value, props.password], () => nextTick(load), { immediate: true });
watch(page, render);
watch(hasResult, (v) => {
  if (v) mode.value = "result";
});
onBeforeUnmount(() => {
  renderTask?.cancel();
  doc.value?.destroy();
});
</script>

<template>
  <section class="pdf">
    <div class="toolbar">
      <Segmented :model-value="mode" :options="[{ value: 'original', label: t('pdf.original') }, { value: 'result', label: t('pdf.result') }]" @update:model-value="setMode" />
      <span class="grow" />
      <button class="btn ghost sm icon" :disabled="page <= 0" @click="page--"><Icon name="chevron-left" /></button>
      <span class="mono pages">{{ page + 1 }} / {{ pageCount || "–" }}</span>
      <button class="btn ghost sm icon" :disabled="page >= pageCount - 1" @click="page++"><Icon name="chevron-right" /></button>
    </div>
    <div ref="wrap" class="stage">
      <div class="paper" :style="{ width: `${cssSize[0]}px`, height: `${cssSize[1]}px` }">
        <canvas ref="canvas" :style="{ width: `${cssSize[0]}px`, height: `${cssSize[1]}px` }" />
        <WatermarkOverlay v-if="mode === 'original'" :candidates="pageCandidates" :active-id="activeId" :scale="1" @select="emit('select', $event)" />
      </div>
      <div v-if="error" class="state"><Icon name="lock" :size="16" />{{ error }}</div>
      <div v-else-if="busy || loading" class="state"><Icon name="loader" :size="16" class="spin" />{{ busy ? t("pdf.processing") : t("pdf.loading") }}</div>
    </div>
  </section>
</template>

<style scoped>
.pdf {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--canvas);
}
.toolbar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 12px;
  background: var(--bg-elev);
  border-bottom: 1px solid var(--border);
}
.pages {
  font-size: var(--fs-12);
  color: var(--text-2);
  min-width: 56px;
  text-align: center;
}
.stage {
  position: relative;
  flex: 1;
  overflow: auto;
  display: grid;
  place-items: center;
  padding: 32px;
}
.paper {
  position: relative;
  background: #fff;
  box-shadow: 0 10px 40px rgba(0, 0, 0, 0.25);
}
.state {
  position: absolute;
  left: 50%;
  bottom: 20px;
  transform: translateX(-50%);
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 8px 14px;
  border-radius: var(--r-pill);
  background: var(--surface);
  box-shadow: var(--shadow-lg);
}
</style>
