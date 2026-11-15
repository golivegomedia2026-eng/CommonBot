<script setup lang="ts">
// 智能体的配置抽屉：新建和编辑共用。
import { computed, onMounted, reactive, ref, watch } from "vue";

import { type Agent, type AgentInput, api } from "../api";
import { errorText } from "../i18n";
import { loadFolders, loadProviders, modelOptions, state } from "../state";
import FolderPicker from "./FolderPicker.vue";

const props = defineProps<{ agent: Agent | null }>();
const emit = defineEmits<{ close: []; saved: [Agent]; deleted: [] }>();

const a = props.agent;
const form = reactive({
  name: a?.name ?? "",
  description: a?.description ?? "",
  duty: a?.duty ?? "",
  out_of_scope: a?.out_of_scope ?? "",
  instructions: a?.instructions ?? "",
  opening: a?.opening ?? "",
  examples: (a?.examples ?? []).join("\n"),
  model: a?.model ? `${a.provider_id}\n${a.model}` : "",
  folders: [...(a?.folders ?? [])],
  web: a?.web ?? "off",
  allow_cloud: a?.allow_cloud ?? false,
});
const error = ref("");
const saving = ref(false);
const confirming = ref(false);
const prompt = ref(a?.system_prompt ?? "");

const options = computed(modelOptions);
/** 原先指定的模型现在不在可选列表里时，仍然保留为一个选项，避免打开配置就把它改掉。 */
const missing = computed(() => (a?.model && form.model && !options.value.some((o) => `${o.providerId}\n${o.model}` === form.model) ? a.model : ""));

function input(): AgentInput {
  const [provider_id, model] = form.model ? form.model.split("\n") : [null, null];
  return {
    name: form.name,
    description: form.description,
    duty: form.duty,
    out_of_scope: form.out_of_scope,
    instructions: form.instructions,
    opening: form.opening,
    examples: form.examples.split("\n").map((l) => l.trim()).filter(Boolean),
    provider_id,
    model,
    web: form.web,
    allow_cloud: form.allow_cloud,
    // 已被删除的文件夹不再保留
    folders: state.foldersLoaded ? form.folders.filter((id) => state.folders.some((f) => f.id === id)) : form.folders,
  };
}

onMounted(() => {
  if (!state.providersLoaded) void loadProviders().catch(() => {});
  void loadFolders().catch(() => {});
});

// 编辑时稍停一下再刷新提示词预览
let timer = 0;
let pending: AbortController | null = null;
watch(
  () => [form.name, form.description, form.duty, form.out_of_scope, form.instructions],
  () => {
    clearTimeout(timer);
    timer = window.setTimeout(async () => {
      pending?.abort();
      pending = new AbortController();
      try {
        prompt.value = (await api.previewPrompt(input(), pending.signal)).system_prompt;
      } catch {
        /* 预览失败不影响编辑 */
      }
    }, 350);
  },
  { immediate: !a },
);

async function save() {
  saving.value = true;
  error.value = "";
  try {
    emit("saved", a ? await api.updateAgent(a.id, input()) : await api.createAgent(input()));
  } catch (e) {
    error.value = errorText(e);
  } finally {
    saving.value = false;
  }
}

async function remove() {
  if (!a) return;
  if (!confirming.value) {
    confirming.value = true;
    setTimeout(() => (confirming.value = false), 4000);
    return;
  }
  try {
    await api.deleteAgent(a.id);
    emit("deleted");
  } catch (e) {
    error.value = errorText(e);
  }
}
</script>

<template>
  <div class="mask" @click="emit('close')" />
  <form class="drawer form" @submit.prevent="save" @keydown.esc="emit('close')">
    <div class="hd2">
      <h2 class="grow">{{ a ? $t("agents.configTitle", { name: a.name }) : $t("agents.createTitle") }}</h2>
      <button type="button" class="btn ghost sm" @click="emit('close')">{{ $t("common.cancel") }}</button>
      <button type="submit" class="btn primary sm" :disabled="saving || !form.name.trim()">{{ $t("common.save") }}</button>
    </div>
    <div v-if="error" class="alert err" role="alert">{{ error }}</div>
    <label class="field"><span>{{ $t("agents.name") }}</span><input v-model="form.name" type="text" required maxlength="40" /></label>
    <label class="field"><span>{{ $t("agents.description") }}</span><input v-model="form.description" type="text" maxlength="200" /></label>
    <label class="field"><span>{{ $t("agents.duty") }}</span><textarea v-model="form.duty" rows="3" maxlength="4000" /><span>{{ $t("agents.dutyHint") }}</span></label>
    <label class="field"><span>{{ $t("agents.outOfScope") }}</span><textarea v-model="form.out_of_scope" rows="2" maxlength="4000" /><span>{{ $t("agents.outOfScopeHint") }}</span></label>
    <label class="field"><span>{{ $t("agents.instructions") }}</span><textarea v-model="form.instructions" rows="2" maxlength="4000" /><span>{{ $t("agents.instructionsHint") }}</span></label>
    <label class="field"><span>{{ $t("agents.opening") }}</span><textarea v-model="form.opening" rows="2" maxlength="1000" /><span>{{ $t("agents.openingHint") }}</span></label>
    <label class="field"><span>{{ $t("agents.examples") }}</span><textarea v-model="form.examples" rows="3" /><span>{{ $t("agents.examplesHint") }}</span></label>
    <div class="field">
      <span>{{ $t("kb.agentFolders") }}</span>
      <FolderPicker v-model="form.folders" />
      <span>{{ $t("kb.agentFoldersHint") }}</span>
    </div>
    <div class="field">
      <span>{{ $t("web.agentMode") }}</span>
      <div class="seg">
        <button v-for="m in ['off', 'fallback', 'always'] as const" :key="m" type="button" :aria-pressed="form.web === m" @click="form.web = m">{{ $t(`web.mode${m[0].toUpperCase()}${m.slice(1)}`) }}</button>
      </div>
      <span>{{ $t("web.modeHint") }}</span>
    </div>
    <label class="field">
      <span>{{ $t("agents.model") }}</span>
      <select v-model="form.model">
        <option value="">{{ $t("agents.modelFollow") }}</option>
        <option v-if="missing" :value="form.model">{{ $t("agents.modelMissing", { model: missing }) }}</option>
        <option v-for="o in options" :key="`${o.providerId}\n${o.model}`" :value="`${o.providerId}\n${o.model}`">{{ o.label }}</option>
      </select>
    </label>
    <div class="field">
      <label class="row"><input v-model="form.allow_cloud" type="checkbox" />{{ $t("web.allowCloud") }}</label>
      <span>{{ $t("web.allowCloudHint") }}</span>
    </div>
    <details>
      <summary>{{ $t("agents.prompt") }}</summary>
      <pre>{{ prompt }}</pre>
      <p class="muted" style="margin-top: 6px">{{ $t("agents.promptHint") }}</p>
    </details>
    <div v-if="a" class="row" style="margin-top: auto; padding-top: 10px; border-top: 1px solid var(--line)">
      <button type="button" class="btn sm" :class="confirming ? 'danger' : 'ghost'" @click="remove">{{ confirming ? $t("agents.deleteConfirm") : $t("agents.delete") }}</button>
    </div>
  </form>
</template>
