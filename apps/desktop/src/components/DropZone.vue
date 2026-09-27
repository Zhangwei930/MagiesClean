<script setup lang="ts">
import { t } from "../i18n";
import { open } from "@tauri-apps/plugin-dialog";
import { useUi } from "../stores/ui";
import { useWorkspace } from "../stores/workspace";
import Icon from "./Icon.vue";

const ws = useWorkspace();
const ui = useUi();
const FORMATS = ["JPG", "PNG", "WebP", "BMP", "TIFF", "PDF"];

async function pickFiles() {
  const r = await open({
    multiple: true,
    filters: [{ name: t("drop.filterName"), extensions: ["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "pdf"] }],
  });
  if (r) await ws.importPaths(Array.isArray(r) ? r : [r]);
}

async function pickFolder() {
  const r = await open({ directory: true, multiple: true });
  if (r) await ws.importPaths(Array.isArray(r) ? r : [r]);
}
</script>

<template>
  <div class="drop" :class="{ active: ui.dragging }">
    <div class="art" aria-hidden="true">
      <div class="sheet s1" />
      <div class="sheet s2" />
      <div class="sheet s3">
        <Icon name="upload" :size="26" :stroke="2" />
      </div>
    </div>
    <h2>{{ t("drop.title") }}</h2>
    <p class="muted">{{ t("drop.subtitle") }}</p>
    <div class="row actions">
      <button class="btn brand lg" @click="pickFiles"><Icon name="image" :size="16" />{{ t("drop.chooseFiles") }}</button>
      <button class="btn lg" @click="pickFolder"><Icon name="folder" :size="16" />{{ t("drop.chooseFolder") }}</button>
    </div>
    <div class="formats">
      <span v-for="f in FORMATS" :key="f" class="chip">{{ f }}</span>
    </div>
    <div class="privacy"><Icon name="shield" :size="14" />{{ t("drop.privacy") }}</div>
  </div>
</template>

<style scoped>
.drop {
  position: relative;
  width: min(680px, 100%);
  padding: 52px 40px 34px;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  border-radius: 24px;
  border: 1.5px dashed var(--border-strong);
  background: radial-gradient(120% 90% at 50% 0%, var(--accent-soft) 0%, transparent 60%), var(--surface);
  box-shadow: var(--shadow);
  transition: border-color var(--dur), transform var(--dur) var(--ease), box-shadow var(--dur);
}
.drop.active {
  border-color: var(--accent);
  transform: scale(1.01);
  box-shadow: 0 0 0 6px var(--accent-soft), var(--shadow-lg);
}
.art {
  position: relative;
  width: 96px;
  height: 84px;
  margin-bottom: 22px;
}
.sheet {
  position: absolute;
  width: 64px;
  height: 76px;
  border-radius: 12px;
  border: 1px solid var(--border);
  background: var(--surface);
  box-shadow: var(--shadow);
}
.s1 {
  left: 4px;
  top: 8px;
  transform: rotate(-10deg);
  background: var(--surface-2);
}
.s2 {
  right: 4px;
  top: 8px;
  transform: rotate(9deg);
  background: var(--surface-2);
}
.s3 {
  left: 16px;
  top: 0;
  display: grid;
  place-items: center;
  color: #fff;
  border: none;
  background: var(--brand-gradient);
  box-shadow: 0 10px 24px rgba(84, 70, 240, 0.35);
}
h2 {
  margin: 0;
  font-size: var(--fs-20);
  font-weight: 700;
  letter-spacing: -0.01em;
}
p {
  margin: 8px 0 24px;
  max-width: 440px;
}
.actions {
  gap: 10px;
}
.formats {
  display: flex;
  gap: 6px;
  margin-top: 22px;
}
.chip {
  padding: 3px 9px;
  border-radius: var(--r-pill);
  background: var(--surface-3);
  color: var(--text-2);
  font-size: var(--fs-11);
  font-weight: 600;
  letter-spacing: 0.02em;
}
.privacy {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 26px;
  padding-top: 18px;
  border-top: 1px solid var(--border);
  width: 100%;
  justify-content: center;
  color: var(--success);
  font-size: var(--fs-12);
  font-weight: 500;
}
</style>
