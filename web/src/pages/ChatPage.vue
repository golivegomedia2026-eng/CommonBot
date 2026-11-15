<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";

import { type Agent, api, ApiError, type Attachment, type Conversation, type Message, sendMessage, type Source } from "../api";
import AgentConfig from "../components/AgentConfig.vue";
import Composer from "../components/Composer.vue";
import Icon from "../components/Icon.vue";
import { errorCodeText, errorText, locText } from "../i18n";
import { renderMarkdown } from "../md";
import { loadFolders, loadProviders, state } from "../state";
import { copyText, isToday, linkCitations } from "../util";

const route = useRoute();
const router = useRouter();

const agent = ref<Agent | null>(null);
const configuring = ref(false);
const conversations = ref<Conversation[]>([]);
const messages = ref<Message[]>([]);
const draft = ref("");
const busy = ref(false);
const live = reactive({ content: "", reasoning: "", sources: [] as Source[], webNote: "", fallback: "" });
/** 这次对话是否联网；null 表示按智能体的默认。 */
const web = ref<boolean | null>(null);
/** 对话里的全部文件；message_id 为空的是还没发出的。 */
const attachments = ref<Attachment[]>([]);
const attaching = ref(false);
const rejected = ref<{ name: string; code: string }[]>([]);
const waiting = computed(() => attachments.value.filter((f) => !f.message_id));
const filesOf = (messageId: string) => attachments.value.filter((f) => f.message_id === messageId);
/** 这次回答没联上网的说明；跟着最后一条回答显示。 */
const webNote = ref("");
/** 这次对话查哪些文件夹；null 表示按智能体的默认。 */
const scope = ref<string[] | null>(null);
/** 展开看原文的出处：消息编号 + 出处编号 */
const openSource = ref("");
const error = ref("");
const histOpen = ref(false);
const confirming = ref("");
const copied = ref("");
const scroller = ref<HTMLElement>();
const composer = ref<InstanceType<typeof Composer>>();
let abort: AbortController | null = null;
/** 刚由发送动作新建的对话，路由变化时不必重新读取。 */
let justCreated = "";

const agentId = computed(() => route.params.agentId as string);
const base = computed(() => `/agents/${agentId.value}`);
const currentId = computed(() => (route.params.id as string) || "");
const current = computed(() => conversations.value.find((c) => c.id === currentId.value));
const groups = computed(() => [
  { label: "chat.today", items: conversations.value.filter((c) => isToday(c.updated_at)) },
  { label: "chat.earlier", items: conversations.value.filter((c) => !isToday(c.updated_at)) },
]);
const noModel = computed(() => !agent.value?.model && state.providersLoaded && !state.selection.model && !Object.values(state.checks).some((c) => c.state === "checking"));

function toBottom(force = false) {
  const el = scroller.value;
  if (!el) return;
  // 用户往上翻看时不打扰
  if (force || el.scrollHeight - el.scrollTop - el.clientHeight < 120) nextTick(() => (el.scrollTop = el.scrollHeight));
}

async function loadList() {
  conversations.value = await api.conversations(agentId.value);
}

async function open(id: string) {
  error.value = "";
  if (!id) {
    messages.value = [];
    scope.value = null;
    web.value = null;
    attachments.value = [];
    return;
  }
  try {
    const got = await api.conversation(id);
    messages.value = got.messages;
    scope.value = got.conversation.scope;
    web.value = got.conversation.web;
    attachments.value = got.attachments;
    toBottom(true);
  } catch (e) {
    if (e instanceof ApiError && e.code === "not_found") return void router.replace(base.value);
    error.value = errorText(e);
  }
}

watch(currentId, (id) => {
  histOpen.value = false;
  rejected.value = [];
  webNote.value = "";
  if (id && id === justCreated) return;
  abort?.abort();
  void open(id);
});

async function loadAgent() {
  agent.value = null;
  conversations.value = [];
  try {
    const [a] = await Promise.all([api.agent(agentId.value), loadList(), open(currentId.value), state.providersLoaded ? null : loadProviders()]);
    agent.value = a;
    void loadFolders().catch(() => {});
    // 从知识库的文件夹页过来：带着范围，问题直接发出去
    const { scope: s, q } = route.query;
    if (typeof s === "string" && s) scope.value = [s];
    if (typeof q === "string" && q) {
      await router.replace({ path: route.path });
      void send(q);
    }
  } catch (e) {
    if (e instanceof ApiError && e.code === "not_found") return void router.replace("/agents");
    error.value = errorText(e);
  }
  composer.value?.focus();
}
watch(agentId, (id) => id && void loadAgent());
onMounted(loadAgent);
onBeforeUnmount(() => abort?.abort());

/** 没有对话时先建一个（上传文件、发消息都需要）。 */
async function ensureConversation(): Promise<string> {
  if (currentId.value) return currentId.value;
  const id = (await api.createConversation(agentId.value)).id;
  justCreated = id;
  await router.replace(`${base.value}/${id}`);
  return id;
}

async function attach(files: File[]) {
  attaching.value = true;
  error.value = "";
  try {
    const id = await ensureConversation();
    const r = await api.attach(id, files);
    attachments.value.push(...r.added);
    rejected.value = r.rejected;
  } catch (e) {
    error.value = errorText(e);
  } finally {
    attaching.value = false;
  }
}
async function detach(id: string) {
  try {
    await api.removeAttachment(currentId.value, id);
    attachments.value = attachments.value.filter((f) => f.id !== id);
  } catch (e) {
    error.value = errorText(e);
  }
}

async function send(text?: string) {
  const content = (text ?? draft.value).trim();
  // 智能体固定了模型时由服务决定用哪个，这里不传
  const fixed = !!agent.value?.model;
  const { providerId, model } = state.selection;
  if (!content || busy.value || (!fixed && !model)) return;
  error.value = "";
  webNote.value = "";
  rejected.value = [];
  busy.value = true;
  let id = currentId.value;
  const sending = waiting.value.map((f) => f.id);
  const pending: Message = { id: "pending", model: null, cloud: false, conversation_id: id, role: "user", content, reasoning: "", status: "ok", error_code: null, sources: [], created_at: Date.now() };
  try {
    id = await ensureConversation();
    messages.value.push(pending);
    if (text === undefined) draft.value = "";
    toBottom(true);
    abort = new AbortController();
    await sendMessage(
      id,
      { content, ...(fixed ? {} : { provider_id: providerId, model }), ...(scope.value ? { scope: scope.value } : {}), ...(web.value === null ? {} : { web: web.value }), ...(sending.length ? { attachment_ids: sending } : {}) },
      (e) => {
        if (e.type === "user") {
          const i = messages.value.findIndex((m) => m.id === "pending");
          if (i >= 0) messages.value[i] = e.message;
          // 随这条消息发出的文件：换成服务端确认后的记录
          attachments.value = [...attachments.value.filter((f) => f.message_id), ...e.attachments];
          const at = conversations.value.findIndex((c) => c.id === e.conversation.id);
          if (at >= 0) conversations.value.splice(at, 1);
          conversations.value.unshift(e.conversation);
        } else if (e.type === "sources") {
          live.sources = e.sources;
          live.webNote = e.web_note === "web_not_configured" ? "web.noteNotConfigured" : e.web_note ? "web.noteFailed" : "";
        } else if (e.type === "fallback") {
          live.fallback = e.model;
        } else if (e.type === "delta") {
          live.content += e.text;
        } else if (e.type === "reasoning") {
          live.reasoning += e.text;
        } else {
          if (e.message) messages.value.push(e.message);
          else if (e.type === "error") error.value = errorCodeText(e.code, e.detail);
          webNote.value = live.webNote;
          live.content = live.reasoning = live.webNote = live.fallback = "";
          live.sources = [];
        }
        toBottom();
      },
      abort.signal,
    );
  } catch (e) {
    if ((e as Error).name === "AbortError") {
      // 服务在连接断开后保存已生成的部分，稍等再读回来
      await new Promise((r) => setTimeout(r, 400));
      if (id === currentId.value) await open(id);
    } else {
      const i = messages.value.findIndex((m) => m.id === "pending");
      if (i >= 0) {
        messages.value.splice(i, 1);
        if (text === undefined) draft.value = content;
      }
      error.value = errorText(e);
    }
  } finally {
    live.content = live.reasoning = live.webNote = live.fallback = "";
    live.sources = [];
    busy.value = false;
    abort = null;
    void loadList().catch(() => {});
  }
}

async function remove(id: string) {
  if (confirming.value !== id) {
    confirming.value = id;
    setTimeout(() => confirming.value === id && (confirming.value = ""), 3000);
    return;
  }
  confirming.value = "";
  try {
    await api.deleteConversation(id);
    conversations.value = conversations.value.filter((c) => c.id !== id);
    if (id === currentId.value) await router.replace(base.value);
  } catch (e) {
    error.value = errorText(e);
  }
}

function saved(a: Agent) {
  agent.value = a;
  configuring.value = false;
}

/** 点回答里的出处编号：展开对应的原文片段。 */
function citeClick(e: MouseEvent, messageId: string) {
  const n = (e.target as HTMLElement).closest<HTMLElement>("[data-cite]")?.dataset.cite;
  if (n) toggleSource(messageId, Number(n));
}
function toggleSource(messageId: string, n: number) {
  const key = `${messageId}:${n}`;
  openSource.value = openSource.value === key ? "" : key;
}

async function copy(m: Message) {
  if (await copyText(m.content)) {
    copied.value = m.id;
    setTimeout(() => copied.value === m.id && (copied.value = ""), 1500);
  }
}
</script>

<template>
  <header class="page-head">
    <button class="btn ghost sm icon-only only-narrow" :aria-label="$t('chat.title')" @click="histOpen = true"><Icon name="list" /></button>
    <div class="grow">
      <div class="eyebrow"><RouterLink class="crumb" to="/agents">{{ $t("agents.all") }}</RouterLink></div>
      <h1>{{ agent?.name ?? "\u00a0" }}</h1>
    </div>
    <button class="btn ghost" :disabled="!agent" @click="configuring = true"><Icon name="gear" />{{ $t("agents.config") }}</button>
    <RouterLink class="btn primary" :to="base"><Icon name="plus" />{{ $t("chat.newChat") }}</RouterLink>
  </header>

  <div class="chatpage">
    <div v-if="histOpen" class="mask only-narrow" @click="histOpen = false" />
    <div class="card hist" :class="{ open: histOpen }">
      <template v-for="g in groups" :key="g.label">
        <small v-if="g.items.length">{{ $t(g.label) }}</small>
        <div v-for="c in g.items" :key="c.id" class="h" :class="{ on: c.id === currentId }" role="link" tabindex="0" @click="router.push(`${base}/${c.id}`)" @keydown.enter="router.push(`${base}/${c.id}`)">
          <span :title="c.title">{{ confirming === c.id ? $t("common.confirmDelete") : c.title || $t("chat.newChat") }}</span>
          <button class="x" :aria-label="$t('common.delete')" :title="$t('common.delete')" @click.stop="remove(c.id)"><Icon name="x" /></button>
        </div>
      </template>
      <div v-if="!conversations.length" class="muted" style="padding: 8px 10px">{{ $t("chat.empty") }}</div>
    </div>

    <div class="card chat">
      <div ref="scroller" class="msgs">
        <div v-if="!messages.length && !busy && agent?.opening" class="msg bot" style="white-space: pre-wrap">{{ agent.opening }}</div>
        <div v-else-if="!messages.length && !busy && agent" class="empty">
          <h2>{{ agent.name }}</h2>
          <p>{{ agent.description || $t("chat.emptyHint") }}</p>
        </div>
        <template v-for="m in messages" :key="m.id">
          <div v-if="m.role === 'user'" class="msg me"><div v-if="filesOf(m.id).length" class="files"><span v-for="f in filesOf(m.id)" :key="f.id" class="att"><em>{{ f.name.split(".").pop()?.toUpperCase().slice(0, 4) }}</em><span>{{ f.name }}</span></span></div>{{ m.content }}</div>
          <div v-else class="msg bot">
            <details v-if="m.reasoning"><summary>{{ $t("chat.thinking") }}</summary><div>{{ m.reasoning }}</div></details>
            <div v-if="m.content" class="md" @click="citeClick($event, m.id)" v-html="linkCitations(renderMarkdown(m.content), m.sources.length)" />
            <div v-if="m.sources.length" class="srcs">
              <template v-for="kind in ['kb', 'file', 'web'] as const" :key="kind">
                <template v-if="m.sources.some((s) => s.kind === kind)">
                  <b>{{ $t(kind === "kb" ? "kb.sources" : kind === "file" ? "web.files" : "web.sources") }}</b>
                  <template v-for="s in m.sources.filter((s) => s.kind === kind)" :key="s.n">
                    <button class="src" :aria-expanded="openSource === `${m.id}:${s.n}`" @click="toggleSource(m.id, s.n)">
                      <span class="cite" :class="{ on: openSource === `${m.id}:${s.n}` }">{{ s.n }}</span>
                      <span v-if="s.kind === 'kb'">{{ s.document }} · {{ [locText(s.locator), s.folder].filter(Boolean).join(" · ") }}</span>
                      <span v-else-if="s.kind === 'file'">{{ s.document }}</span>
                      <span v-else>{{ s.title || s.url }}</span>
                    </button>
                    <blockquote v-if="openSource === `${m.id}:${s.n}`">{{ s.text }}<template v-if="s.kind === 'web'"><br /><a :href="s.url" target="_blank" rel="noopener noreferrer">{{ s.url }}</a></template></blockquote>
                  </template>
                </template>
              </template>
            </div>
            <div v-if="webNote && m.id === messages[messages.length - 1]?.id" class="muted" style="margin-top: 6px">{{ $t(webNote) }}</div>
            <div v-if="m.status === 'error'" class="alert err">{{ errorCodeText(m.error_code ?? "http") }}</div>
            <div class="foot">
              <span v-if="m.status === 'stopped'" class="tag warn">{{ $t("chat.stopped") }}</span>
              <span v-if="m.cloud" class="tag info">{{ $t("web.cloudTag", { model: m.model }) }}</span>
              <button v-if="m.content" class="btn ghost sm" @click="copy(m)"><Icon name="copy" />{{ copied === m.id ? $t("common.copied") : $t("common.copy") }}</button>
            </div>
          </div>
        </template>
        <div v-if="busy" class="msg bot" aria-live="polite">
          <div v-if="live.fallback" class="alert info" style="margin-bottom: 6px">{{ $t("web.fallback", { model: live.fallback }) }}</div>
          <details v-if="live.reasoning" open><summary>{{ $t("chat.thinking") }}</summary><div>{{ live.reasoning }}</div></details>
          <div v-if="live.content" class="md" v-html="linkCitations(renderMarkdown(live.content), live.sources.length)" />
          <span class="caret" :title="$t('chat.generating')" />
        </div>
      </div>

      <div v-if="error" class="alert err" role="alert">{{ error }}</div>
      <div v-if="rejected.length" class="alert warn" style="flex-direction: column; align-items: flex-start">
        <span v-for="r in rejected" :key="r.name">{{ $t("kb.rejected", { name: r.name, reason: errorCodeText(r.code) }) }}</span>
      </div>
      <div v-if="noModel" class="alert warn">
        <span class="grow"><b>{{ $t("chat.noModel") }}</b> · {{ $t("chat.noModelHint") }}</span>
        <RouterLink class="btn ghost sm" to="/settings/models">{{ $t("chat.goModels") }}</RouterLink>
      </div>
      <div v-if="agent?.examples.length && !busy && !messages.length" class="quick">
        <button v-for="q in agent.examples" :key="q" class="chip" @click="send(q)">{{ q }}</button>
      </div>
      <Composer ref="composer" v-model="draft" :busy="busy" :placeholder="$t('chat.placeholder')" :locked-model="agent?.model" v-model:scope="scope" :default-scope="agent?.folders ?? []" v-model:web="web" :default-web="agent?.web === 'always'" :attachments="waiting" :attaching="attaching" @attach="attach" @detach="detach" @send="send()" @stop="abort?.abort()" />
      <div class="tip">{{ $t("web.uploadHint") }}</div>
    </div>
  </div>
  <AgentConfig v-if="configuring && agent" :agent="agent" @close="configuring = false" @saved="saved" @deleted="router.replace('/agents')" />
</template>
