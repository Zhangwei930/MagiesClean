<script setup lang="ts">
// 手动修正 Mask：画笔、橡皮擦、矩形、套索。实时在本地画布上反馈笔迹，
// 松开后把矢量操作（归一化坐标）交给后端栅格化；不在 IPC 中搬运像素，也不改写原图。
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { MaskOp, Point } from "../types";

export type Tool = "brush" | "eraser" | "rect" | "lasso";

const props = defineProps<{ tool: Tool; radius: number; width: number; height: number }>();
const emit = defineEmits<{ commit: [op: MaskOp] }>();

const canvas = ref<HTMLCanvasElement | null>(null);
// 按住 Alt / Option 时矩形与套索为擦除
let eraseMod = false;
let drawing = false;
let pts: Point[] = [];
let start: Point | null = null;

function ctx() {
  return canvas.value?.getContext("2d") ?? null;
}

function clear() {
  const c = ctx();
  if (c && canvas.value) c.clearRect(0, 0, canvas.value.width, canvas.value.height);
}

onMounted(clear);
watch(() => [props.width, props.height], clear);

function local(e: PointerEvent): Point {
  const r = canvas.value!.getBoundingClientRect();
  return { x: ((e.clientX - r.left) / r.width) * props.width, y: ((e.clientY - r.top) / r.height) * props.height };
}

function paint() {
  const c = ctx();
  if (!c) return;
  clear();
  const erase = props.tool === "eraser";
  c.fillStyle = erase ? "rgba(255,255,255,0.55)" : "rgba(255,70,112,0.55)";
  c.strokeStyle = erase ? "rgba(255,255,255,0.85)" : "rgba(255,70,112,0.9)";
  if (props.tool === "brush" || props.tool === "eraser") {
    c.lineCap = "round";
    c.lineJoin = "round";
    c.lineWidth = props.radius * 2;
    c.strokeStyle = c.fillStyle;
    c.beginPath();
    pts.forEach((p, i) => (i ? c.lineTo(p.x, p.y) : c.moveTo(p.x, p.y)));
    if (pts.length === 1) c.lineTo(pts[0].x + 0.01, pts[0].y);
    c.stroke();
  } else if (props.tool === "rect" && start && pts.length) {
    const p = pts[pts.length - 1];
    c.fillRect(Math.min(start.x, p.x), Math.min(start.y, p.y), Math.abs(p.x - start.x), Math.abs(p.y - start.y));
    c.lineWidth = 1.5;
    c.strokeRect(Math.min(start.x, p.x), Math.min(start.y, p.y), Math.abs(p.x - start.x), Math.abs(p.y - start.y));
  } else if (props.tool === "lasso" && pts.length > 1) {
    c.beginPath();
    pts.forEach((p, i) => (i ? c.lineTo(p.x, p.y) : c.moveTo(p.x, p.y)));
    c.closePath();
    c.fill();
    c.lineWidth = 1.5;
    c.stroke();
  }
}

function down(e: PointerEvent) {
  if (e.button !== 0) return;
  e.stopPropagation();
  (e.target as HTMLElement).setPointerCapture(e.pointerId);
  drawing = true;
  const p = local(e);
  start = p;
  pts = [p];
  paint();
}

function move(e: PointerEvent) {
  if (!drawing) return;
  const p = local(e);
  const last = pts[pts.length - 1];
  if (Math.hypot(p.x - last.x, p.y - last.y) < 1.2) return;
  pts.push(p);
  paint();
}

function up() {
  if (!drawing) return;
  drawing = false;
  const W = props.width;
  const H = props.height;
  const n = (p: Point) => ({ x: p.x / W, y: p.y / H });
  let op: MaskOp | null = null;
  if (props.tool === "brush" || props.tool === "eraser") {
    op = { op: "brush", points: pts.map(n), radius: props.radius / Math.max(W, H), erase: props.tool === "eraser" };
  } else if (props.tool === "rect" && start && pts.length > 1) {
    const p = pts[pts.length - 1];
    const a = n({ x: Math.min(start.x, p.x), y: Math.min(start.y, p.y) });
    op = { op: "rect", x: a.x, y: a.y, width: Math.abs(p.x - start.x) / W, height: Math.abs(p.y - start.y) / H, erase: eraseMod };
  } else if (props.tool === "lasso" && pts.length > 2) {
    op = { op: "polygon", points: pts.map(n), erase: eraseMod };
  }
  pts = [];
  start = null;
  if (op) emit("commit", op);
  // 后端返回新的叠加层后再清除本地笔迹，避免闪烁
  setTimeout(clear, 350);
}

function key(e: KeyboardEvent) {
  eraseMod = e.altKey;
}
onMounted(() => {
  window.addEventListener("keydown", key);
  window.addEventListener("keyup", key);
});
onBeforeUnmount(() => {
  window.removeEventListener("keydown", key);
  window.removeEventListener("keyup", key);
});
</script>

<template>
  <canvas
    ref="canvas"
    class="editor"
    :class="tool"
    :width="width"
    :height="height"
    @pointerdown="down"
    @pointermove="move"
    @pointerup="up"
    @pointercancel="up"
  />
</template>

<style scoped>
.editor {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  touch-action: none;
  cursor: crosshair;
}
</style>
