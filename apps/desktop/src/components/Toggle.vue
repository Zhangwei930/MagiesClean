<script setup lang="ts">
defineProps<{ modelValue: boolean; disabled?: boolean }>();
defineEmits<{ "update:modelValue": [value: boolean] }>();
</script>

<template>
  <button
    type="button"
    role="switch"
    class="tog"
    :class="{ on: modelValue }"
    :aria-checked="modelValue"
    :disabled="disabled"
    @click="$emit('update:modelValue', !modelValue)"
  >
    <span class="knob" />
  </button>
</template>

<style scoped>
.tog {
  position: relative;
  width: 34px;
  height: 20px;
  border-radius: 20px;
  border: none;
  background: var(--border-strong);
  cursor: pointer;
  transition: background var(--dur) var(--ease);
  flex: none;
}
.tog.on {
  background: var(--accent);
}
.tog:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.knob {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
  transition: transform var(--dur) var(--ease);
}
.on .knob {
  transform: translateX(14px);
}
</style>
