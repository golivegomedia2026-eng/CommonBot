/* 鉴色·色境切换（复制自鉴见能力平台）：八套整体配色，可固定一套，也可跟随系统（浅色、深色各选一套）。
 * 在 <head> 里同步执行，首屏不闪。选择只存在本机浏览器（localStorage），读写失败时用默认值。 */
(() => {
  const KEY = "commonbot_realm";
  // swatch：底、框、主、点，用于配色面板里的色卡
  const REALMS = [
    { key: "xuanzhi", name: "宣纸", dark: false, desc: "汉白玉底 · 苍筤", swatch: ["#F8F4ED", "#F5F0E6", "#2E5D4A", "#806332"] },
    { key: "tianqing", name: "天青", dark: false, desc: "汝窑冷釉 · 铁足暖点", swatch: ["#E8F4F8", "#2A475F", "#1661AB", "#C1651A"] },
    { key: "qiuxiang", name: "秋香", dark: false, desc: "古籍纸色 · 朱印", swatch: ["#FBF2E3", "#FBECDE", "#8E443D", "#D92121"] },
    { key: "zhuyue", name: "竹月", dark: false, desc: "竹林月色 · 藕丝", swatch: ["#EEF7F2", "#223E36", "#0D5E3A", "#D8B0C8"] },
    { key: "ouhe", name: "藕荷", dark: false, desc: "杏粉绛紫 · 青矾", swatch: ["#F5E6E0", "#4B1E2F", "#8E354A", "#2C9678"] },
    { key: "daiye", name: "黛夜", dark: true, desc: "夜读青灯 · 缃绮", swatch: ["#0F1A20", "#101F30", "#8ABCD1", "#F8C471"] },
    { key: "mozhu", name: "墨竹", dark: true, desc: "夜竹 · 樱粉", swatch: ["#141E1B", "#15231B", "#7BC092", "#F7B7C5"] },
    { key: "jiangye", name: "绛夜", dark: true, desc: "深绛如酒 · 天水碧", swatch: ["#2B1216", "#2D0C13", "#F7B7C5", "#12A182"] },
  ];
  const byKey = (k, dark) => REALMS.find((r) => r.key === k && (dark === undefined || r.dark === dark));
  const media = window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  const listeners = [];
  let state = { realm: "xuanzhi", follow: false, light: "xuanzhi", dark: "daiye" };
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) || "{}");
    if (byKey(saved.realm)) state.realm = saved.realm;
    if (typeof saved.follow === "boolean") state.follow = saved.follow;
    if (byKey(saved.light, false)) state.light = saved.light;
    if (byKey(saved.dark, true)) state.dark = saved.dark;
  } catch {}

  const current = () => (state.follow ? (media && media.matches ? state.dark : state.light) : state.realm);
  function apply() {
    const r = byKey(current()) || REALMS[0];
    document.documentElement.dataset.realm = r.key;
    document.documentElement.dataset.theme = r.dark ? "dark" : "light";
    listeners.forEach((fn) => fn(get()));
  }
  function get() { return { ...state, current: current() }; }
  function set(next) {
    state = { ...state, ...next };
    // 固定选择一套时，同时记为对应明暗的跟随默认，下次打开「跟随系统」更顺手
    if (next.realm) {
      const r = byKey(next.realm);
      if (r) state[r.dark ? "dark" : "light"] = r.key;
    }
    try { localStorage.setItem(KEY, JSON.stringify(state)); } catch {}
    apply();
  }
  if (media && media.addEventListener) media.addEventListener("change", () => { if (state.follow) apply(); });
  apply();

  window.JJTheme = { get, set, REALMS, onChange: (fn) => listeners.push(fn) };
})();
