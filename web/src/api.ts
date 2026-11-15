// 与服务通信。所有错误都整理成 ApiError，界面按 code 显示对应语言的说明。

export interface Provider {
  id: string;
  name: string;
  kind: "local" | "cloud";
  base_url: string;
  has_key: boolean;
  default_model: string | null;
}
export interface ProviderInput {
  name: string;
  kind: "local" | "cloud";
  base_url: string;
  /** 不传：保持原样；空字符串：清除 */
  api_key?: string;
  default_model?: string | null;
}
export interface Agent {
  id: string;
  name: string;
  description: string;
  duty: string;
  out_of_scope: string;
  instructions: string;
  opening: string;
  examples: string[];
  provider_id: string | null;
  model: string | null;
  folders: string[];
  web: "off" | "fallback" | "always";
  allow_cloud: boolean;
  public_token: string | null;
  api_key: string | null;
  updated_at: number;
  system_prompt?: string;
}
export interface TestCase {
  question: string;
  expect: string;
  answer: string | null;
  pass: boolean | null;
  reason: string;
}
export interface Draft extends AgentInput {
  manual: string[];
}
export interface Build {
  id: string;
  conversation_id: string;
  draft: Draft;
  tests: TestCase[];
  agent_id: string | null;
  updated_at: number;
}
export interface PublicInfo {
  name: string;
  description: string;
  opening: string;
  examples: string[];
}
export interface AgentSummary extends Agent {
  conversations: number;
  last_used_at: number | null;
}
export type AgentInput = Pick<Agent, "name" | "description" | "duty" | "out_of_scope" | "instructions" | "opening" | "examples" | "provider_id" | "model" | "folders" | "web" | "allow_cloud">;

export type Locator = { kind: "none" } | { kind: "page" | "slide"; value: number } | { kind: "sheet" | "heading"; value: string };
export type Source =
  | { n: number; kind: "kb"; document_id: string; document: string; folder: string; locator: Locator; text: string }
  | { n: number; kind: "file"; document: string; text: string }
  | { n: number; kind: "web"; title: string; url: string; text: string };
export interface Attachment {
  id: string;
  message_id: string | null;
  name: string;
  size: number;
  chars: number;
  skipped_pages: number;
}
export interface WebSettings {
  kind: "builtin" | "none" | "searxng" | "tavily" | "bocha";
  base_url: string;
  has_key: boolean;
  fetch_pages: boolean;
}
export interface Folder {
  id: string;
  parent_id: string | null;
  name: string;
  documents: number;
}
export interface KbStats {
  documents: number;
  chunks: number;
  queued: number;
  failed: number;
  ready: number;
  keyword_only: number;
}
export interface KbDocument {
  id: string;
  folder_id: string;
  name: string;
  ext: string;
  size: number;
  status: "queued" | "parsing" | "ocr" | "embedding" | "ready" | "failed" | "paused";
  done: number;
  total: number;
  error_code: string | null;
  error_detail: string | null;
  pages: number | null;
  chunks: number;
  ocr_pages: number;
  ocr_skipped: number;
  embed_model: string | null;
}
export interface Hit {
  chunk_id: number;
  document_id: string;
  document: string;
  folder: string;
  locator: Locator;
  text: string;
}
export interface ModelRef {
  provider_id: string;
  model: string;
}
export interface KbSettings {
  embed: ModelRef | null;
  ocr: ModelRef | null;
  pdf_engine: boolean;
}

export interface Conversation {
  id: string;
  agent_id: string;
  scope: string[] | null;
  web: boolean | null;
  title: string;
  provider_id: string | null;
  model: string | null;
  created_at: number;
  updated_at: number;
}
export interface Message {
  id: string;
  conversation_id: string;
  role: "user" | "assistant";
  content: string;
  reasoning: string;
  status: "ok" | "stopped" | "error";
  error_code: string | null;
  sources: Source[];
  model: string | null;
  cloud: boolean;
  created_at: number;
}
export interface UpdateStatus {
  state: "disabled" | "unknown" | "up_to_date" | "optional" | "required";
  current: string;
  latest: string | null;
  minimum: string | null;
  notes: string;
  url: string;
  auto: boolean;
  failed: boolean;
}
export interface Health {
  name: string;
  version: string;
  local: boolean;
  authed: boolean;
  lan: boolean;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    public detail: string,
  ) {
    super(`${code}: ${detail}`);
  }
}

/** 没有访问凭据时调用，由入口处设置为跳转到访问码页面。 */
export const hooks = { onAccessRequired: () => {} };

async function toError(resp: Response): Promise<ApiError> {
  let code = "http";
  let detail = resp.statusText;
  try {
    const body = await resp.json();
    code = body.code ?? code;
    detail = body.message ?? detail;
  } catch {
    /* 不是 JSON，保留状态文字 */
  }
  if (code === "access_required") hooks.onAccessRequired();
  return new ApiError(resp.status, code, detail);
}

async function call<T>(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  let resp: Response;
  try {
    resp = await fetch(`/api${path}`, {
      method,
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
    });
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new ApiError(0, "offline", String(e));
  }
  if (!resp.ok) throw await toError(resp);
  return resp.status === 204 ? (undefined as T) : resp.json();
}

/** 本机算力：随包的推理运行时的状态，以及算力包里各模型的用场。 */
export interface ComputeInfo {
  state: "none" | "external" | "starting" | "running" | "failed";
  detail?: string;
  dir?: string;
  models?: { chat: string; extra: string[]; embed: string; vision: string };
}

export const api = {
  health: () => call<Health>("GET", "/health"),
  update: () => call<UpdateStatus>("GET", "/update"),
  checkUpdate: () => call<UpdateStatus>("POST", "/update/check"),
  setAutoUpdate: (enabled: boolean) => call<UpdateStatus>("PUT", "/update/auto", { enabled }),
  system: () => call<{ version: string; lan: boolean; lan_fixed: boolean; port: number; addresses: string[]; compute: ComputeInfo }>("GET", "/system"),
  setLan: (enabled: boolean) => call<{ lan: boolean }>("PUT", "/system/lan", { enabled }),
  enterAccessCode: (code: string) => call<{ ok: boolean }>("POST", "/access", { code }),
  accessCode: () => call<{ code: string; listen: string }>("GET", "/access-code"),
  resetAccessCode: () => call<{ code: string; listen: string }>("POST", "/access-code"),

  providers: () => call<Provider[]>("GET", "/providers"),
  createProvider: (input: ProviderInput) => call<Provider>("POST", "/providers", input),
  updateProvider: (id: string, input: ProviderInput) => call<Provider>("PUT", `/providers/${id}`, input),
  deleteProvider: (id: string) => call<void>("DELETE", `/providers/${id}`),
  checkProvider: (id: string) => call<{ models: string[]; latency_ms: number }>("POST", `/providers/${id}/check`),

  agents: () => call<AgentSummary[]>("GET", "/agents"),
  agent: (id: string) => call<Agent>("GET", `/agents/${id}`),
  createAgent: (input: AgentInput) => call<Agent>("POST", "/agents", input),
  updateAgent: (id: string, input: AgentInput) => call<Agent>("PUT", `/agents/${id}`, input),
  deleteAgent: (id: string) => call<void>("DELETE", `/agents/${id}`),
  publishAgent: (id: string) => call<Agent>("POST", `/agents/${id}/publish`),
  unpublishAgent: (id: string) => call<Agent>("DELETE", `/agents/${id}/publish`),
  rotateAgentKey: (id: string) => call<Agent>("POST", `/agents/${id}/rotate-key`),
  builds: () => call<Build[]>("GET", "/builds"),
  createBuild: (opening: string) => call<Build>("POST", "/builds", { opening }),
  build: (id: string) => call<{ build: Build; messages: Message[]; attachments: Attachment[] }>("GET", `/builds/${id}`),
  deleteBuild: (id: string) => call<void>("DELETE", `/builds/${id}`),
  saveDraft: (id: string, input: AgentInput) => call<Build>("PUT", `/builds/${id}/draft`, input),
  publishBuild: (id: string, model: { provider_id: string; model: string }) => call<Agent>("POST", `/builds/${id}/publish`, model),
  publicInfo: (token: string) => call<PublicInfo>("GET", `/public/${token}`),
  publicNewConversation: (token: string) => call<{ id: string }>("POST", `/public/${token}/conversations`),
  publicConversation: (token: string, id: string) => call<{ messages: Message[] }>("GET", `/public/${token}/conversations/${id}`),
  previewPrompt: (input: AgentInput, signal?: AbortSignal) => call<{ system_prompt: string }>("POST", "/agents/prompt-preview", input, signal),

  conversations: (agentId: string) => call<Conversation[]>("GET", `/conversations?agent_id=${encodeURIComponent(agentId)}`),
  createConversation: (agentId: string) => call<Conversation>("POST", "/conversations", { agent_id: agentId }),
  conversation: (id: string) => call<{ conversation: Conversation; messages: Message[]; attachments: Attachment[] }>("GET", `/conversations/${id}`),
  removeAttachment: (conversationId: string, id: string) => call<void>("DELETE", `/conversations/${conversationId}/attachments/${id}`),
  /** 给对话上传文件；读不了的在 rejected 里逐个说明。 */
  async attach(conversationId: string, files: File[]): Promise<{ added: Attachment[]; rejected: { name: string; code: string }[] }> {
    const form = new FormData();
    files.forEach((f) => form.append("files", f, f.name));
    let resp: Response;
    try {
      resp = await fetch(`/api/conversations/${conversationId}/attachments`, { method: "POST", body: form });
    } catch (e) {
      throw new ApiError(0, "upload_failed", String(e));
    }
    if (!resp.ok) throw await toError(resp);
    return resp.json();
  },
  webSettings: () => call<WebSettings>("GET", "/settings/web"),
  saveWebSettings: (s: { kind: string; base_url: string; api_key?: string; fetch_pages: boolean }) => call<WebSettings>("PUT", "/settings/web", s),
  testWeb: (query: string) => call<{ results: { title: string; url: string; text: string }[] }>("POST", "/settings/web/test", { query }),
  renameConversation: (id: string, title: string) => call<Conversation>("PATCH", `/conversations/${id}`, { title }),
  deleteConversation: (id: string) => call<void>("DELETE", `/conversations/${id}`),
};

export const kb = {
  folders: () => call<{ folders: Folder[]; stats: KbStats }>("GET", "/kb/folders"),
  createFolder: (name: string, parent_id: string | null) => call<{ id: string }>("POST", "/kb/folders", { name, parent_id }),
  renameFolder: (id: string, name: string) => call<void>("PATCH", `/kb/folders/${id}`, { name }),
  deleteFolder: (id: string) => call<void>("DELETE", `/kb/folders/${id}`),
  documents: (folderId: string, recursive: boolean) =>
    call<{ documents: KbDocument[]; stats: KbStats }>("GET", `/kb/folders/${folderId}/documents?recursive=${recursive}`),
  deleteDocument: (id: string) => call<void>("DELETE", `/kb/documents/${id}`),
  documentAction: (id: string, action: "retry" | "pause" | "resume") => call<KbDocument>("POST", `/kb/documents/${id}/${action}`),
  chunks: (id: string, offset: number) =>
    call<{ chunks: { seq: number; locator: Locator; text: string }[] }>("GET", `/kb/documents/${id}/chunks?offset=${offset}&limit=50`),
  search: (query: string, folder_ids: string[]) =>
    call<{ hits: Hit[]; semantic: boolean; note: string | null }>("POST", "/kb/search", { query, folder_ids }),
  settings: () => call<KbSettings>("GET", "/kb/settings"),
  saveSettings: (s: { embed: ModelRef | null; ocr: ModelRef | null }) =>
    call<KbSettings>("PUT", "/kb/settings", {
      embed_provider_id: s.embed?.provider_id ?? "",
      embed_model: s.embed?.model ?? "",
      ocr_provider_id: s.ocr?.provider_id ?? "",
      ocr_model: s.ocr?.model ?? "",
    }),
  /** 上传若干文件。不支持或过大的文件在 rejected 里逐个说明。 */
  async upload(folderId: string, files: File[]): Promise<{ added: KbDocument[]; rejected: { name: string; code: string }[] }> {
    const form = new FormData();
    files.forEach((f) => form.append("files", f, f.name));
    let resp: Response;
    try {
      resp = await fetch(`/api/kb/folders/${folderId}/documents`, { method: "POST", body: form });
    } catch (e) {
      throw new ApiError(0, "upload_failed", String(e));
    }
    if (!resp.ok) throw await toError(resp);
    return resp.json();
  },
};

export type StreamEvent =
  | { type: "user"; message: Message; conversation: Conversation; attachments: Attachment[] }
  | { type: "fallback"; model: string; provider: string }
  | { type: "sources"; sources: Source[]; semantic: boolean; note: string | null; web_note: string | null }
  | { type: "delta"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "draft"; draft: Draft }
  | { type: "draft_error"; code: string }
  | { type: "tests"; tests: TestCase[] }
  | { type: "test"; index: number; test: TestCase }
  | { type: "done"; message: Message; tests?: TestCase[] }
  | { type: "error"; code: string; detail: string; message?: Message };

/** 把事件流的文本拆成事件。数据块可能在任意位置断开，没收完的部分留在返回的 rest 里。 */
export function parseEvents(buffer: string): { events: StreamEvent[]; rest: string } {
  const events: StreamEvent[] = [];
  const blocks = buffer.split(/\r?\n\r?\n/);
  const rest = blocks.pop() ?? "";
  for (const block of blocks) {
    let type = "";
    const data: string[] = [];
    for (const line of block.split(/\r?\n/)) {
      if (line.startsWith("event:")) type = line.slice(6).trim();
      else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
    }
    if (!type || !data.length) continue; // 心跳等
    try {
      events.push({ type, ...JSON.parse(data.join("\n")) } as StreamEvent);
    } catch {
      /* 丢弃无法解析的事件 */
    }
  }
  return { events, rest };
}

/** 发一个请求并逐个接收事件流里的事件。中止 signal 即停止。 */
export async function postStream(path: string, body: unknown, onEvent: (e: StreamEvent) => void, signal?: AbortSignal): Promise<void> {
  let resp: Response;
  try {
    resp = await fetch(`/api${path}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal,
    });
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new ApiError(0, "offline", String(e));
  }
  if (!resp.ok || !resp.body) throw await toError(resp);
  const reader = resp.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const parsed = parseEvents(buffer);
    buffer = parsed.rest;
    parsed.events.forEach(onEvent);
  }
}

/** 发一句话并逐段接收回答。中止 signal 即停止生成，已生成的部分由服务保存。 */
export function sendMessage(
  conversationId: string,
  body: { content: string; provider_id?: string; model?: string; scope?: string[]; web?: boolean; attachment_ids?: string[] },
  onEvent: (e: StreamEvent) => void,
  signal: AbortSignal,
): Promise<void> {
  return postStream(`/conversations/${conversationId}/messages`, body, onEvent, signal);
}
