<script setup lang="ts">
import { t } from "../i18n";
import { useUi } from "../stores/ui";
import Icon from "./Icon.vue";

const ui = useUi();
const ICON = { info: "info", success: "check-circle", warning: "alert", danger: "x-circle" } as const;
</script>

<template>
  <div class="toasts" aria-live="polite">
    <TransitionGroup name="pop">
      <div v-for="item in ui.toasts" :key="item.id" class="toast card" :class="item.tone">
        <Icon :name="ICON[item.tone]" :size="18" class="ic" />
        <div class="grow">
          <div class="title">{{ item.title }}</div>
          <div v-if="item.body" class="body">{{ item.body }}</div>
          <button v-if="item.action" class="link" @click="item.action.run(); ui.dismiss(item.id)">{{ item.action.label }}</button>
        </div>
        <button class="btn ghost sm icon" :aria-label="t('common.close')" @click="ui.dismiss(item.id)"><Icon name="x" :size="14" /></button>
      </div>
    </TransitionGroup>
  </div>
</template>

<style scoped>
.toasts {
  position: fixed;
  right: 20px;
  bottom: calc(var(--statusbar-h) + 16px);
  display: flex;
  flex-direction: column;
  gap: 10px;
  z-index: 200;
  width: 360px;
  pointer-events: none;
}
.toast {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  padding: 12px 10px 12px 14px;
  box-shadow: var(--shadow-lg);
  pointer-events: auto;
  border-radius: var(--r-12);
}
.ic {
  margin-top: 1px;
}
.info .ic {
  color: var(--info);
}
.success .ic {
  color: var(--success);
}
.warning .ic {
  color: var(--warning);
}
.danger .ic {
  color: var(--danger);
}
.title {
  font-weight: 600;
}
.body {
  color: var(--text-2);
  font-size: var(--fs-12);
  margin-top: 2px;
  word-break: break-all;
}
.link {
  margin-top: 6px;
  padding: 0;
  border: none;
  background: none;
  color: var(--accent-text);
  font-weight: 600;
  font-size: var(--fs-12);
  cursor: pointer;
}
</style>
