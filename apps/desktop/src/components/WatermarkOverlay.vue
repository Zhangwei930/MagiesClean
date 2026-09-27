<script setup lang="ts">
import type { CandidateView } from "../types";

// 候选框（归一化坐标）。颜色表达决策：自动去除 / 需复核 / 忽略。
defineProps<{ candidates: CandidateView[]; activeId: string | null; scale: number }>();
defineEmits<{ select: [id: string] }>();

function tone(c: CandidateView) {
  if (c.userAction === "ignore" || (c.decision === "ignore" && c.userAction === "pending")) return "ignore";
  if (c.willRemove) return "remove";
  return "review";
}
</script>

<template>
  <div class="overlay">
    <div
      v-for="c in candidates"
      :key="c.id"
      class="box"
      :class="[tone(c), { active: c.id === activeId }]"
      :style="{
        left: `${c.bbox.x * 100}%`,
        top: `${c.bbox.y * 100}%`,
        width: `${c.bbox.width * 100}%`,
        height: `${c.bbox.height * 100}%`,
        borderWidth: `${1.5 / scale}px`,
      }"
      @mousedown.stop
      @click.stop="$emit('select', c.id)"
    >
      <span class="tag" :style="{ transform: `scale(${1 / scale})` }">{{ c.typeLabel }} {{ Math.round(c.confidence * 100) }}%</span>
    </div>
  </div>
</template>

<style scoped>
.overlay {
  position: absolute;
  inset: 0;
  pointer-events: none;
}
.box {
  position: absolute;
  border-style: solid;
  border-radius: 2px;
  pointer-events: auto;
  cursor: pointer;
  transition: background var(--dur-fast);
}
.box.remove {
  border-color: #12b76a;
  background: rgba(18, 183, 106, 0.06);
}
.box.review {
  border-color: #f79009;
  border-style: dashed;
  background: rgba(247, 144, 9, 0.07);
}
.box.ignore {
  border-color: rgba(152, 162, 179, 0.8);
  border-style: dotted;
}
.box.active {
  background: rgba(84, 70, 240, 0.12);
  border-color: #7b6ffb;
  border-style: solid;
}
.tag {
  position: absolute;
  left: -1px;
  bottom: 100%;
  margin-bottom: 3px;
  transform-origin: left bottom;
  white-space: nowrap;
  padding: 2px 6px;
  border-radius: 5px;
  font-size: 11px;
  font-weight: 600;
  color: #fff;
  background: rgba(16, 24, 40, 0.78);
  pointer-events: none;
}
.remove .tag {
  background: #079455;
}
.review .tag {
  background: #dc6803;
}
.active .tag {
  background: #5446f0;
}
</style>
