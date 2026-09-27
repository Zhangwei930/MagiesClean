<script setup lang="ts" generic="T extends string">
import Icon from "./Icon.vue";

defineProps<{ modelValue: T; options: { value: T; label: string; hint?: string; icon?: string }[]; size?: "sm" | "md" }>();
defineEmits<{ "update:modelValue": [value: T] }>();
</script>

<template>
  <div class="seg" :class="size ?? 'md'" role="radiogroup">
    <button
      v-for="o in options"
      :key="o.value"
      type="button"
      role="radio"
      :aria-checked="modelValue === o.value"
      :class="{ on: modelValue === o.value }"
      :title="o.hint"
      @click="$emit('update:modelValue', o.value)"
    >
      <Icon v-if="o.icon" :name="o.icon" :size="14" />
      {{ o.label }}
    </button>
  </div>
</template>

<style scoped>
.seg {
  flex: none;
  display: inline-flex;
  padding: 2px;
  gap: 2px;
  border-radius: var(--r-8);
  background: var(--surface-3);
  border: 1px solid var(--border);
}
.seg button {
  flex: none;
  display: inline-flex;
  align-items: center;
  white-space: nowrap;
  gap: 5px;
  height: 26px;
  padding: 0 12px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-2);
  font-size: var(--fs-12);
  font-weight: 500;
  cursor: pointer;
  transition: background var(--dur-fast), color var(--dur-fast), box-shadow var(--dur-fast);
}
.seg.sm button {
  height: 22px;
  padding: 0 9px;
}
.seg button:hover {
  color: var(--text);
}
.seg button.on {
  background: var(--surface);
  color: var(--text);
  box-shadow: var(--shadow-sm);
}
</style>
