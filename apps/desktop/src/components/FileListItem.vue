<script setup lang="ts">
import { t } from "../i18n";
import { computed } from "vue";
import { formatSize } from "../composables/format";
import { assetUrl } from "../services/ipc";
import type { FileView } from "../types";
import CheckBox from "./CheckBox.vue";
import ConfidenceBadge from "./ConfidenceBadge.vue";
import Icon from "./Icon.vue";
import StatusPill from "./StatusPill.vue";

const props = defineProps<{ file: FileView; selected: boolean; checked: boolean }>();
defineEmits<{ toggle: [e: MouseEvent] }>();

const meta = computed(() => {
  const i = props.file.info;
  if (!i) return formatSize(props.file.size);
  if (i.type === "image") return `${i.format} · ${i.width}×${i.height} · ${formatSize(props.file.size)}`;
  return `${t("files.pdfPages", { n: i.pageCount })} · ${formatSize(props.file.size)}`;
});

const top = computed(() => props.file.candidates.reduce((m, c) => (c.confidence > (m?.confidence ?? -1) ? c : m), null as null | (typeof props.file.candidates)[number]));
</script>

<template>
  <div class="item" :class="{ selected, checked }" role="option" :aria-selected="selected">
    <CheckBox :checked="checked" @toggle="$emit('toggle', $event)" />
    <div class="thumb">
      <img v-if="file.thumb" :src="assetUrl(file.thumb)" alt="" loading="lazy" draggable="false" />
      <Icon v-else :name="file.kind === 'pdf' ? 'file-text' : 'image'" :size="20" />
      <span v-if="file.kind === 'pdf'" class="kind">PDF</span>
    </div>
    <div class="grow body">
      <div class="name truncate" :title="file.path">{{ file.name }}</div>
      <div v-if="file.status === 'failed' && file.error" class="meta err truncate" :title="`${file.error.message}\n${file.error.nextStep}`">{{ file.error.message }}</div>
      <div v-else class="meta truncate">{{ meta }}</div>
      <div class="row foot">
        <StatusPill :status="file.status" />
        <span v-if="file.candidates.length" class="count">{{ t("files.count", { n: file.candidates.length }) }}</span>
        <span class="grow" />
        <ConfidenceBadge v-if="top" :value="top.confidence" :decision="top.decision" compact />
      </div>
    </div>
  </div>
</template>

<style scoped>
.item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 8px 8px 4px;
  margin: 0 8px;
  border-radius: var(--r-10);
  cursor: pointer;
  transition: background var(--dur-fast);
}
.item:hover {
  background: var(--surface-2);
}
.item.selected {
  background: var(--accent-soft);
  box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 35%, transparent);
}
.thumb {
  position: relative;
  width: 52px;
  height: 52px;
  flex: none;
  border-radius: var(--r-8);
  overflow: hidden;
  display: grid;
  place-items: center;
  color: var(--text-3);
  background: var(--surface-3);
  border: 1px solid var(--border);
}
.thumb img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.kind {
  position: absolute;
  bottom: 3px;
  right: 3px;
  padding: 0 4px;
  border-radius: 4px;
  font-size: 9px;
  font-weight: 700;
  background: var(--danger);
  color: #fff;
}
.body {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.name {
  font-weight: 600;
  font-size: var(--fs-13);
}
.meta {
  font-size: var(--fs-11);
  color: var(--text-3);
}
.meta.err {
  color: var(--danger);
}
.foot {
  gap: 6px;
  margin-top: 3px;
}
.count {
  font-size: var(--fs-11);
  color: var(--text-2);
}
</style>
