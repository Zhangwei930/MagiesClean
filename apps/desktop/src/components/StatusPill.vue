<script setup lang="ts">
import { computed } from "vue";
import { STATUS_TONE, statusLabel } from "../composables/format";
import type { FileStatus } from "../types";

const props = defineProps<{ status: FileStatus }>();
const meta = computed(() => ({ label: statusLabel(props.status), tone: STATUS_TONE[props.status] }));
</script>

<template>
  <span class="pill" :class="meta.tone">
    <span class="dot" :class="{ pulse: status === 'scanning' || status === 'processing' }" />
    {{ meta.label }}
  </span>
</template>

<style scoped>
.pill {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 20px;
  padding: 0 8px;
  border-radius: var(--r-pill);
  font-size: var(--fs-11);
  font-weight: 600;
  white-space: nowrap;
}
.dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
}
.pulse {
  animation: pulse 1.2s ease-in-out infinite;
}
@keyframes pulse {
  50% {
    opacity: 0.35;
  }
}
.neutral {
  background: var(--surface-3);
  color: var(--text-2);
}
.info {
  background: var(--info-soft);
  color: var(--info);
}
.accent {
  background: var(--accent-soft);
  color: var(--accent-text);
}
.success {
  background: var(--success-soft);
  color: var(--success);
}
.warning {
  background: var(--warning-soft);
  color: var(--warning);
}
.danger {
  background: var(--danger-soft);
  color: var(--danger);
}
</style>
