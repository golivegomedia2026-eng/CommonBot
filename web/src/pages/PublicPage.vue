<script setup lang="ts">
// 发布出去的智能体的独立对话页：访客凭链接使用，只能和这一个智能体对话。
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import { useRoute } from "vue-router";

import { api, ApiError, type Message, postStream, type PublicInfo, type Source } from "../api";
import Icon from "../components/Icon.vue";
import ReplyBody from "../components/ReplyBody.vue";
import { errorCodeText, errorText } from "../i18n";

const route = useRoute();
const token = computed(() => route.params.token as string);
const storeKey = computed(() => `commonbot_pub_${token.value}`);

const info = ref<PublicInfo | null>(null);
const gone = ref(false);
const conversation = ref("");
const messages = ref<Message[]>([]);
const draft = ref("");
const busy = ref(false);
const live = reactive({ content: "", reasoning: "", sources: [] as Source[] });
const error = ref("");
const scroller = ref<HTMLElement>();
let abort: AbortController | null = null;

function toBottom() {
  nextTick(() => scroller.value && (scroller.value.scrollTop = scroller.value.scrollHeight));
}
function remember(id: string) {
  conversation.value = id;
  try {
    if (id) localStorage.setItem(storeKey.value, id);
    else localStorage.removeItem(storeKey.value);
  } catch {
    /* 记不住也能用，只是刷新后从新对话开始 */
  }
}

onMounted(async () => {
  try {
    info.value = await api.publicInfo(token.value);
    document.title = info.value.name;
    let saved = "";
    try {
      saved = localStorage.getItem(storeKey.value) ?? "";
    } catch {
      /* 同上 */
    }
    if (saved) {
      try {
        messages.value = (await api.publicConversation(token.value, saved)).messages;
        conversation.value = saved;
        toBottom();
      } catch {
        remember("");
      }
    }
  } catch (e) {
    if (e instanceof ApiError && e.code === "not_found") gone.value = true;
    else error.value = errorText(e);
  }
});
onBeforeUnmount(() => abort?.abort());

function fresh() {
  abort?.abort();
  messages.value = [];
  remember("");
  error.value = "";
}

async function send(text?: string) {
  const content = (text ?? draft.value).trim();
  if (!content || busy.value) return;
  error.value = "";
  busy.value = true;
  const pending: Message = { id: "pending", conversation_id: "", role: "user", content, reasoning: "", status: "ok", error_code: null, sources: [], model: null, cloud: false, created_at: Date.now() };
  try {
    if (!conversation.value) remember((await api.publicNewConversation(token.value)).id);
    messages.value.push(pending);
    if (text === undefined) draft.value = "";
    toBottom();
    abort = new AbortController();
    await postStream(
      `/public/${token.value}/conversations/${conversation.value}/messages`,
      { content },
      (e) => {
        if (e.type === "user") {
          const i = messages.value.findIndex((m) => m.id === "pending");
          if (i >= 0) messages.value[i] = e.message;
        } else if (e.type === "sources") live.sources = e.sources;
        else if (e.type === "delta") live.content += e.text;
        else if (e.type === "reasoning") live.reasoning += e.text;
        else if (e.type === "done" || e.type === "error") {
          if (e.message) messages.value.push(e.message);
          else if (e.type === "error") error.value = errorCodeText(e.code, e.detail);
          live.content = live.reasoning = "";
          live.sources = [];
        }
        toBottom();
      },
      abort.signal,
    );
  } catch (e) {
    if ((e as Error).name === "AbortError") {
      await new Promise((r) => setTimeout(r, 400));
      if (conversation.value) messages.value = (await api.publicConversation(token.value, conversation.value).catch(() => ({ messages: messages.value }))).messages;
    } else {
      const i = messages.value.findIndex((m) => m.id === "pending");
      if (i >= 0) {
        messages.value.splice(i, 1);
        if (text === undefined) draft.value = content;
      }
      if (e instanceof ApiError && e.code === "not_found") gone.value = true;
      else error.value = errorText(e);
    }
  } finally {
    live.content = live.reasoning = "";
    live.sources = [];
    busy.value = false;
    abort = null;
  }
}

function onKey(e: KeyboardEvent) {
  if (e.key !== "Enter" || e.shiftKey || e.isComposing || e.keyCode === 229) return;
  e.preventDefault();
  void send();
}
</script>

<template>
  <div v-if="gone" class="center"><div class="card"><div class="alert warn">{{ $t("pub.gone") }}</div></div></div>
  <div v-else class="pubpage">
    <header class="pubhead">
      <span class="seal" aria-hidden="true">{{ [...(info?.name ?? "…")][0] }}</span>
      <div class="grow"><h1>{{ info?.name ?? " " }}</h1><div v-if="info?.description" class="muted">{{ info.description }}</div></div>
      <button class="btn ghost sm" :disabled="!messages.length" @click="fresh"><Icon name="plus" />{{ $t("pub.newChat") }}</button>
    </header>
    <div class="card chat">
      <div ref="scroller" class="msgs">
        <div v-if="!messages.length && !busy && info?.opening" class="msg bot" style="white-space: pre-wrap">{{ info.opening }}</div>
        <template v-for="m in messages" :key="m.id">
          <div v-if="m.role === 'user'" class="msg me">{{ m.content }}</div>
          <div v-else class="msg bot"><ReplyBody :message="m" /></div>
        </template>
        <div v-if="busy" class="msg bot" aria-live="polite"><ReplyBody live :message="{ content: live.content, reasoning: live.reasoning, sources: live.sources, status: 'ok', error_code: null, cloud: false, model: null }" /></div>
      </div>
      <div v-if="error" class="alert err" role="alert">{{ error }}</div>
      <div v-if="info?.examples.length && !busy && !messages.length" class="quick">
        <button v-for="q in info.examples" :key="q" class="chip" @click="send(q)">{{ q }}</button>
      </div>
      <div class="composer" style="margin-bottom: 14px">
        <textarea v-model="draft" rows="2" :placeholder="$t('chat.placeholder')" :aria-label="$t('chat.placeholder')" @keydown="onKey" />
        <div class="tools">
          <button v-if="busy" class="btn ghost sm send" @click="abort?.abort()"><Icon name="stop" />{{ $t("chat.stop") }}</button>
          <button v-else class="btn primary sm send" :disabled="!draft.trim() || !info" @click="send()"><Icon name="send" />{{ $t("chat.send") }}</button>
        </div>
      </div>
    </div>
    <footer class="pubfoot">{{ $t("pub.poweredBy") }}</footer>
  </div>
</template>
