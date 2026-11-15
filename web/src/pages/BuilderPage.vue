<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { useRoute, useRouter } from "vue-router";

import { type Agent, type AgentInput, api, type Attachment, type Build, type Draft, type Message, postStream, type TestCase } from "../api";
import AgentConfig from "../components/AgentConfig.vue";
import Composer from "../components/Composer.vue";
import Icon from "../components/Icon.vue";
import PublishInfo from "../components/PublishInfo.vue";
import ReplyBody from "../components/ReplyBody.vue";
import { errorCodeText, errorText } from "../i18n";
import { folderPath, loadFolders, loadProviders, state } from "../state";

const { t } = useI18n();
const route = useRoute();
const router = useRouter();

const builds = ref<Build[]>([]);
const build = ref<Build | null>(null);
const messages = ref<Message[]>([]);
const attachments = ref<Attachment[]>([]);
const draftText = ref("");
const busy = ref(false);
const live = reactive({ content: "", reasoning: "" });
const error = ref("");
const draftNote = ref("");
const attaching = ref(false);
const rejected = ref<{ name: string; code: string }[]>([]);
const editing = ref(false);
const testing = ref(false);
const publishing = ref(false);
const published = ref<Agent | null>(null);
const confirming = ref("");
const scroller = ref<HTMLElement>();
let abort: AbortController | null = null;

const id = computed(() => (route.params.id as string) || "");
const draft = computed(() => build.value?.draft);
const waiting = computed(() => attachments.value.filter((f) => !f.message_id));
const filesOf = (messageId: string) => attachments.value.filter((f) => f.message_id === messageId);
const ready = computed(() => !!draft.value?.name.trim() && !!draft.value?.duty.trim());
const tests = computed(() => build.value?.tests ?? []);
const passed = computed(() => tests.value.filter((x) => x.pass === true).length);
const model = computed(() => ({ provider_id: state.selection.providerId, model: state.selection.model }));
/** 进行到哪一步：访谈 → 整理 → 试答 → 发布 */
const step = computed(() => (build.value?.agent_id ? 4 : tests.value.some((x) => x.pass !== null) ? 3 : ready.value ? 2 : 1));
const fields = computed(() => {
  const d = draft.value;
  if (!d) return [];
  const web = { off: "web.modeOff", fallback: "web.modeFallback", always: "web.modeAlways" }[d.web || "off"] ?? "web.modeOff";
  return [
    { label: "agents.name", value: d.name, need: true },
    { label: "agents.description", value: d.description },
    { label: "agents.duty", value: d.duty, need: true },
    { label: "agents.outOfScope", value: d.out_of_scope },
    { label: "agents.instructions", value: d.instructions },
    { label: "web.agentMode", value: t(web) },
    { label: "kb.agentFolders", value: d.folders.map(folderPath).filter(Boolean).join("、") },
    { label: "agents.opening", value: d.opening },
    { label: "agents.examples", value: d.examples.join("\n") },
  ];
});

function toBottom() {
  nextTick(() => scroller.value && (scroller.value.scrollTop = scroller.value.scrollHeight));
}

async function load() {
  error.value = draftNote.value = "";
  published.value = null;
  rejected.value = [];
  try {
    if (!id.value) {
      build.value = null;
      builds.value = await api.builds();
      return;
    }
    const got = await api.build(id.value);
    build.value = got.build;
    messages.value = got.messages;
    attachments.value = got.attachments;
    if (got.build.agent_id) published.value = await api.agent(got.build.agent_id).catch(() => null);
    toBottom();
  } catch (e) {
    error.value = errorText(e);
  }
}
watch(id, () => {
  abort?.abort();
  void load();
});
onMounted(() => {
  void load();
  if (!state.providersLoaded) void loadProviders().catch(() => {});
  void loadFolders().catch(() => {});
});
onBeforeUnmount(() => abort?.abort());

async function start() {
  try {
    const b = await api.createBuild(t("builder.opening"));
    await router.push(`/builder/${b.id}`);
  } catch (e) {
    error.value = errorText(e);
  }
}

async function remove(b: Build) {
  if (confirming.value !== b.id) {
    confirming.value = b.id;
    setTimeout(() => confirming.value === b.id && (confirming.value = ""), 3000);
    return;
  }
  try {
    await api.deleteBuild(b.id);
    builds.value = builds.value.filter((x) => x.id !== b.id);
  } catch (e) {
    error.value = errorText(e);
  }
}

async function attach(files: File[]) {
  if (!build.value) return;
  attaching.value = true;
  try {
    const r = await api.attach(build.value.conversation_id, files);
    attachments.value.push(...r.added);
    rejected.value = r.rejected;
  } catch (e) {
    error.value = errorText(e);
  } finally {
    attaching.value = false;
  }
}
async function detach(fileId: string) {
  if (!build.value) return;
  try {
    await api.removeAttachment(build.value.conversation_id, fileId);
    attachments.value = attachments.value.filter((f) => f.id !== fileId);
  } catch (e) {
    error.value = errorText(e);
  }
}

async function send() {
  const content = draftText.value.trim();
  if (!content || busy.value || !build.value || !model.value.model) return;
  error.value = draftNote.value = "";
  rejected.value = [];
  busy.value = true;
  const sending = waiting.value.map((f) => f.id);
  const pending: Message = { id: "pending", conversation_id: "", role: "user", content, reasoning: "", status: "ok", error_code: null, sources: [], model: null, cloud: false, created_at: Date.now() };
  messages.value.push(pending);
  draftText.value = "";
  toBottom();
  abort = new AbortController();
  try {
    await postStream(
      `/builds/${build.value.id}/messages`,
      { content, ...model.value, attachment_ids: sending },
      (e) => {
        if (e.type === "user") {
          const i = messages.value.findIndex((m) => m.id === "pending");
          if (i >= 0) messages.value[i] = e.message;
          attachments.value = [...attachments.value.filter((f) => f.message_id), ...e.attachments];
        } else if (e.type === "delta") live.content += e.text;
        else if (e.type === "reasoning") live.reasoning += e.text;
        else if (e.type === "done" || e.type === "error") {
          if (e.message) messages.value.push(e.message);
          else if (e.type === "error") error.value = errorCodeText(e.code, e.detail);
          live.content = live.reasoning = "";
        } else if (e.type === "draft" && build.value) build.value.draft = e.draft;
        else if (e.type === "draft_error") draftNote.value = "builder.draftError";
        toBottom();
      },
      abort.signal,
    );
  } catch (e) {
    if ((e as Error).name === "AbortError") {
      await new Promise((r) => setTimeout(r, 400));
      await load();
    } else {
      const i = messages.value.findIndex((m) => m.id === "pending");
      if (i >= 0) {
        messages.value.splice(i, 1);
        draftText.value = content;
      }
      error.value = errorText(e);
    }
  } finally {
    live.content = live.reasoning = "";
    busy.value = false;
    abort = null;
  }
}

async function saveDraft(input: AgentInput) {
  if (!build.value) return;
  try {
    build.value = await api.saveDraft(build.value.id, input);
    editing.value = false;
  } catch (e) {
    error.value = errorText(e);
  }
}

async function runTests() {
  if (!build.value || testing.value || !model.value.model) return;
  testing.value = true;
  error.value = "";
  try {
    await postStream(`/builds/${build.value.id}/test`, model.value, (e) => {
      if (!build.value) return;
      if (e.type === "tests") build.value.tests = e.tests;
      else if (e.type === "test") build.value.tests[e.index] = e.test;
      else if (e.type === "done" && e.tests) build.value.tests = e.tests;
      else if (e.type === "error") error.value = errorCodeText(e.code, e.detail);
    });
  } catch (e) {
    error.value = errorText(e);
  } finally {
    testing.value = false;
  }
}

async function publish() {
  if (!build.value || !model.value.model) return;
  publishing.value = true;
  error.value = "";
  try {
    published.value = await api.publishBuild(build.value.id, model.value);
    build.value.agent_id = published.value.id;
  } catch (e) {
    error.value = errorText(e);
  } finally {
    publishing.value = false;
  }
}

const verdict = (x: TestCase) => (x.pass === true ? "pass" : x.pass === false ? "fail" : x.answer === null && !x.reason ? "" : "unjudged");
const asDraftAgent = (d: Draft): Agent => ({ ...d, id: "", public_token: null, api_key: null, updated_at: 0 });
</script>

<template>
  <header class="page-head">
    <div class="grow">
      <div class="eyebrow"><RouterLink v-if="id" class="crumb" to="/builder">{{ $t("builder.eyebrow") }}</RouterLink><template v-else>{{ $t("builder.eyebrow") }}</template></div>
      <h1>{{ (id && draft?.name) || $t("builder.title") }}</h1>
    </div>
    <div v-if="id" class="steps" aria-hidden="true">
      <span v-for="(s, i) in ['stepTalk', 'stepDraft', 'stepTest', 'stepPublish']" :key="s" class="step" :class="{ done: i + 1 < step || (i === 3 && step === 4), now: i + 1 === step && step < 4 }">{{ $t(`builder.${s}`) }}</span>
    </div>
    <button v-else class="btn primary" @click="start"><Icon name="plus" />{{ $t("builder.start") }}</button>
  </header>
  <div v-if="error" class="alert err" role="alert">{{ error }}</div>

  <template v-if="!id">
    <div class="card"><p style="max-width: 46em">{{ $t("builder.intro") }}</p></div>
    <template v-if="builds.length">
      <h2>{{ $t("builder.recent") }}</h2>
      <div class="agents">
        <RouterLink v-for="b in builds" :key="b.id" class="card agent" :to="`/builder/${b.id}`">
          <div class="top">
            <span class="seal" aria-hidden="true">{{ [...(b.draft.name || "…")][0] }}</span>
            <div><h2>{{ b.draft.name || $t("builder.untitled") }}</h2><span class="muted">{{ new Date(b.updated_at).toLocaleString() }}</span></div>
          </div>
          <p v-if="b.draft.duty">{{ b.draft.duty }}</p>
          <div class="row">
            <span v-if="b.agent_id" class="tag ok">✓ {{ $t("builder.published") }}</span>
            <span class="grow" />
            <button class="btn sm" :class="confirming === b.id ? 'danger' : 'ghost'" @click.prevent="remove(b)">{{ confirming === b.id ? $t("common.confirmDelete") : $t("common.delete") }}</button>
          </div>
        </RouterLink>
      </div>
    </template>
  </template>

  <div v-else-if="build" class="buildpage">
    <div class="card chat">
      <div ref="scroller" class="msgs">
        <template v-for="m in messages" :key="m.id">
          <div v-if="m.role === 'user'" class="msg me"><div v-if="filesOf(m.id).length" class="files"><span v-for="f in filesOf(m.id)" :key="f.id" class="att"><em>{{ f.name.split(".").pop()?.toUpperCase().slice(0, 4) }}</em><span>{{ f.name }}</span></span></div>{{ m.content }}</div>
          <div v-else class="msg bot"><ReplyBody :message="m" /></div>
        </template>
        <div v-if="busy" class="msg bot" aria-live="polite"><ReplyBody live :message="{ content: live.content, reasoning: live.reasoning, sources: [], status: 'ok', error_code: null, cloud: false, model: null }" /></div>
      </div>
      <div v-if="rejected.length" class="alert warn" style="flex-direction: column; align-items: flex-start">
        <span v-for="r in rejected" :key="r.name">{{ $t("kb.rejected", { name: r.name, reason: errorCodeText(r.code) }) }}</span>
      </div>
      <div v-if="state.providersLoaded && !state.selection.model" class="alert warn">
        <span class="grow"><b>{{ $t("chat.noModel") }}</b> · {{ $t("chat.noModelHint") }}</span>
        <RouterLink class="btn ghost sm" to="/settings/models">{{ $t("chat.goModels") }}</RouterLink>
      </div>
      <Composer v-model="draftText" :busy="busy" :placeholder="$t('builder.placeholder')" :attachments="waiting" :attaching="attaching" @attach="attach" @detach="detach" @send="send" @stop="abort?.abort()" />
      <div class="tip">{{ $t("web.uploadHint") }}</div>
    </div>

    <div class="buildside">
      <div class="card spec">
        <div class="row" style="margin-bottom: 12px"><h2 class="grow">{{ $t("builder.draft") }}</h2><button class="btn ghost sm" @click="editing = true">{{ $t("builder.edit") }}</button></div>
        <dl>
          <template v-for="f in fields" :key="f.label">
            <dt>{{ $t(f.label) }}</dt>
            <dd :class="{ todo: !f.value && f.need, muted: !f.value && !f.need }">{{ f.value || $t("builder.empty") }}</dd>
          </template>
        </dl>
        <p v-if="draftNote" class="muted" style="margin-top: 10px">{{ $t(draftNote) }}</p>
        <p v-else-if="!ready" class="muted" style="margin-top: 10px">{{ $t("builder.draftHint") }}</p>
      </div>

      <div class="card">
        <div class="row" style="margin-bottom: 10px">
          <h2>{{ $t("builder.tests") }}</h2>
          <span v-if="tests.length" class="muted">{{ $t("builder.passed", { pass: passed, total: tests.length }) }}</span>
          <span class="grow" />
          <button class="btn ghost sm" :disabled="!ready || testing" @click="runTests">{{ testing ? $t("builder.testing") : tests.length ? $t("builder.rerun") : $t("builder.runTests") }}</button>
        </div>
        <p v-if="!tests.length" class="muted">{{ ready ? $t("builder.testsHint") : $t("builder.needMore") }}</p>
        <div class="tests">
          <details v-for="(x, i) in tests" :key="i" class="test">
            <summary>
              <span>{{ x.question }}</span>
              <span v-if="verdict(x) === 'pass'" class="tag ok">✓ {{ $t("builder.pass") }}</span>
              <span v-else-if="verdict(x) === 'fail'" class="tag err">× {{ x.reason || $t("builder.fail") }}</span>
              <span v-else-if="verdict(x) === 'unjudged'" class="tag warn">! {{ $t("builder.unjudged") }}</span>
              <span v-else class="tag">…</span>
            </summary>
            <div class="more"><b>{{ $t("builder.expect") }}</b>　{{ x.expect || "—" }}<template v-if="x.answer"><br /><br /><b>{{ $t("builder.answer") }}</b>　{{ x.answer }}</template></div>
          </details>
        </div>
        <div class="row" style="margin-top: 14px">
          <button class="btn primary" :disabled="!ready || publishing || !model.model" @click="publish">{{ build.agent_id ? $t("builder.republish") : $t("builder.publish") }}</button>
          <span class="muted">{{ $t("builder.publishHint") }}</span>
        </div>
      </div>

      <div v-if="published" class="card">
        <div class="row" style="margin-bottom: 10px"><h2 class="grow">✓ {{ $t("builder.done") }} · {{ published.name }}</h2><RouterLink class="btn primary sm" :to="`/agents/${published.id}`">{{ $t("builder.open") }}</RouterLink></div>
        <PublishInfo :agent="published" />
      </div>
    </div>
  </div>

  <AgentConfig v-if="editing && draft" :agent="asDraftAgent(draft)" draft-mode @close="editing = false" @draft="saveDraft" />
</template>
