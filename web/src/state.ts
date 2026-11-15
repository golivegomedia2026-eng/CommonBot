// 各页面共用的状态：服务信息、推理服务及其检测结果、当前选用的模型。
import { reactive } from "vue";

import { api, type Folder, type Health, kb, type Provider, type UpdateStatus } from "./api";
import { errorText } from "./i18n";

export interface Check {
  state: "checking" | "ok" | "error";
  models: string[];
  latency: number;
  error: string;
}

export const state = reactive({
  health: null as Health | null,
  providers: [] as Provider[],
  providersLoaded: false,
  checks: {} as Record<string, Check>,
  selection: { providerId: "", model: "" },
  update: null as UpdateStatus | null,
  folders: [] as Folder[],
  foldersLoaded: false,
});

export async function loadFolders() {
  state.folders = (await kb.folders()).folders;
  state.foldersLoaded = true;
}

/** 文件夹的完整路径，如「招标文件 / 2026 政务云」。 */
export function folderPath(id: string): string {
  const parts: string[] = [];
  let cur = state.folders.find((f) => f.id === id);
  while (cur && parts.length < 32) {
    parts.unshift(cur.name);
    const parent = cur.parent_id;
    cur = parent ? state.folders.find((f) => f.id === parent) : undefined;
  }
  return parts.join(" / ");
}

/** 按树的顺序排好的文件夹，带层级。 */
export function folderTree(): { folder: Folder; depth: number }[] {
  const out: { folder: Folder; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const f of state.folders.filter((x) => x.parent_id === parent)) {
      out.push({ folder: f, depth });
      if (depth < 32) walk(f.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

/** 一个文件夹连同所有子文件夹里的文档总数。 */
export function folderTotal(id: string): number {
  const self = state.folders.find((f) => f.id === id);
  return (self?.documents ?? 0) + state.folders.filter((f) => f.parent_id === id).reduce((n, f) => n + folderTotal(f.id), 0);
}

const KEY = "commonbot_model";
try {
  const saved = JSON.parse(localStorage.getItem(KEY) ?? "{}");
  if (typeof saved.providerId === "string" && typeof saved.model === "string") state.selection = saved;
} catch {
  /* 没有保存过 */
}

export function select(providerId: string, model: string) {
  state.selection = { providerId, model };
  try {
    localStorage.setItem(KEY, JSON.stringify(state.selection));
  } catch {
    /* 存不了也不影响本次使用 */
  }
}

/** 可选的「服务 + 模型」组合，只含检测通过的服务。 */
export function modelOptions(): { providerId: string; model: string; label: string }[] {
  const many = state.providers.length > 1;
  return state.providers.flatMap((p) =>
    (state.checks[p.id]?.models ?? []).map((model) => ({
      providerId: p.id,
      model,
      label: many ? `${p.name} · ${model}` : model,
    })),
  );
}

/** 当前选择失效时（服务被删、模型没了），换成一个可用的。 */
export function ensureSelection() {
  const options = modelOptions();
  const { providerId, model } = state.selection;
  if (options.some((o) => o.providerId === providerId && o.model === model)) return;
  for (const p of state.providers) {
    const models = state.checks[p.id]?.models ?? [];
    if (!models.length) continue;
    // 推理服务会把向量化、重排模型也列出来，它们不能用来对话：自动挑选时避开
    const chat = models.filter((m) => !/embed|bge|rerank|minilm|(^|[-_/])e5[-_]/i.test(m));
    const pick = p.default_model && models.includes(p.default_model) ? p.default_model : (chat[0] ?? models[0]);
    return select(p.id, pick);
  }
  if (providerId || model) select("", "");
}

export async function checkProvider(id: string) {
  state.checks[id] = { state: "checking", models: [], latency: 0, error: "" };
  try {
    const r = await api.checkProvider(id);
    state.checks[id] = { state: "ok", models: r.models, latency: r.latency_ms, error: "" };
  } catch (e) {
    state.checks[id] = { state: "error", models: [], latency: 0, error: errorText(e) };
  }
  ensureSelection();
}

export async function loadProviders(check = true) {
  state.providers = await api.providers();
  state.providersLoaded = true;
  for (const id of Object.keys(state.checks)) {
    if (!state.providers.some((p) => p.id === id)) delete state.checks[id];
  }
  if (check) await Promise.all(state.providers.map((p) => checkProvider(p.id)));
  else ensureSelection();
}
