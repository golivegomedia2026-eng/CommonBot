<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { useI18n } from "vue-i18n";

import { api, kb, type ModelRef, type Provider, type ProviderInput } from "../api";
import Icon from "../components/Icon.vue";
import { errorText } from "../i18n";
import { checkProvider, loadProviders, modelOptions, state } from "../state";

const { t } = useI18n();
const error = ref("");
const formError = ref("");
const saving = ref(false);
const confirming = ref("");
/** null：没在编辑；""：新增；其他：正在编辑的服务 */
const editing = ref<string | null>(null);
const form = reactive({ name: "", kind: "local" as "local" | "cloud", base_url: "", api_key: "", clear_key: false, default_model: "", has_key: false });

onMounted(() => loadProviders().catch((e) => (error.value = errorText(e))));

// 知识库用的两个模型
const options = computed(modelOptions);
const kbPick = reactive({ embed: "", ocr: "" });
const kbSaved = ref(false);
const key = (m: ModelRef | null) => (m ? `${m.provider_id}\n${m.model}` : "");
const unkey = (v: string): ModelRef | null => (v ? { provider_id: v.split("\n")[0], model: v.split("\n")[1] } : null);
onMounted(async () => {
  try {
    const s = await kb.settings();
    Object.assign(kbPick, { embed: key(s.embed), ocr: key(s.ocr) });
  } catch (e) {
    error.value = errorText(e);
  }
});
async function saveKb() {
  kbSaved.value = false;
  try {
    await kb.saveSettings({ embed: unkey(kbPick.embed), ocr: unkey(kbPick.ocr) });
    kbSaved.value = true;
    setTimeout(() => (kbSaved.value = false), 2500);
  } catch (e) {
    error.value = errorText(e);
  }
}
// 联网搜索
const webForm = reactive({ kind: "none", base_url: "", api_key: "", fetch_pages: false, has_key: false });
const webMsg = ref<{ ok: boolean; text: string } | null>(null);
const webBusy = ref(false);
onMounted(async () => {
  try {
    Object.assign(webForm, await api.webSettings(), { api_key: "" });
  } catch (e) {
    error.value = errorText(e);
  }
});
async function saveWeb(): Promise<boolean> {
  webMsg.value = null;
  try {
    const saved = await api.saveWebSettings({ kind: webForm.kind, base_url: webForm.base_url, fetch_pages: webForm.fetch_pages, ...(webForm.api_key ? { api_key: webForm.api_key } : {}) });
    Object.assign(webForm, saved, { api_key: "" });
    return true;
  } catch (e) {
    webMsg.value = { ok: false, text: errorText(e) };
    return false;
  }
}
async function testWeb() {
  webBusy.value = true;
  try {
    if (!(await saveWeb())) return;
    const r = await api.testWeb(t("web.testQuery"));
    webMsg.value = { ok: true, text: t("web.testOk", { n: r.results.length }) };
  } catch (e) {
    webMsg.value = { ok: false, text: errorText(e) };
  } finally {
    webBusy.value = false;
  }
}

/** 已保存的选择不在当前可选列表里时也要能显示出来。 */
const extra = (v: string) => (v && !options.value.some((o) => `${o.providerId}\n${o.model}` === v) ? v : "");

function startAdd() {
  Object.assign(form, { name: "", kind: "local", base_url: "http://127.0.0.1:11434/v1", api_key: "", clear_key: false, default_model: "", has_key: false });
  formError.value = "";
  editing.value = "";
}
function startEdit(p: Provider) {
  Object.assign(form, { name: p.name, kind: p.kind, base_url: p.base_url, api_key: "", clear_key: false, default_model: p.default_model ?? "", has_key: p.has_key });
  formError.value = "";
  editing.value = p.id;
}

async function save() {
  if (editing.value === null) return;
  saving.value = true;
  formError.value = "";
  const input: ProviderInput = { name: form.name, kind: form.kind, base_url: form.base_url, default_model: form.default_model || null };
  if (form.clear_key) input.api_key = "";
  else if (form.api_key) input.api_key = form.api_key;
  try {
    const saved = editing.value ? await api.updateProvider(editing.value, input) : await api.createProvider(input);
    editing.value = null;
    await loadProviders(false);
    await checkProvider(saved.id);
  } catch (e) {
    formError.value = errorText(e);
  } finally {
    saving.value = false;
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
    await api.deleteProvider(id);
    await loadProviders(false);
  } catch (e) {
    error.value = errorText(e);
  }
}
</script>

<template>
  <header class="page-head">
    <div class="grow">
      <div class="eyebrow">{{ $t("nav.settings") }}</div>
      <h1>{{ $t("models.title") }}</h1>
    </div>
    <button class="btn primary" @click="startAdd"><Icon name="plus" />{{ $t("models.add") }}</button>
  </header>
  <p class="muted">{{ $t("models.subtitle") }}</p>
  <div v-if="error" class="alert err" role="alert">{{ error }}</div>

  <div v-if="state.providersLoaded && !state.providers.length" class="card muted">{{ $t("models.emptyList") }}</div>
  <div v-for="p in state.providers" :key="p.id" class="card">
    <div class="row">
      <h2>{{ p.name }}</h2>
      <span class="tag" :class="{ info: p.kind === 'cloud' }">{{ $t(`models.${p.kind}`) }}</span>
      <span v-if="p.has_key" class="tag">{{ $t("models.hasKey") }}</span>
      <span class="grow" />
      <button class="btn ghost sm" :disabled="state.checks[p.id]?.state === 'checking'" @click="checkProvider(p.id)">{{ $t("models.check") }}</button>
      <button class="btn ghost sm" @click="startEdit(p)">{{ $t("common.edit") }}</button>
      <button class="btn sm" :class="confirming === p.id ? 'danger' : 'ghost'" @click="remove(p.id)">{{ confirming === p.id ? $t("common.confirmDelete") : $t("common.delete") }}</button>
    </div>
    <div class="kv" style="margin: 10px 0"><span>{{ p.base_url }}</span></div>
    <div class="row" aria-live="polite">
      <template v-if="state.checks[p.id]?.state === 'ok'">
        <span v-if="state.checks[p.id].models.length" class="tag ok">✓ {{ $t("models.ok", { n: state.checks[p.id].models.length, ms: state.checks[p.id].latency }) }}</span>
        <span v-else class="tag warn">! {{ $t("models.okNoModels") }}</span>
        <span v-if="p.default_model" class="muted">{{ $t("models.defaultModel") }}：{{ p.default_model }}</span>
      </template>
      <span v-else-if="state.checks[p.id]?.state === 'error'" class="tag err">× {{ state.checks[p.id].error }}</span>
      <span v-else-if="state.checks[p.id]?.state === 'checking'" class="tag">{{ $t("models.checking") }}</span>
      <span v-else class="tag">{{ $t("models.notChecked") }}</span>
    </div>
  </div>

  <div class="card form">
    <div class="row"><h2 class="grow">{{ $t("kb.modelsTitle") }}</h2><span v-if="kbSaved" class="tag ok">✓ {{ $t("kb.saved") }}</span></div>
    <label v-for="which in ['embed', 'ocr'] as const" :key="which" class="field">
      <span>{{ $t(`kb.${which}`) }}</span>
      <select v-model="kbPick[which]" @change="saveKb">
        <option value="">{{ $t("kb.notUsed") }}</option>
        <option v-if="extra(kbPick[which])" :value="kbPick[which]">{{ kbPick[which].split("\n")[1] }}</option>
        <option v-for="o in options" :key="`${o.providerId}\n${o.model}`" :value="`${o.providerId}\n${o.model}`">{{ o.label }}</option>
      </select>
      <span>{{ $t(`kb.${which}Hint`) }}</span>
    </label>
  </div>

  <form class="card form" @submit.prevent="testWeb">
    <div><h2>{{ $t("web.title") }}</h2><p class="muted" style="margin-top: 4px">{{ $t("web.subtitle") }}</p></div>
    <label class="field">
      <span>{{ $t("web.kind") }}</span>
      <select v-model="webForm.kind" @change="webForm.kind === 'searxng' && !webForm.base_url ? (webMsg = null) : saveWeb()">
        <option v-for="k in ['none', 'searxng', 'tavily', 'bocha']" :key="k" :value="k">{{ $t(`web.${k}`) }}</option>
      </select>
    </label>
    <label v-if="webForm.kind === 'searxng'" class="field"><span>{{ $t("web.baseUrl") }}</span><input v-model="webForm.base_url" type="text" placeholder="http://127.0.0.1:8888" spellcheck="false" autocapitalize="off" /></label>
    <label v-if="webForm.kind === 'tavily' || webForm.kind === 'bocha'" class="field"><span>{{ $t("models.apiKey") }}</span><input v-model="webForm.api_key" type="password" autocomplete="off" :placeholder="webForm.has_key ? $t('models.apiKeyKeep') : ''" /></label>
    <template v-if="webForm.kind !== 'none'">
      <label class="row"><input v-model="webForm.fetch_pages" type="checkbox" @change="saveWeb" />{{ $t("web.fetchPages") }}</label>
      <div class="row"><button class="btn ghost sm" type="submit" :disabled="webBusy">{{ $t("web.test") }}</button><span v-if="webMsg" class="tag" :class="webMsg.ok ? 'ok' : 'err'">{{ webMsg.ok ? "✓" : "×" }} {{ webMsg.text }}</span></div>
    </template>
  </form>

  <template v-if="editing !== null">
    <div class="mask" @click="editing = null" />
    <form class="drawer form" @submit.prevent="save">
      <div class="hd2">
        <h2 class="grow">{{ editing ? $t("models.editTitle") : $t("models.addTitle") }}</h2>
        <button type="button" class="btn ghost sm" @click="editing = null">{{ $t("common.cancel") }}</button>
        <button type="submit" class="btn primary sm" :disabled="saving">{{ $t("common.save") }}</button>
      </div>
      <div v-if="formError" class="alert err" role="alert">{{ formError }}</div>
      <label class="field"><span>{{ $t("models.name") }}</span><input v-model="form.name" type="text" required maxlength="60" /></label>
      <div class="field">
        <span>{{ $t("models.kind") }}</span>
        <div class="seg">
          <button type="button" :aria-pressed="form.kind === 'local'" @click="form.kind = 'local'">{{ $t("models.local") }}</button>
          <button type="button" :aria-pressed="form.kind === 'cloud'" @click="form.kind = 'cloud'">{{ $t("models.cloud") }}</button>
        </div>
      </div>
      <label class="field"><span>{{ $t("models.baseUrl") }}</span><input v-model="form.base_url" type="text" required spellcheck="false" autocapitalize="off" /><span>{{ $t("models.baseUrlHint") }}</span></label>
      <label class="field">
        <span>{{ $t("models.apiKey") }}</span>
        <input v-model="form.api_key" type="password" autocomplete="off" :disabled="form.clear_key" :placeholder="form.has_key ? $t('models.apiKeyKeep') : $t('models.apiKeyNone')" />
      </label>
      <label v-if="form.has_key" class="row"><input v-model="form.clear_key" type="checkbox" />{{ $t("models.clearKey") }}</label>
      <label class="field">
        <span>{{ $t("models.defaultModel") }}</span>
        <select v-if="editing && state.checks[editing]?.models.length" v-model="form.default_model">
          <option value="">{{ $t("models.none") }}</option>
          <option v-if="form.default_model && !state.checks[editing].models.includes(form.default_model)" :value="form.default_model">{{ form.default_model }}</option>
          <option v-for="m in state.checks[editing].models" :key="m" :value="m">{{ m }}</option>
        </select>
        <input v-else v-model="form.default_model" type="text" spellcheck="false" autocapitalize="off" />
        <span>{{ $t("models.defaultModelHint") }}</span>
      </label>
    </form>
  </template>
</template>
