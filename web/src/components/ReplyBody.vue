<script setup lang="ts">
// 一条回答：思考过程、正文（出处编号可点）、出处清单、状态标记、复制。
import { ref } from "vue";

import type { Message, Source } from "../api";
import { errorCodeText, locText } from "../i18n";
import { renderMarkdown } from "../md";
import { copyText, linkCitations } from "../util";
import Icon from "./Icon.vue";

const props = defineProps<{
  message: Pick<Message, "content" | "reasoning" | "sources" | "status" | "error_code" | "cloud" | "model">;
  /** 还在生成中：显示光标，不显示复制等。 */
  live?: boolean;
  /** 这次没联上网的说明（文案的键）。 */
  webNote?: string;
}>();

const open = ref(0);
const copied = ref(false);

function citeClick(e: MouseEvent) {
  const n = (e.target as HTMLElement).closest<HTMLElement>("[data-cite]")?.dataset.cite;
  if (n) open.value = open.value === Number(n) ? 0 : Number(n);
}
async function copy() {
  if (await copyText(props.message.content)) {
    copied.value = true;
    setTimeout(() => (copied.value = false), 1500);
  }
}
const label = (s: Source) => (s.kind === "kb" ? `${s.document} · ${[locText(s.locator), s.folder].filter(Boolean).join(" · ")}` : s.kind === "file" ? s.document : s.title || s.url);
</script>

<template>
  <details v-if="message.reasoning" :open="live && !message.content"><summary>{{ $t("chat.thinking") }}</summary><div>{{ message.reasoning }}</div></details>
  <div v-if="message.content" class="md" @click="citeClick" v-html="linkCitations(renderMarkdown(message.content), message.sources.length)" />
  <span v-if="live" class="caret" :title="$t('chat.generating')" />
  <div v-if="message.sources.length && !live" class="srcs">
    <template v-for="kind in ['kb', 'file', 'web'] as const" :key="kind">
      <template v-if="message.sources.some((s) => s.kind === kind)">
        <b>{{ $t(kind === "kb" ? "kb.sources" : kind === "file" ? "web.files" : "web.sources") }}</b>
        <template v-for="s in message.sources.filter((s) => s.kind === kind)" :key="s.n">
          <button class="src" :aria-expanded="open === s.n" @click="open = open === s.n ? 0 : s.n">
            <span class="cite" :class="{ on: open === s.n }">{{ s.n }}</span><span>{{ label(s) }}</span>
          </button>
          <blockquote v-if="open === s.n">{{ s.text }}<template v-if="s.kind === 'web'"><br /><a :href="s.url" target="_blank" rel="noopener noreferrer">{{ s.url }}</a></template></blockquote>
        </template>
      </template>
    </template>
  </div>
  <div v-if="webNote" class="muted" style="margin-top: 6px">{{ $t(webNote) }}</div>
  <div v-if="message.status === 'error'" class="alert err">{{ errorCodeText(message.error_code ?? "http") }}</div>
  <div v-if="!live" class="foot">
    <span v-if="message.status === 'stopped'" class="tag warn">{{ $t("chat.stopped") }}</span>
    <span v-if="message.cloud" class="tag info">{{ $t("web.cloudTag", { model: message.model }) }}</span>
    <button v-if="message.content" class="btn ghost sm" @click="copy"><Icon name="copy" />{{ copied ? $t("common.copied") : $t("common.copy") }}</button>
  </div>
</template>
