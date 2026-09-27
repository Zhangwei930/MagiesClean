// 轻量 i18n：中英文成对维护（避免漏译），响应式切换，完全离线。默认英文。
import { ref } from "vue";
import { messages, type Messages } from "./messages";

export type Locale = "en" | "zh-CN";

export const locale = ref<Locale>("en");

type Leaf = readonly [string, string];

// 由字典结构推导出所有合法键，例如 "home.title"
type Paths<T, P extends string = ""> = {
  [K in keyof T & string]: T[K] extends Leaf ? `${P}${K}` : Paths<T[K], `${P}${K}.`>;
}[keyof T & string];
export type MessageKey = Paths<Messages>;

function lookup(key: string): Leaf | undefined {
  let node: unknown = messages;
  for (const part of key.split(".")) {
    if (node && typeof node === "object" && part in (node as object)) node = (node as Record<string, unknown>)[part];
    else return undefined;
  }
  return Array.isArray(node) ? (node as unknown as Leaf) : undefined;
}

/** 翻译：`t("sb.files", { n: 3 })`，占位符写作 `{n}`。 */
export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const leaf = lookup(key);
  if (!leaf) {
    if (import.meta.env.DEV) console.warn(`[i18n] missing key: ${key}`);
    return key;
  }
  let s = locale.value === "zh-CN" ? leaf[1] : leaf[0];
  if (params) for (const [k, v] of Object.entries(params)) s = s.split(`{${k}}`).join(String(v));
  return s;
}

/** 动态键（运行时拼接）版本；键不存在时返回键本身。 */
export function tx(key: string, params?: Record<string, string | number>): string {
  return t(key as MessageKey, params);
}

export function setLocale(l: Locale) {
  locale.value = l;
  document.documentElement.lang = l === "zh-CN" ? "zh-CN" : "en";
}

export const isZh = () => locale.value === "zh-CN";
