<script setup lang="ts">
// 已发布的智能体：链接、API 密钥、接口地址。
import { computed, ref } from "vue";

import type { Agent } from "../api";
import { copyText } from "../util";

const props = defineProps<{ agent: Agent }>();
const copied = ref("");
const link = computed(() => `${location.origin}/a/${props.agent.public_token}`);
const apiBase = computed(() => `${location.origin}/v1`);
/** 用本机地址打开界面时，提醒这个链接别的设备打不开。 */
const localOnly = computed(() => ["localhost", "127.0.0.1", "[::1]"].includes(location.hostname));

async function copy(what: string, text: string) {
  if (await copyText(text)) {
    copied.value = what;
    setTimeout(() => copied.value === what && (copied.value = ""), 1500);
  }
}
</script>

<template>
  <div class="field">
    <span>{{ $t("pub.link") }}</span>
    <div class="kv"><span>{{ link }}</span><button type="button" class="btn ghost sm" @click="copy('link', link)">{{ copied === "link" ? $t("common.copied") : $t("common.copy") }}</button><a class="btn ghost sm" :href="link" target="_blank" rel="noopener">↗</a></div>
  </div>
  <div class="field" style="margin-top: 10px">
    <span>{{ $t("pub.key") }}</span>
    <div class="kv"><span>{{ agent.api_key }}</span><button type="button" class="btn ghost sm" @click="copy('key', agent.api_key ?? '')">{{ copied === "key" ? $t("common.copied") : $t("common.copy") }}</button><slot name="key-actions" /></div>
  </div>
  <div class="field" style="margin-top: 10px">
    <span>{{ $t("pub.api") }}</span>
    <div class="kv"><span>{{ apiBase }}</span><button type="button" class="btn ghost sm" @click="copy('api', apiBase)">{{ copied === "api" ? $t("common.copied") : $t("common.copy") }}</button></div>
  </div>
  <p v-if="localOnly" class="muted" style="margin-top: 8px">{{ $t("pub.lanHint") }}</p>
</template>
