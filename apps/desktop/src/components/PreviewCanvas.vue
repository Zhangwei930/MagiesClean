<script setup lang="ts">
import { t } from "../i18n";
// 预览画布：Zoom、Pan、Fit、100%、原图 / 结果 / 对比滑块、Mask 叠加、候选框，以及 MaskEditor。
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { assetUrl } from "../services/ipc";
import type { FileView, MaskOp } from "../types";
import Icon from "./Icon.vue";
import MaskEditor, { type Tool } from "./MaskEditor.vue";
import Segmented from "./Segmented.vue";
import WatermarkOverlay from "./WatermarkOverlay.vue";

type Mode = "original" | "result" | "compare";

const props = defineProps<{ file: FileView; activeId: string | null; busy: boolean }>();
const emit = defineEmits<{
  select: [id: string];
  generate: [];
  maskOp: [op: MaskOp];
  undo: [redo: boolean];
}>();

const mode = ref<Mode>("original");
const showMask = ref(true);
const showBoxes = ref(true);
const editing = ref(false);
const tool = ref<Tool>("brush");
const radius = ref(14);
const split = ref(0.5);

const viewport = ref<HTMLElement | null>(null);
const scale = ref(1);
const tx = ref(0);
const ty = ref(0);
const spaceDown = ref(false);
let panning: { x: number; y: number; tx: number; ty: number } | null = null;

const size = computed<[number, number]>(() => props.file.previewSize ?? [1200, 800]);
const origW = computed(() => (props.file.info?.type === "image" ? props.file.info.width : size.value[0]));
const hasResult = computed(() => !!props.file.resultPreview && props.file.resultCurrent);
const zoomLabel = computed(() => `${Math.round((scale.value * size.value[0] * 100) / origW.value)}%`);

function fit() {
  const el = viewport.value;
  if (!el) return;
  const [w, h] = size.value;
  const s = Math.min((el.clientWidth - 48) / w, (el.clientHeight - 48) / h);
  scale.value = Math.max(0.02, s);
  tx.value = (el.clientWidth - w * scale.value) / 2;
  ty.value = (el.clientHeight - h * scale.value) / 2;
}

function zoomAt(factor: number, cx?: number, cy?: number) {
  const el = viewport.value;
  if (!el) return;
  const px = cx ?? el.clientWidth / 2;
  const py = cy ?? el.clientHeight / 2;
  const ns = Math.min(40, Math.max(0.02, scale.value * factor));
  tx.value = px - ((px - tx.value) * ns) / scale.value;
  ty.value = py - ((py - ty.value) * ns) / scale.value;
  scale.value = ns;
}

/** 100%：一个原图像素对应一个屏幕像素。 */
function actual() {
  const target = origW.value / size.value[0];
  zoomAt(target / scale.value);
}

function onWheel(e: WheelEvent) {
  e.preventDefault();
  const r = viewport.value!.getBoundingClientRect();
  if (e.ctrlKey || e.metaKey) {
    zoomAt(Math.exp(-e.deltaY * 0.0022), e.clientX - r.left, e.clientY - r.top);
  } else {
    tx.value -= e.deltaX;
    ty.value -= e.deltaY;
  }
}

function onDown(e: MouseEvent) {
  const panMode = !editing.value || spaceDown.value || e.button === 1;
  if (!panMode) return;
  panning = { x: e.clientX, y: e.clientY, tx: tx.value, ty: ty.value };
}
function onMove(e: MouseEvent) {
  if (panning) {
    tx.value = panning.tx + e.clientX - panning.x;
    ty.value = panning.ty + e.clientY - panning.y;
  }
}
function onUp() {
  panning = null;
}

// 对比滑块
let dragSplit = false;
function splitDown(e: PointerEvent) {
  e.stopPropagation();
  dragSplit = true;
  (e.target as HTMLElement).setPointerCapture(e.pointerId);
}
function splitMove(e: PointerEvent) {
  if (!dragSplit) return;
  const img = viewport.value!.querySelector(".content") as HTMLElement;
  const r = img.getBoundingClientRect();
  split.value = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
}
function splitUp() {
  dragSplit = false;
}

// 当前文件的结果刚生成（例如确认去除后的自动预览）：切到“处理后”视图
watch([() => props.file.id, hasResult], ([id, has], [prevId, had]) => {
  if (id === prevId && has && !had && mode.value === "original" && !editing.value) mode.value = "result";
});

function setMode(m: Mode) {
  if (m !== "original" && !hasResult.value) emit("generate");
  mode.value = m;
}

function toggleBefore() {
  setMode(mode.value === "original" ? "result" : "original");
}
function toggleMask() {
  showMask.value = !showMask.value;
}

function onKey(e: KeyboardEvent) {
  if (e.code === "Space" && !(e.target instanceof HTMLInputElement)) {
    spaceDown.value = e.type === "keydown";
    if (e.type === "keydown") e.preventDefault();
  }
}

onMounted(() => {
  nextTick(fit);
  window.addEventListener("keydown", onKey);
  window.addEventListener("keyup", onKey);
  window.addEventListener("resize", fit);
});
onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("keyup", onKey);
  window.removeEventListener("resize", fit);
});

watch(
  () => props.file.id,
  () => {
    editing.value = false;
    mode.value = "original";
    nextTick(fit);
  },
);
watch(size, () => nextTick(fit));
// 结果生成后自动切换到对比
watch(hasResult, (v, old) => {
  if (v && !old && mode.value === "original") mode.value = "compare";
});

defineExpose({
  fit,
  actual,
  zoomIn: () => zoomAt(1.25),
  zoomOut: () => zoomAt(0.8),
  toggleBefore,
  toggleMask,
  edit: () => {
    mode.value = "original";
    editing.value = true;
  },
});

const modes = computed(() => [
  { value: "original" as Mode, label: t("preview.original") },
  { value: "result" as Mode, label: t("preview.result") },
  { value: "compare" as Mode, label: t("preview.compare"), icon: "compare" },
]);
const tools = computed<{ value: Tool; label: string; icon: string; hint: string }[]>(() => [
  { value: "brush", label: t("preview.brush"), icon: "brush", hint: t("preview.brushHint") },
  { value: "eraser", label: t("preview.eraser"), icon: "eraser", hint: t("preview.eraserHint") },
  { value: "rect", label: t("preview.rect"), icon: "square", hint: t("preview.altHint") },
  { value: "lasso", label: t("preview.lasso"), icon: "lasso", hint: t("preview.altHint") },
]);
</script>

<template>
  <section class="preview">
    <div class="toolbar">
      <Segmented :model-value="mode" :options="modes" @update:model-value="setMode" />
      <div class="sep" />
      <button class="btn ghost sm" :class="{ pressed: showMask }" :title="t('preview.maskTip')" @click="toggleMask">
        <Icon :name="showMask ? 'eye' : 'eye-off'" :size="14" />{{ t("preview.mask") }}
      </button>
      <button class="btn ghost sm" :class="{ pressed: showBoxes }" :title="t('preview.boxesTip')" @click="showBoxes = !showBoxes">
        <Icon name="frame" :size="14" />{{ t("preview.boxes") }}
      </button>
      <div class="sep" />
      <button class="btn sm" :class="{ primary: editing }" :title="t('preview.editTip')" @click="editing = !editing">
        <Icon name="brush" :size="14" />{{ editing ? t("preview.doneEditing") : t("preview.editMask") }}
      </button>
      <span class="grow" />
      <button class="btn ghost sm icon" :title="t('preview.zoomOut')" @click="zoomAt(0.8)"><Icon name="zoom-out" :size="15" /></button>
      <span class="zoom mono">{{ zoomLabel }}</span>
      <button class="btn ghost sm icon" :title="t('preview.zoomIn')" @click="zoomAt(1.25)"><Icon name="zoom-in" :size="15" /></button>
      <button class="btn ghost sm" :title="t('preview.fitTip')" @click="fit"><Icon name="maximize" :size="14" />{{ t("preview.fit") }}</button>
      <button class="btn ghost sm" :title="t('preview.actualTip')" @click="actual">100%</button>
    </div>

    <Transition name="fade">
      <div v-if="editing" class="edit-bar">
        <Segmented v-model="tool" :options="tools" size="sm" />
        <label v-if="tool === 'brush' || tool === 'eraser'" class="row radius">
          <span class="subtle">{{ t("preview.brushSize") }}</span>
          <input v-model.number="radius" type="range" min="2" max="80" />
          <span class="mono">{{ radius }}</span>
        </label>
        <span class="grow" />
        <button class="btn ghost sm icon" :disabled="!file.canUndoMask" :title="t('preview.undo')" @click="emit('undo', false)"><Icon name="undo" :size="15" /></button>
        <button class="btn ghost sm icon" :disabled="!file.canRedoMask" :title="t('preview.redo')" @click="emit('undo', true)"><Icon name="redo" :size="15" /></button>
        <button class="btn ghost sm danger" @click="emit('maskOp', { op: 'clear' })"><Icon name="trash" :size="14" />{{ t("preview.clear") }}</button>
      </div>
    </Transition>

    <div
      ref="viewport"
      class="viewport"
      :class="{ grabbing: !!panning, pan: !editing || spaceDown }"
      @wheel="onWheel"
      @mousedown="onDown"
      @mousemove="onMove"
      @mouseup="onUp"
      @mouseleave="onUp"
      @dblclick="fit"
    >
      <div
        class="content"
        :style="{ width: `${size[0]}px`, height: `${size[1]}px`, transform: `translate(${tx}px, ${ty}px) scale(${scale})` }"
      >
        <img v-if="file.preview" class="layer" :src="assetUrl(file.preview)" draggable="false" :alt="t('preview.original')" />
        <img
          v-if="hasResult && mode !== 'original'"
          class="layer"
          :src="assetUrl(file.resultPreview)"
          draggable="false"
          :alt="t('preview.result')"
          :style="mode === 'compare' ? { clipPath: `inset(0 0 0 ${split * 100}%)` } : undefined"
        />
        <img
          v-if="showMask && file.maskOverlay && (mode === 'original' || editing)"
          class="layer mask"
          :src="assetUrl(file.maskOverlay)"
          draggable="false"
          alt=""
        />
        <WatermarkOverlay
          v-if="showBoxes && !editing && mode === 'original'"
          :candidates="file.candidates"
          :active-id="activeId"
          :scale="scale"
          @select="emit('select', $event)"
        />
        <MaskEditor v-if="editing && !spaceDown" :tool="tool" :radius="radius" :width="size[0]" :height="size[1]" @commit="emit('maskOp', $event)" />
        <template v-if="mode === 'compare' && hasResult">
          <div class="split" :style="{ left: `${split * 100}%`, width: `${2 / scale}px` }">
            <div class="handle" :style="{ transform: `translate(-50%, -50%) scale(${1 / scale})` }" @pointerdown="splitDown" @pointermove="splitMove" @pointerup="splitUp">
              <Icon name="compare" :size="16" />
            </div>
          </div>
        </template>
      </div>

      <div v-if="mode === 'compare' && hasResult" class="labels">
        <span>{{ t("preview.original") }}</span>
        <span>{{ t("preview.result") }}</span>
      </div>

      <div v-if="busy" class="busy">
        <Icon name="loader" :size="18" class="spin" />
        {{ t("preview.generating") }}
      </div>
      <div v-else-if="mode !== 'original' && !hasResult" class="busy muted">
        <Icon name="info" :size="16" />
        {{ file.summary.toRemove || file.hasManualMask ? t("preview.notGenerated") : t("preview.nothing") }}
      </div>
    </div>
  </section>
</template>

<style scoped>
.preview {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: var(--canvas);
  position: relative;
}
.toolbar,
.edit-bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 8px 12px;
  background: var(--bg-elev);
  border-bottom: 1px solid var(--border);
  /* 窗口较窄时横向滚动，不挤压按钮文字 */
  overflow-x: auto;
  scrollbar-width: none;
}
.toolbar::-webkit-scrollbar,
.edit-bar::-webkit-scrollbar {
  display: none;
}
.toolbar > *,
.edit-bar > * {
  flex-shrink: 0;
}
.edit-bar {
  background: var(--accent-soft);
  border-bottom-color: color-mix(in srgb, var(--accent) 25%, transparent);
}
.sep {
  width: 1px;
  height: 18px;
  background: var(--border);
  margin: 0 4px;
}
.pressed {
  color: var(--accent-text);
  background: var(--accent-soft);
}
.zoom {
  min-width: 46px;
  text-align: center;
  color: var(--text-2);
  font-size: var(--fs-12);
}
.radius {
  gap: 8px;
  margin-left: 8px;
}
.radius input {
  width: 110px;
}
.viewport {
  position: relative;
  flex: 1;
  overflow: hidden;
  background-color: var(--canvas);
  background-image: linear-gradient(45deg, var(--canvas-check) 25%, transparent 25%), linear-gradient(-45deg, var(--canvas-check) 25%, transparent 25%),
    linear-gradient(45deg, transparent 75%, var(--canvas-check) 75%), linear-gradient(-45deg, transparent 75%, var(--canvas-check) 75%);
  background-size: 20px 20px;
  background-position: 0 0, 0 10px, 10px -10px, -10px 0;
}
.viewport.pan {
  cursor: grab;
}
.viewport.grabbing {
  cursor: grabbing;
}
.content {
  position: absolute;
  left: 0;
  top: 0;
  transform-origin: 0 0;
  box-shadow: 0 8px 30px rgba(0, 0, 0, 0.25);
  will-change: transform;
}
.layer {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  image-rendering: auto;
}
.mask {
  opacity: 0.9;
}
.split {
  position: absolute;
  top: 0;
  bottom: 0;
  background: #fff;
  box-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.3);
  transform: translateX(-50%);
}
.handle {
  position: absolute;
  top: 50%;
  left: 50%;
  width: 34px;
  height: 34px;
  border-radius: 50%;
  background: #fff;
  color: #101828;
  display: grid;
  place-items: center;
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35);
  cursor: ew-resize;
  touch-action: none;
}
.labels {
  position: absolute;
  top: 12px;
  left: 12px;
  right: 12px;
  display: flex;
  justify-content: space-between;
  pointer-events: none;
}
.labels span {
  padding: 3px 9px;
  border-radius: var(--r-pill);
  background: rgba(16, 24, 40, 0.7);
  color: #fff;
  font-size: var(--fs-11);
  font-weight: 600;
}
.busy {
  position: absolute;
  left: 50%;
  bottom: 20px;
  transform: translateX(-50%);
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 14px;
  border-radius: var(--r-pill);
  background: var(--surface);
  box-shadow: var(--shadow-lg);
  font-weight: 500;
}
</style>
