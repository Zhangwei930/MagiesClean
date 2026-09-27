<script setup lang="ts">
import { t } from "../i18n";
import { computed } from "vue";
import type { CandidateDecision } from "../types";

const props = defineProps<{ value: number; decision?: CandidateDecision; compact?: boolean }>();

const tone = computed(() => {
  if (props.decision === "auto") return "success";
  if (props.decision === "review") return "warning";
  if (props.decision === "ignore") return "neutral";
  return props.value >= 0.85 ? "success" : props.value >= 0.7 ? "warning" : "neutral";
});
</script>

<template>
  <span class="conf" :class="[tone, { compact }]" :title="t('decision.confidence', { v: `${Math.round(value * 100)}%` })">
    <span class="bar"><span class="fill" :style="{ width: `${Math.round(value * 100)}%` }" /></span>
    <span class="num">{{ Math.round(value * 100) }}%</span>
  </span>
</template>

<style scoped>
.conf {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: var(--fs-12);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.bar {
  width: 36px;
  height: 4px;
  border-radius: 4px;
  background: var(--surface-3);
  overflow: hidden;
}
.compact .bar {
  width: 22px;
}
.fill {
  display: block;
  height: 100%;
  border-radius: 4px;
  background: currentColor;
}
.success {
  color: var(--success);
}
.warning {
  color: var(--warning);
}
.neutral {
  color: var(--text-3);
}
</style>
