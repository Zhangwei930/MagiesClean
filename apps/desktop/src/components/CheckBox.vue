<script setup lang="ts">
import Icon from "./Icon.vue";

defineProps<{ checked: boolean; indeterminate?: boolean; label?: string }>();
defineEmits<{ toggle: [e: MouseEvent] }>();
</script>

<template>
  <button
    type="button"
    class="cb"
    role="checkbox"
    :aria-checked="indeterminate ? 'mixed' : checked"
    :aria-label="label"
    :class="{ on: checked || indeterminate }"
    @click.stop="$emit('toggle', $event)"
  >
    <span class="box">
      <span v-if="indeterminate" class="dash" />
      <Icon v-else-if="checked" name="check" :size="12" />
    </span>
    <span v-if="label" class="label">{{ label }}</span>
  </button>
</template>

<style scoped>
.cb {
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 2px;
  border: none;
  background: transparent;
  color: var(--text-2);
  font: inherit;
  font-size: var(--fs-12);
  cursor: pointer;
  white-space: nowrap;
}
.box {
  width: 16px;
  height: 16px;
  border-radius: 4px;
  border: 1.5px solid var(--border-strong);
  background: var(--surface);
  display: grid;
  place-items: center;
  color: #fff;
  transition: background var(--dur-fast), border-color var(--dur-fast);
}
.cb:hover .box {
  border-color: var(--accent);
}
.cb.on .box {
  background: var(--accent);
  border-color: var(--accent);
}
.dash {
  width: 8px;
  height: 2px;
  border-radius: 1px;
  background: #fff;
}
</style>
