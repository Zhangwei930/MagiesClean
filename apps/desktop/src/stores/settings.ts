import { defineStore } from "pinia";
import { ref } from "vue";
import { api } from "../services/ipc";
import type { AppInfo, AppSettings, ModelStatus, Preset } from "../types";
import { useUi } from "./ui";
import { useWorkspace } from "./workspace";
import { setLocale, t } from "../i18n";
import type { Language } from "../types";

export const useSettings = defineStore("settings", () => {
  const settings = ref<AppSettings | null>(null);
  const presets = ref<Preset[]>([]);
  const models = ref<ModelStatus[]>([]);
  const info = ref<AppInfo | null>(null);

  async function load() {
    const [s, p, m] = await Promise.all([api.getSettings(), api.getPresets(), api.getModelStatus()]);
    settings.value = s;
    presets.value = p;
    models.value = m;
    setLocale(s.language);
    useUi().applyTheme(s.theme);
    info.value = await api.appInfo().catch(() => null);
  }

  /** 修改设置并立即保存（后端校验并返回规范化后的值）。 */
  async function patch(mut: (s: AppSettings) => void) {
    if (!settings.value) return;
    const next: AppSettings = JSON.parse(JSON.stringify(settings.value));
    mut(next);
    settings.value = next;
    try {
      settings.value = await api.updateSettings(next);
      useUi().applyTheme(settings.value.theme);
    } catch (e) {
      useUi().toast({ tone: "danger", title: t("settings.saveFailed"), body: (e as Error).message });
      settings.value = await api.getSettings();
    }
  }

  /** 切换界面语言：后端消息（提示、错误、路由原因、预设名）随之以新语言重新输出。 */
  async function setLanguage(l: Language) {
    setLocale(l);
    await patch((x) => (x.language = l));
    const [p, m] = await Promise.all([api.getPresets(), api.getModelStatus()]);
    presets.value = p;
    models.value = m;
    await useWorkspace().load();
  }

  async function applyPreset(id: string) {
    settings.value = await api.applyPreset(id);
  }

  async function savePreset(p: Preset) {
    presets.value = await api.savePreset(p);
  }

  async function deletePreset(id: string) {
    presets.value = await api.deletePreset(id);
  }

  async function reloadModels() {
    models.value = await api.reloadModels();
  }

  async function refreshInfo() {
    info.value = await api.appInfo();
  }

  return { settings, presets, models, info, load, patch, setLanguage, applyPreset, savePreset, deletePreset, reloadModels, refreshInfo };
});
