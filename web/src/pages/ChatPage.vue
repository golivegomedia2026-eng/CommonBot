<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";

import { api, ApiError, type Conversation, type Message, sendMessage } from "../api";
import Composer from "../components/Composer.vue";
import Icon from "../components/Icon.vue";
import { errorCodeText, errorText } from "../i18n";
import { renderMarkdown } from "../md";
import { loadProviders, state } from "../state";
import { copyText, isToday } from "../util";

const route = useRoute();
const router = useRouter();

const conversations = ref<Conversation[]>([]);
const messages = ref<Message[]>([]);
const draft = ref("");
const busy = ref(false);
const live = reactive({ content: "", reasoning: "" });
const error = ref("");
const histOpen = ref(false);
const confirming = ref("");
const copied = ref("");
const scroller = ref<HTMLElement>();
const composer = ref<InstanceType<typeof Composer>>();
let abort: AbortController | null = null;
/** 刚由发送动作新建的对话，路由变化时不必重新读取。 */
let justCreated = "";

const currentId = computed(() => (route.params.id as string) || "");
const current = computed(() => conversations.value.find((c) => c.id === currentId.value));
const groups = computed(() => [
  { label: "chat.today", items: conversations.value.filter((c) => isToday(c.updated_at)) },
  { label: "chat.earlier", items: conversations.value.filter((c) => !isToday(c.updated_at)) },
]);
const noModel = computed(() => state.providersLoaded && !state.selection.model && !Object.values(state.checks).some((c) => c.state === "checking"));

function toBottom(force = false) {
  const el = scroller.value;
  if (!el) return;
  // 用户往上翻看时不打扰
  if (force || el.scrollHeight - el.scrollTop - el.clientHeight < 120) nextTick(() => (el.scrollTop = el.scrollHeight));
}

async function loadList() {
  conversations.value = await api.conversations();
}

async function open(id: string) {
  error.value = "";
  if (!id) {
    messages.value = [];
    return;
  }
  try {
    messages.value = (await api.conversation(id)).messages;
    toBottom(true);
  } catch (e) {
    if (e instanceof ApiError && e.code === "not_found") return void router.replace("/chat");
    error.value = errorText(e);
  }
}

watch(currentId, (id) => {
  histOpen.value = false;
  if (id && id === justCreated) return;
  abort?.abort();
  void open(id);
});

onMounted(async () => {
  try {
    await Promise.all([loadList(), open(currentId.value), state.providersLoaded ? null : loadProviders()]);
  } catch (e) {
    error.value = errorText(e);
  }
  composer.value?.focus();
});
onBeforeUnmount(() => abort?.abort());

async function send() {
  const content = draft.value.trim();
  const { providerId, model } = state.selection;
  if (!content || busy.value || !model) return;
  error.value = "";
  busy.value = true;
  let id = currentId.value;
  const pending: Message = { id: "pending", conversation_id: id, role: "user", content, reasoning: "", status: "ok", error_code: null, created_at: Date.now() };
  try {
    if (!id) {
      id = (await api.createConversation()).id;
      justCreated = id;
      await router.replace(`/chat/${id}`);
    }
    messages.value.push(pending);
    draft.value = "";
    toBottom(true);
    abort = new AbortController();
    await sendMessage(
      id,
      { content, provider_id: providerId, model },
      (e) => {
        if (e.type === "user") {
          const i = messages.value.findIndex((m) => m.id === "pending");
          if (i >= 0) messages.value[i] = e.message;
          const at = conversations.value.findIndex((c) => c.id === e.conversation.id);
          if (at >= 0) conversations.value.splice(at, 1);
          conversations.value.unshift(e.conversation);
        } else if (e.type === "delta") {
          live.content += e.text;
        } else if (e.type === "reasoning") {
          live.reasoning += e.text;
        } else {
          if (e.message) messages.value.push(e.message);
          else if (e.type === "error") error.value = errorCodeText(e.code, e.detail);
          live.content = live.reasoning = "";
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
        draft.value = content;
      }
      error.value = errorText(e);
    }
  } finally {
    live.content = live.reasoning = "";
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
    if (id === currentId.value) await router.replace("/chat");
  } catch (e) {
    error.value = errorText(e);
  }
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
      <div class="eyebrow">{{ $t("nav.workbench") }}</div>
      <h1>{{ current?.title || $t("chat.title") }}</h1>
    </div>
    <RouterLink class="btn primary" to="/chat"><Icon name="plus" />{{ $t("chat.newChat") }}</RouterLink>
  </header>

  <div class="chatpage">
    <div v-if="histOpen" class="mask only-narrow" @click="histOpen = false" />
    <div class="card hist" :class="{ open: histOpen }">
      <template v-for="g in groups" :key="g.label">
        <small v-if="g.items.length">{{ $t(g.label) }}</small>
        <div v-for="c in g.items" :key="c.id" class="h" :class="{ on: c.id === currentId }" role="link" tabindex="0" @click="router.push(`/chat/${c.id}`)" @keydown.enter="router.push(`/chat/${c.id}`)">
          <span :title="c.title">{{ confirming === c.id ? $t("common.confirmDelete") : c.title || $t("chat.newChat") }}</span>
          <button class="x" :aria-label="$t('common.delete')" :title="$t('common.delete')" @click.stop="remove(c.id)"><Icon name="x" /></button>
        </div>
      </template>
      <div v-if="!conversations.length" class="muted" style="padding: 8px 10px">{{ $t("chat.empty") }}</div>
    </div>

    <div class="card chat">
      <div ref="scroller" class="msgs">
        <div v-if="!messages.length && !busy" class="empty">
          <h2>{{ $t("chat.empty") }}</h2>
          <p>{{ $t("chat.emptyHint") }}</p>
        </div>
        <template v-for="m in messages" :key="m.id">
          <div v-if="m.role === 'user'" class="msg me">{{ m.content }}</div>
          <div v-else class="msg bot">
            <details v-if="m.reasoning"><summary>{{ $t("chat.thinking") }}</summary><div>{{ m.reasoning }}</div></details>
            <div v-if="m.content" class="md" v-html="renderMarkdown(m.content)" />
            <div v-if="m.status === 'error'" class="alert err">{{ errorCodeText(m.error_code ?? "http") }}</div>
            <div class="foot">
              <span v-if="m.status === 'stopped'" class="tag warn">{{ $t("chat.stopped") }}</span>
              <button v-if="m.content" class="btn ghost sm" @click="copy(m)"><Icon name="copy" />{{ copied === m.id ? $t("common.copied") : $t("common.copy") }}</button>
            </div>
          </div>
        </template>
        <div v-if="busy" class="msg bot" aria-live="polite">
          <details v-if="live.reasoning" open><summary>{{ $t("chat.thinking") }}</summary><div>{{ live.reasoning }}</div></details>
          <div v-if="live.content" class="md" v-html="renderMarkdown(live.content)" />
          <span class="caret" :title="$t('chat.generating')" />
        </div>
      </div>

      <div v-if="error" class="alert err" role="alert">{{ error }}</div>
      <div v-if="noModel" class="alert warn">
        <span class="grow"><b>{{ $t("chat.noModel") }}</b> · {{ $t("chat.noModelHint") }}</span>
        <RouterLink class="btn ghost sm" to="/settings/models">{{ $t("chat.goModels") }}</RouterLink>
      </div>
      <Composer ref="composer" v-model="draft" :busy="busy" :placeholder="$t('chat.placeholder')" @send="send" @stop="abort?.abort()" />
      <div class="tip">{{ $t("chat.tip") }}</div>
    </div>
  </div>
</template>
