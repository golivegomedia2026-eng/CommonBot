import { createI18n } from "vue-i18n";

import { ApiError } from "../api";
import en from "./en";
import ja from "./ja";
import zhCN from "./zh-CN";
import zhTW from "./zh-TW";

export const LANGS = [
  { key: "zh-CN", label: "简体中文" },
  { key: "zh-TW", label: "繁體中文" },
  { key: "en", label: "English" },
  { key: "ja", label: "日本語" },
] as const;
export type Lang = (typeof LANGS)[number]["key"];

const KEY = "commonbot_lang";

/** 按浏览器语言选一个最接近的。 */
export function detect(preferred: readonly string[]): Lang {
  for (const raw of preferred) {
    const l = raw.toLowerCase();
    if (l.startsWith("zh")) return /tw|hk|mo|hant/.test(l) ? "zh-TW" : "zh-CN";
    if (l.startsWith("ja")) return "ja";
    if (l.startsWith("en")) return "en";
  }
  return "zh-CN";
}

function initial(): Lang {
  try {
    const saved = localStorage.getItem(KEY);
    if (LANGS.some((l) => l.key === saved)) return saved as Lang;
  } catch {
    /* 读不到就按浏览器语言 */
  }
  return detect(navigator.languages ?? [navigator.language]);
}

export const i18n = createI18n({
  legacy: false,
  locale: initial(),
  fallbackLocale: "zh-CN",
  messages: { "zh-CN": zhCN, "zh-TW": zhTW, en, ja },
});

export function setLang(lang: Lang) {
  i18n.global.locale.value = lang;
  document.documentElement.lang = lang;
  try {
    localStorage.setItem(KEY, lang);
  } catch {
    /* 存不了也不影响本次使用 */
  }
}
document.documentElement.lang = i18n.global.locale.value;

/** 把错误变成当前语言的一句话。 */
export function errorText(e: unknown): string {
  const t = i18n.global.t;
  if (e instanceof ApiError) {
    const key = `errors.${e.code}`;
    return i18n.global.te(key) ? t(key, { detail: e.detail }) : t("errors.http", { detail: e.detail });
  }
  return t("errors.http", { detail: String(e) });
}

/** 已保存消息上的错误代码对应的说明。 */
export function errorCodeText(code: string, detail = ""): string {
  const key = `errors.${code}`;
  return i18n.global.te(key) ? i18n.global.t(key, { detail }) : i18n.global.t("errors.http", { detail: code });
}
