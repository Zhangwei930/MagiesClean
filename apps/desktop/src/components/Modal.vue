<script setup lang="ts">
import { t } from "../i18n";
import { onBeforeUnmount, onMounted } from "vue";
import Icon from "./Icon.vue";

const props = defineProps<{ title: string; width?: number; icon?: string; tone?: "default" | "warning" | "danger" }>();
const emit = defineEmits<{ close: [] }>();

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") emit("close");
}
onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));
</script>

<template>
  <Teleport to="body">
    <div class="backdrop" @mousedown.self="emit('close')">
      <div class="modal card" :style="{ width: `${props.width ?? 480}px` }" role="dialog" aria-modal="true">
        <header>
          <span v-if="icon" class="ic" :class="tone ?? 'default'"><Icon :name="icon" :size="18" /></span>
          <h3>{{ title }}</h3>
          <button class="btn ghost sm icon close" :aria-label="t('common.close')" @click="emit('close')"><Icon name="x" /></button>
        </header>
        <div class="body"><slot /></div>
        <footer v-if="$slots.footer"><slot name="footer" /></footer>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.backdrop {
  position: fixed;
  inset: 0;
  background: var(--overlay);
  backdrop-filter: blur(3px);
  display: grid;
  place-items: center;
  z-index: 100;
  animation: fade var(--dur) var(--ease);
}
@keyframes fade {
  from {
    opacity: 0;
  }
}
.modal {
  max-width: calc(100vw - 48px);
  max-height: calc(100vh - 80px);
  display: flex;
  flex-direction: column;
  box-shadow: var(--shadow-lg);
  border-radius: var(--r-16);
  animation: rise var(--dur) var(--ease);
}
@keyframes rise {
  from {
    transform: translateY(8px) scale(0.985);
    opacity: 0;
  }
}
header {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 18px 20px 6px;
}
h3 {
  margin: 0;
  font-size: var(--fs-16);
  font-weight: 650;
  flex: 1;
}
.ic {
  width: 32px;
  height: 32px;
  border-radius: 10px;
  display: grid;
  place-items: center;
}
.ic.default {
  background: var(--accent-soft);
  color: var(--accent-text);
}
.ic.warning {
  background: var(--warning-soft);
  color: var(--warning);
}
.ic.danger {
  background: var(--danger-soft);
  color: var(--danger);
}
.close {
  margin-right: -6px;
}
.body {
  padding: 8px 20px 16px;
  overflow: auto;
  color: var(--text-2);
}
footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 20px 18px;
  border-top: 1px solid var(--border);
}
</style>
