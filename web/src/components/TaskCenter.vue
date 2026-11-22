<script setup lang="ts">
import { computed } from "vue";
import { useRouter } from "vue-router";

import { api, type ServerTask } from "../api";
import { errorCodeText } from "../i18n";
import { clearFinished, refreshTasks, tasks } from "../tasks";
import Icon from "./Icon.vue";

const router = useRouter();
const empty = computed(() => !tasks.server.length && !tasks.uploads.length);
const hasFinished = computed(() => tasks.server.some((t) => t.status !== "running") || tasks.uploads.some((u) => u.status === "done" || u.status === "failed"));
const tagClass = (status: string) => (status === "done" ? "ok" : status === "error" || status === "failed" ? "err" : status === "stopped" ? "warn" : "info");
const gb = (n: number) => (n / 2 ** 30).toFixed(1);
const percent = (t: ServerTask) => (t.total ? Math.min(100, Math.floor(((t.done ?? 0) / t.total) * 100)) : 0);

function go(path: string) {
  tasks.open = false;
  void router.push(path);
}
async function stop(t: ServerTask) {
  if (!t.conversation_id) return;
  try {
    await api.stopReply(t.conversation_id);
  } catch {
    /* 已经结束了 */
  }
  void refreshTasks();
}
</script>

<template>
  <div class="mask" @click="tasks.open = false" />
  <aside class="drawer" role="dialog" aria-modal="true" :aria-label="$t('tasks.title')" data-test="tasks" @keydown.esc="tasks.open = false">
    <div class="hd2">
      <h2 class="grow">{{ $t("tasks.title") }}</h2>
      <button v-if="hasFinished" class="btn ghost sm" @click="clearFinished">{{ $t("tasks.clear") }}</button>
      <button class="btn ghost sm" @click="tasks.open = false">{{ $t("common.close") }}</button>
    </div>
    <p class="muted">{{ $t("tasks.hint") }}</p>
    <div v-if="empty" class="muted">{{ $t("tasks.empty") }}</div>

    <div v-for="t in tasks.server" :key="t.id" class="task">
      <template v-if="t.kind === 'chat'">
        <div class="row">
          <span class="tag" :class="tagClass(t.status)">{{ $t(`tasks.status.${t.status}`) }}</span>
          <b class="grow ell">{{ t.agent_name }}</b>
          <button v-if="t.status === 'running'" class="btn ghost sm" @click="stop(t)">{{ $t("tasks.stop") }}</button>
          <button class="btn ghost sm" @click="go(`/agents/${t.agent_id}/${t.conversation_id}`)">{{ $t("tasks.open") }}</button>
        </div>
        <div class="muted ell">{{ t.title }}</div>
      </template>
      <template v-else-if="t.kind === 'kb'">
        <div class="row">
          <span class="tag info">{{ $t("tasks.status.running") }}</span>
          <b class="grow">{{ $t("tasks.kb") }}</b>
          <button class="btn ghost sm" @click="go('/kb')">{{ $t("tasks.open") }}</button>
        </div>
        <div class="muted">{{ $t("tasks.kbQueued", { n: t.queued }) }}</div>
      </template>
      <template v-else>
        <div class="row">
          <span class="tag" :class="tagClass(t.status)">{{ $t(`tasks.status.${t.status}`) }}</span>
          <b class="grow">{{ $t("tasks.compute") }}</b>
          <button class="btn ghost sm" @click="go('/settings/models')">{{ $t("tasks.open") }}</button>
        </div>
        <div v-if="t.status === 'running'" class="row" style="gap: 10px">
          <div class="bar grow" style="height: 8px; margin: 0"><i :style="{ width: percent(t) + '%' }"></i></div>
          <span class="muted">{{ gb(t.done ?? 0) }} / {{ gb(t.total ?? 0) }} GB</span>
        </div>
        <div v-else-if="t.error" class="muted">{{ $t(`compute.err.${t.error}`, { detail: t.detail }) }}</div>
      </template>
    </div>

    <template v-if="tasks.uploads.length">
      <h3>{{ $t("tasks.uploads") }}</h3>
      <div v-for="u in tasks.uploads" :key="u.id" class="task">
        <div class="row">
          <span class="tag" :class="u.status === 'done' ? 'ok' : u.status === 'failed' ? 'err' : u.status === 'uploading' ? 'info' : ''">{{ $t(`tasks.upload.${u.status}`) }}</span>
          <span class="grow ell" :title="u.name">{{ u.name }}</span>
          <button class="btn ghost sm icon-only" :aria-label="$t('tasks.open')" :title="u.folderName" @click="go(`/kb/${u.folderId}`)"><Icon name="list" /></button>
        </div>
        <div v-if="u.code" class="muted">{{ errorCodeText(u.code) }}</div>
      </div>
    </template>
  </aside>
</template>
