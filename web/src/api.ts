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
export interface Conversation {
  id: string;
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
  created_at: number;
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

export const api = {
  health: () => call<Health>("GET", "/health"),
  enterAccessCode: (code: string) => call<{ ok: boolean }>("POST", "/access", { code }),
  accessCode: () => call<{ code: string; listen: string }>("GET", "/access-code"),
  resetAccessCode: () => call<{ code: string; listen: string }>("POST", "/access-code"),

  providers: () => call<Provider[]>("GET", "/providers"),
  createProvider: (input: ProviderInput) => call<Provider>("POST", "/providers", input),
  updateProvider: (id: string, input: ProviderInput) => call<Provider>("PUT", `/providers/${id}`, input),
  deleteProvider: (id: string) => call<void>("DELETE", `/providers/${id}`),
  checkProvider: (id: string) => call<{ models: string[]; latency_ms: number }>("POST", `/providers/${id}/check`),

  conversations: () => call<Conversation[]>("GET", "/conversations"),
  createConversation: () => call<Conversation>("POST", "/conversations"),
  conversation: (id: string) => call<{ conversation: Conversation; messages: Message[] }>("GET", `/conversations/${id}`),
  renameConversation: (id: string, title: string) => call<Conversation>("PATCH", `/conversations/${id}`, { title }),
  deleteConversation: (id: string) => call<void>("DELETE", `/conversations/${id}`),
};

export type StreamEvent =
  | { type: "user"; message: Message; conversation: Conversation }
  | { type: "delta"; text: string }
  | { type: "reasoning"; text: string }
  | { type: "done"; message: Message }
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

/** 发一句话并逐段接收回答。中止 signal 即停止生成，已生成的部分由服务保存。 */
export async function sendMessage(
  conversationId: string,
  body: { content: string; provider_id: string; model?: string },
  onEvent: (e: StreamEvent) => void,
  signal: AbortSignal,
): Promise<void> {
  let resp: Response;
  try {
    resp = await fetch(`/api/conversations/${conversationId}/messages`, {
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
