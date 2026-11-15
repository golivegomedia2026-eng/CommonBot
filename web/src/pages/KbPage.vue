<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";

import { type AgentSummary, api, type Hit, kb, type KbDocument, type KbSettings, type KbStats, type Locator } from "../api";
import Icon from "../components/Icon.vue";
import { errorCodeText, errorText, locText } from "../i18n";
import { folderPath, folderTotal, folderTree, loadFolders, state } from "../state";

const route = useRoute();
const router = useRouter();

const error = ref("");
const settings = ref<KbSettings | null>(null);
const agents = ref<AgentSummary[]>([]);
const documents = ref<KbDocument[]>([]);
const stats = ref<KbStats | null>(null);
const rejected = ref<{ name: string; code: string }[]>([]);
const uploading = ref(false);
const dragOver = ref(false);
const picker = ref<HTMLInputElement>();
const confirming = ref("");
/** 正在输入名称：新建最上层 / 新建子文件夹 / 重命名 */
const naming = ref<"" | "top" | "sub" | "rename">("");
const nameDraft = ref("");
const question = ref("");
const viewing = ref<KbDocument | null>(null);
const chunks = ref<{ seq: number; locator: Locator; text: string }[]>([]);
const chunksDone = ref(false);
const searching = ref(false);
const query = ref("");
const hits = ref<Hit[] | null>(null);
const keywordOnly = ref(false);

const tree = computed(folderTree);
const folderId = computed(() => (route.params.folderId as string) || "");
const folder = computed(() => state.folders.find((f) => f.id === folderId.value));
const path = computed(() => (folder.value ? folderPath(folder.value.id) : ""));
/** 文件夹里提问时由谁回答：通用助手优先，否则第一个智能体。 */
const answerer = computed(() => agents.value.find((a) => a.id === "general") ?? agents.value[0]);
const busyDocs = computed(() => documents.value.some((d) => ["queued", "parsing", "ocr", "embedding"].includes(d.status)));

let timer = 0;
async function loadDocuments() {
  if (!folderId.value) return;
  try {
    const r = await kb.documents(folderId.value, true);
    documents.value = r.documents;
    stats.value = r.stats;
  } catch (e) {
    error.value = errorText(e);
  }
}
function schedule() {
  clearTimeout(timer);
  // 有文档在处理时定时刷新进度
  if (busyDocs.value) timer = window.setTimeout(async () => { await loadDocuments(); schedule(); }, 1500);
}
watch(busyDocs, schedule);

async function refresh() {
  error.value = "";
  try {
    await loadFolders();
    if (!folderId.value && tree.value.length) return void router.replace(`/kb/${tree.value[0].folder.id}`);
    if (folderId.value && !folder.value) return void router.replace("/kb");
    await loadDocuments();
  } catch (e) {
    error.value = errorText(e);
  }
}
watch(folderId, () => {
  documents.value = [];
  stats.value = null;
  rejected.value = [];
  naming.value = "";
  void refresh();
});
onMounted(async () => {
  await refresh();
  kb.settings().then((s) => (settings.value = s)).catch(() => {});
  api.agents().then((a) => (agents.value = a)).catch(() => {});
});
onBeforeUnmount(() => clearTimeout(timer));

function startNaming(mode: "top" | "sub" | "rename") {
  naming.value = mode;
  nameDraft.value = mode === "rename" ? (folder.value?.name ?? "") : "";
}
async function submitName() {
  error.value = "";
  try {
    if (naming.value === "rename") {
      await kb.renameFolder(folderId.value, nameDraft.value);
      await loadFolders();
    } else {
      const { id } = await kb.createFolder(nameDraft.value, naming.value === "sub" ? folderId.value : null);
      await loadFolders();
      await router.push(`/kb/${id}`);
    }
    naming.value = "";
  } catch (e) {
    error.value = errorText(e);
  }
}

function confirmThen(key: string, action: () => Promise<void>) {
  if (confirming.value !== key) {
    confirming.value = key;
    setTimeout(() => confirming.value === key && (confirming.value = ""), 3500);
    return;
  }
  confirming.value = "";
  action().catch((e) => (error.value = errorText(e)));
}
const removeFolder = () =>
  confirmThen("folder", async () => {
    const parent = folder.value?.parent_id;
    await kb.deleteFolder(folderId.value);
    await loadFolders();
    await router.replace(parent ? `/kb/${parent}` : "/kb");
  });
const removeDoc = (d: KbDocument) =>
  confirmThen(d.id, async () => {
    await kb.deleteDocument(d.id);
    await Promise.all([loadDocuments(), loadFolders()]);
  });

async function upload(files: FileList | File[] | null | undefined) {
  const list = Array.from(files ?? []);
  if (!list.length || !folderId.value) return;
  uploading.value = true;
  error.value = "";
  try {
    const r = await kb.upload(folderId.value, list);
    rejected.value = r.rejected;
    await Promise.all([loadDocuments(), loadFolders()]);
    schedule();
  } catch (e) {
    error.value = errorText(e);
  } finally {
    uploading.value = false;
    if (picker.value) picker.value.value = "";
  }
}

async function act(d: KbDocument, action: "retry" | "pause" | "resume") {
  try {
    await kb.documentAction(d.id, action);
    await loadDocuments();
    schedule();
  } catch (e) {
    error.value = errorText(e);
  }
}

async function view(d: KbDocument) {
  viewing.value = d;
  chunks.value = [];
  chunksDone.value = false;
  await moreChunks();
}
async function moreChunks() {
  if (!viewing.value) return;
  try {
    const r = await kb.chunks(viewing.value.id, chunks.value.length);
    chunks.value.push(...r.chunks);
    chunksDone.value = r.chunks.length < 50;
  } catch (e) {
    error.value = errorText(e);
  }
}

async function runSearch() {
  if (!query.value.trim()) return;
  try {
    const r = await kb.search(query.value, [folderId.value]);
    hits.value = r.hits;
    keywordOnly.value = !r.semantic;
  } catch (e) {
    error.value = errorText(e);
  }
}

function ask() {
  const q = question.value.trim();
  if (!q || !answerer.value) return;
  void router.push({ path: `/agents/${answerer.value.id}`, query: { scope: folderId.value, q } });
}

function statusText(d: KbDocument): string {
  const key = { queued: "stQueued", parsing: "stParsing", ocr: "stOcr", embedding: "stEmbedding", ready: "stReady", failed: "stFailed", paused: "stPaused" }[d.status];
  return `kb.${key}`;
}
</script>

<template>
  <header class="page-head">
    <div class="grow">
      <div class="eyebrow">{{ $t("nav.workbench") }}</div>
      <h1>{{ $t("kb.title") }}</h1>
    </div>
    <button class="btn ghost" @click="startNaming('top')"><Icon name="plus" />{{ $t("kb.newTop") }}</button>
    <button class="btn primary" :disabled="!folder || uploading" @click="picker?.click()"><Icon name="clip" />{{ uploading ? $t("kb.uploading") : $t("kb.upload") }}</button>
    <input ref="picker" type="file" multiple hidden @change="upload(($event.target as HTMLInputElement).files)" />
  </header>

  <div v-if="error" class="alert err" role="alert">{{ error }}</div>
  <div v-if="settings && !settings.embed" class="alert info"><span class="grow">{{ $t("kb.bannerNoEmbed") }}</span><RouterLink class="btn ghost sm" to="/settings/models">{{ $t("kb.goSettings") }}</RouterLink></div>
  <div v-if="settings && !settings.pdf_engine" class="alert warn">{{ $t("kb.bannerNoPdf") }}</div>

  <form v-if="naming === 'top'" class="card inline-form" @submit.prevent="submitName">
    <input v-model="nameDraft" v-focus type="text" maxlength="60" required :placeholder="$t('kb.folderName')" :aria-label="$t('kb.folderName')" />
    <button class="btn primary sm" type="submit">{{ $t("kb.create") }}</button>
    <button class="btn ghost sm" type="button" @click="naming = ''">{{ $t("common.cancel") }}</button>
  </form>

  <div v-if="state.foldersLoaded && !tree.length && naming !== 'top'" class="card empty">
    <h2>{{ $t("kb.empty") }}</h2>
    <p>{{ $t("kb.emptyHint") }}</p>
  </div>

  <div v-if="tree.length" class="kbpage">
    <nav class="card tree" :aria-label="$t('kb.title')">
      <RouterLink v-for="t in tree" :key="t.folder.id" class="node" :class="{ on: t.folder.id === folderId }" :style="{ paddingLeft: `${8 + t.depth * 18}px` }" :to="`/kb/${t.folder.id}`">
        <Icon name="folder" /><span :title="t.folder.name">{{ t.folder.name }}</span><i>{{ folderTotal(t.folder.id) }}</i>
      </RouterLink>
    </nav>

    <div v-if="folder" class="kbmain" @dragover.prevent="dragOver = true" @dragleave="dragOver = false" @drop.prevent="dragOver = false; upload($event.dataTransfer?.files)">
      <div class="row">
        <h2 class="grow" style="overflow-wrap: anywhere">{{ path }}</h2>
        <button class="btn ghost sm" @click="startNaming('sub')"><Icon name="plus" />{{ $t("kb.newFolder") }}</button>
        <button class="btn ghost sm" @click="startNaming('rename')">{{ $t("kb.rename") }}</button>
        <button class="btn ghost sm" @click="searching = true; hits = null"><Icon name="search" />{{ $t("kb.search") }}</button>
        <button class="btn sm" :class="confirming === 'folder' ? 'danger' : 'ghost'" @click="removeFolder">{{ confirming === "folder" ? $t("kb.deleteFolderConfirm") : $t("kb.deleteFolder") }}</button>
      </div>
      <form v-if="naming === 'sub' || naming === 'rename'" class="card inline-form" @submit.prevent="submitName">
        <input v-model="nameDraft" v-focus type="text" maxlength="60" required :placeholder="$t('kb.folderName')" :aria-label="$t('kb.folderName')" />
        <button class="btn primary sm" type="submit">{{ naming === "rename" ? $t("common.save") : $t("kb.create") }}</button>
        <button class="btn ghost sm" type="button" @click="naming = ''">{{ $t("common.cancel") }}</button>
      </form>

      <form class="card ask" @submit.prevent="ask">
        <div class="row"><h2>{{ $t("kb.askHere") }}</h2><span class="tag info" style="white-space: normal">{{ $t("kb.askScope", { path }) }}</span></div>
        <div class="composer">
          <textarea v-model="question" rows="2" :placeholder="$t('kb.askPlaceholder')" :aria-label="$t('kb.askHere')" @keydown.enter.exact.prevent="ask" />
          <div class="tools">
            <span class="muted">{{ answerer ? $t("kb.askVia", { agent: answerer.name }) : $t("kb.noAgent") }}</span>
            <button class="btn primary sm send" type="submit" :disabled="!question.trim() || !answerer"><Icon name="send" />{{ $t("chat.send") }}</button>
          </div>
        </div>
      </form>

      <div v-if="stats" class="kpis">
        <div class="card"><span class="muted">{{ $t("kb.docs") }}</span><div class="num">{{ stats.documents.toLocaleString() }}</div></div>
        <div class="card"><span class="muted">{{ $t("kb.chunks") }}</span><div class="num">{{ stats.chunks.toLocaleString() }}</div></div>
        <div class="card"><span class="muted">{{ $t("kb.processing") }}</span><div class="num">{{ stats.queued.toLocaleString() }}</div></div>
        <div class="card" :style="stats.failed ? { borderColor: 'var(--point)' } : {}"><span class="muted">{{ $t("kb.failedCount") }}</span><div class="num">{{ stats.failed.toLocaleString() }}</div></div>
      </div>

      <div v-if="rejected.length" class="alert warn" style="flex-direction: column; align-items: flex-start">
        <span v-for="r in rejected" :key="r.name">{{ $t("kb.rejected", { name: r.name, reason: errorCodeText(r.code) }) }}</span>
      </div>

      <div v-if="documents.length" class="tbl">
        <table>
          <thead><tr><th>{{ $t("kb.colName") }}</th><th class="n">{{ $t("kb.colPages") }}</th><th class="n">{{ $t("kb.chunks") }}</th><th>{{ $t("kb.colStatus") }}</th><th /></tr></thead>
          <tbody>
            <tr v-for="d in documents" :key="d.id">
              <td class="name">{{ d.name }}<div v-if="d.folder_id !== folderId" class="muted">{{ folderPath(d.folder_id) }}</div></td>
              <td class="n">{{ d.pages ?? "—" }}</td>
              <td class="n">{{ d.status === "ready" ? d.chunks.toLocaleString() : "—" }}</td>
              <td class="st">
                <template v-if="d.status === 'ocr' || d.status === 'embedding'"><span class="bar"><i :style="{ width: `${d.total ? (d.done / d.total) * 100 : 0}%` }" /></span>{{ $t(statusText(d), { done: d.done, total: d.total }) }}</template>
                <span v-else-if="d.status === 'ready'" class="tag ok">✓ {{ $t("kb.stReady") }}</span>
                <span v-else-if="d.status === 'failed'" class="tag err">× {{ errorCodeText(d.error_code ?? "http", d.error_detail ?? "") }}</span>
                <span v-else class="tag">{{ $t(statusText(d)) }}</span>
                <span v-if="d.status === 'ready' && !d.embed_model" class="tag" style="margin-left: 4px">{{ $t("kb.keywordOnly") }}</span>
                <span v-if="d.ocr_skipped" class="tag warn" style="margin-left: 4px">! {{ $t("kb.ocrSkipped", { n: d.ocr_skipped }) }}</span>
                <span v-else-if="d.ocr_pages && d.status === 'ready'" class="muted" style="margin-left: 6px">{{ $t("kb.ocrPages", { n: d.ocr_pages }) }}</span>
              </td>
              <td>
                <div class="row" style="flex-wrap: nowrap; justify-content: flex-end">
                  <button v-if="d.status === 'ready'" class="btn ghost sm" @click="view(d)">{{ $t("kb.view") }}</button>
                  <button v-if="['queued', 'parsing', 'ocr', 'embedding'].includes(d.status)" class="btn ghost sm" @click="act(d, 'pause')">{{ $t("kb.pause") }}</button>
                  <button v-if="d.status === 'paused'" class="btn ghost sm" @click="act(d, 'resume')">{{ $t("kb.resume") }}</button>
                  <button v-if="d.status === 'failed'" class="btn ghost sm" @click="act(d, 'retry')">{{ $t("kb.retry") }}</button>
                  <button class="btn sm" :class="confirming === d.id ? 'danger' : 'ghost'" @click="removeDoc(d)">{{ confirming === d.id ? $t("common.confirmDelete") : $t("common.delete") }}</button>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="drop" :class="{ over: dragOver }">
        <template v-if="!documents.length">{{ $t("kb.folderEmpty") }}<br /></template>{{ $t("kb.dropHint") }}<br /><span class="muted">{{ $t("kb.supported") }}</span>
      </div>
    </div>
  </div>

  <template v-if="viewing">
    <div class="mask" @click="viewing = null" />
    <aside class="drawer" @keydown.esc="viewing = null">
      <div class="hd2"><h2 class="grow" style="overflow-wrap: anywhere">{{ $t("kb.chunksTitle", { name: viewing.name }) }}</h2><button class="btn ghost sm" @click="viewing = null">{{ $t("common.close") }}</button></div>
      <div v-for="c in chunks" :key="c.seq" class="hit"><span class="tag">{{ c.seq + 1 }}</span> <span class="muted">{{ locText(c.locator) }}</span><p>{{ c.text }}</p></div>
      <button v-if="!chunksDone" class="btn ghost sm" @click="moreChunks">{{ $t("kb.more") }}</button>
    </aside>
  </template>

  <template v-if="searching">
    <div class="mask" @click="searching = false" />
    <aside class="drawer" @keydown.esc="searching = false">
      <div class="hd2"><h2 class="grow">{{ $t("kb.search") }}</h2><button class="btn ghost sm" @click="searching = false">{{ $t("common.close") }}</button></div>
      <span class="tag info" style="white-space: normal; align-self: flex-start">{{ $t("kb.askScope", { path }) }}</span>
      <form class="inline-form" @submit.prevent="runSearch">
        <input v-model="query" v-focus type="text" :placeholder="$t('kb.searchPlaceholder')" :aria-label="$t('kb.search')" />
        <button class="btn primary sm" type="submit" :disabled="!query.trim()"><Icon name="search" /></button>
      </form>
      <template v-if="hits">
        <div v-if="keywordOnly" class="muted">{{ $t("kb.searchKeyword") }}</div>
        <div v-if="!hits.length" class="muted">{{ $t("kb.searchEmpty") }}</div>
        <div v-for="(h, i) in hits" :key="h.chunk_id" class="hit"><span class="cite">{{ i + 1 }}</span> <b>{{ h.document }}</b> <span class="muted">{{ locText(h.locator) }} · {{ h.folder }}</span><p>{{ h.text }}</p></div>
      </template>
    </aside>
  </template>
</template>
