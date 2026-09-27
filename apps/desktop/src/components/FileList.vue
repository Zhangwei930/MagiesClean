<script setup lang="ts">
import { t, tx } from "../i18n";
import { open } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, ref, watch } from "vue";
import { FILTERS, useWorkspace } from "../stores/workspace";
import { useTask } from "../stores/task";
import CheckBox from "./CheckBox.vue";
import FileListItem from "./FileListItem.vue";
import Icon from "./Icon.vue";

const ws = useWorkspace();
const task = useTask();
const scroller = ref<HTMLElement | null>(null);
const menu = ref(false);

// 简单虚拟列表：数千个文件时只渲染可见区域
const ROW = 76;
const scrollTop = ref(0);
const viewH = ref(600);
const visible = computed(() => {
  const start = Math.max(0, Math.floor(scrollTop.value / ROW) - 6);
  const end = Math.min(ws.list.length, Math.ceil((scrollTop.value + viewH.value) / ROW) + 6);
  return { start, items: ws.list.slice(start, end) };
});

function onScroll(e: Event) {
  const el = e.target as HTMLElement;
  scrollTop.value = el.scrollTop;
  viewH.value = el.clientHeight;
}

watch(
  () => ws.selectedId,
  async (id) => {
    await nextTick();
    const i = ws.list.findIndex((f) => f.id === id);
    const el = scroller.value;
    if (i < 0 || !el) return;
    const y = i * ROW;
    if (y < el.scrollTop) el.scrollTop = y;
    else if (y + ROW > el.scrollTop + el.clientHeight) el.scrollTop = y + ROW - el.clientHeight;
  },
);

// 勾选：全选作用于当前筛选出的列表；按住 Shift 点击可连续勾选一段
const listIds = computed(() => ws.list.map((f) => f.id));
const checkedInList = computed(() => listIds.value.filter((id) => ws.checked.has(id)).length);
const allChecked = computed(() => listIds.value.length > 0 && checkedInList.value === listIds.value.length);
let anchor: string | null = null;

function toggleAll() {
  ws.setChecked(listIds.value, !allChecked.value);
}

function toggleOne(id: string, e: MouseEvent) {
  const on = !ws.checked.has(id);
  if (e.shiftKey && anchor) {
    const a = listIds.value.indexOf(anchor);
    const b = listIds.value.indexOf(id);
    if (a >= 0 && b >= 0) {
      ws.setChecked(listIds.value.slice(Math.min(a, b), Math.max(a, b) + 1), on);
      anchor = id;
      return;
    }
  }
  ws.toggleChecked(id, on);
  anchor = id;
}

function removeFiles() {
  const ids = ws.checkedIds.length ? ws.checkedIds : ws.selectedId ? [ws.selectedId] : [];
  if (ids.length) ws.remove(ids);
  menu.value = false;
}

async function addMore() {
  const r = await open({ multiple: true, filters: [{ name: t("drop.filterName"), extensions: ["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "pdf"] }] });
  if (r) await ws.importPaths(Array.isArray(r) ? r : [r]);
}
async function addFolder() {
  const r = await open({ directory: true, multiple: true });
  if (r) await ws.importPaths(Array.isArray(r) ? r : [r]);
}
</script>

<template>
  <aside class="files">
    <div class="head">
      <div class="row">
        <span class="section-title">{{ t("files.title") }}</span>
        <span class="total">{{ ws.order.length }}</span>
        <span class="grow" />
        <button class="btn ghost sm icon" :title="t('files.addFiles')" @click="addMore"><Icon name="image" :size="15" /></button>
        <button class="btn ghost sm icon" :title="t('files.addFolder')" @click="addFolder"><Icon name="folder" :size="15" /></button>
        <div class="menu-wrap">
          <button class="btn ghost sm icon" :title="t('files.more')" @click="menu = !menu"><Icon name="dots" :size="15" /></button>
          <div v-if="menu" class="menu card" @mouseleave="menu = false">
            <button :disabled="task.busy" @click="ws.rescan(); menu = false"><Icon name="refresh" :size="14" />{{ t("files.rescanAll") }}</button>
            <button :disabled="task.busy || (!ws.selectedId && !ws.checkedIds.length)" @click="removeFiles">
              <Icon name="x" :size="14" />{{ ws.checkedIds.length ? t("files.removeChecked", { n: ws.checkedIds.length }) : t("files.remove") }}
            </button>
            <button class="danger" :disabled="task.busy" @click="ws.clear(); menu = false"><Icon name="trash" :size="14" />{{ t("files.clear") }}</button>
          </div>
        </div>
      </div>
      <label class="search">
        <Icon name="search" :size="14" />
        <input v-model="ws.search" :placeholder="t('files.search')" spellcheck="false" />
      </label>
      <div v-if="ws.order.length" class="row checkbar">
        <CheckBox :checked="allChecked" :indeterminate="checkedInList > 0 && !allChecked" :label="t('files.selectAll')" @toggle="toggleAll" />
        <span class="grow" />
        <template v-if="ws.checkedIds.length">
          <span class="picked">{{ t("files.checked", { n: ws.checkedIds.length }) }}</span>
          <button class="link" @click="ws.clearChecked()">{{ t("files.uncheck") }}</button>
        </template>
      </div>
      <div class="filters">
        <button v-for="f in FILTERS" v-show="f.key === 'all' || ws.counts[f.key]" :key="f.key" :class="{ on: ws.filter === f.key, warn: f.key === 'needs_review' && ws.counts[f.key] }" @click="ws.filter = f.key">
          {{ tx(`filter.${f.key}`) }}<span>{{ ws.counts[f.key] }}</span>
        </button>
      </div>
    </div>
    <div ref="scroller" class="scroll" role="listbox" @scroll="onScroll">
      <div :style="{ height: `${ws.list.length * ROW}px`, position: 'relative' }">
        <div :style="{ transform: `translateY(${visible.start * ROW}px)` }">
          <FileListItem
            v-for="f in visible.items"
            :key="f.id"
            :file="f"
            :selected="f.id === ws.selectedId"
            :checked="ws.checked.has(f.id)"
            @toggle="toggleOne(f.id, $event)"
            :style="{ height: `${ROW}px` }"
            @click="ws.select(f.id)"
          />
        </div>
      </div>
      <div v-if="!ws.list.length" class="empty subtle">{{ t("files.empty") }}</div>
    </div>
  </aside>
</template>

<style scoped>
.checkbar {
  gap: 8px;
  margin: -2px 0 -4px;
  font-size: var(--fs-12);
}
.picked {
  color: var(--accent);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.link {
  border: none;
  background: none;
  padding: 0;
  color: var(--text-2);
  font: inherit;
  cursor: pointer;
}
.link:hover {
  color: var(--text);
  text-decoration: underline;
}
.files {
  width: var(--sidebar-w);
  flex: none;
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--border);
  background: var(--bg-elev);
  min-height: 0;
}
.head {
  padding: 12px 14px 8px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  border-bottom: 1px solid var(--border);
}
.total {
  font-size: var(--fs-11);
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
.search {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 30px;
  padding: 0 10px;
  border-radius: var(--r-8);
  background: var(--surface-3);
  color: var(--text-3);
}
.search input {
  flex: 1;
  border: none;
  outline: none;
  background: transparent;
  color: var(--text);
  font: inherit;
}
.filters {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}
.filters button {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  height: 24px;
  padding: 0 9px;
  border-radius: var(--r-pill);
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text-2);
  font-size: var(--fs-12);
  cursor: pointer;
}
.filters button span {
  font-size: var(--fs-11);
  color: var(--text-3);
  font-variant-numeric: tabular-nums;
}
.filters button.on {
  border-color: var(--accent);
  background: var(--accent-soft);
  color: var(--accent-text);
}
.filters button.warn:not(.on) {
  color: var(--warning);
}
.scroll {
  flex: 1;
  overflow-y: auto;
  padding: 6px 0 12px;
}
.empty {
  text-align: center;
  padding: 24px;
}
.menu-wrap {
  position: relative;
}
.menu {
  position: absolute;
  right: 0;
  top: 30px;
  z-index: 20;
  width: 170px;
  padding: 4px;
  display: flex;
  flex-direction: column;
  box-shadow: var(--shadow-lg);
}
.menu button {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 30px;
  padding: 0 10px;
  border: none;
  border-radius: 6px;
  background: transparent;
  text-align: left;
  cursor: pointer;
}
.menu button:hover:not(:disabled) {
  background: var(--surface-2);
}
.menu button:disabled {
  opacity: 0.4;
}
.menu button.danger {
  color: var(--danger);
}
</style>
