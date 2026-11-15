<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useRoute, useRouter } from "vue-router";

import { api } from "./api";
import { errorText } from "./i18n";
import { state } from "./state";

const route = useRoute();
const router = useRouter();
const failed = ref("");

async function boot() {
  failed.value = "";
  try {
    await router.isReady();
    state.health = await api.health();
    // 发布出去的独立页面凭链接进入，不需要访问码
    if (!state.health.authed && !route.meta.public) await router.replace({ name: "access" });
    else if (!route.meta.public) void refreshUpdate();
  } catch (e) {
    failed.value = errorText(e);
  }
}
// 升级状态：读的是服务上次验证通过的结果，不访问网络；每小时看一次
const dismissed = ref("");
try {
  dismissed.value = localStorage.getItem("commonbot_update_dismissed") ?? "";
} catch {
  /* 记不住也没关系 */
}
const rechecking = ref(false);
async function refreshUpdate() {
  try {
    state.update = await api.update();
  } catch {
    /* 查不到就当没有 */
  }
}
async function recheck() {
  rechecking.value = true;
  try {
    state.update = await api.checkUpdate();
  } catch {
    /* 同上 */
  } finally {
    rechecking.value = false;
  }
}
function later() {
  dismissed.value = state.update?.latest ?? "";
  try {
    localStorage.setItem("commonbot_update_dismissed", dismissed.value);
  } catch {
    /* 同上 */
  }
}
const mustUpgrade = computed(() => state.update?.state === "required" && !route.meta.bare);
const canUpgrade = computed(() => state.update?.state === "optional" && state.update.latest !== dismissed.value);
let updateTimer = 0;
onMounted(() => {
  void boot();
  updateTimer = window.setInterval(() => state.health?.authed && !route.meta.public && void refreshUpdate(), 3600_000);
});
onBeforeUnmount(() => clearInterval(updateTimer));
</script>

<template>
  <div v-if="failed" class="center">
    <div class="card">
      <div class="alert err">{{ failed }}</div>
      <button class="btn primary" @click="boot">{{ $t("common.retry") }}</button>
    </div>
  </div>
  <RouterView v-else-if="route.meta.bare" />
  <div v-else-if="state.health?.authed" class="shell" :inert="mustUpgrade">
    <nav class="side" :aria-label="$t('nav.workbench')">
      <div class="brand"><span class="logo">智</span><b>CommonBot</b></div>
      <span class="group">{{ $t("nav.workbench") }}</span>
      <RouterLink class="link" :class="{ on: route.name === 'chat' || route.name === 'agents' }" to="/agents">{{ $t("nav.agents") }}</RouterLink>
      <RouterLink class="link" :class="{ on: route.name === 'builder' }" to="/builder">{{ $t("nav.builder") }}</RouterLink>
      <RouterLink class="link" :class="{ on: route.name === 'kb' }" to="/kb">{{ $t("nav.kb") }}</RouterLink>
      <span class="group">{{ $t("nav.settings") }}</span>
      <RouterLink class="link" active-class="on" to="/settings/models">{{ $t("nav.models") }}</RouterLink>
      <RouterLink class="link" active-class="on" to="/settings/system">{{ $t("nav.system") }}</RouterLink>
      <RouterLink class="link" active-class="on" to="/help">{{ $t("nav.help") }}</RouterLink>
      <div class="host">
        <b><span class="dot" />{{ state.health.local ? $t("nav.thisHost") : $t("nav.remote") }}</b>
        <template v-if="state.health.local">{{ state.health.lan ? $t("nav.lanOn") : $t("nav.lanOff") }}</template>
        <template v-else>CommonBot {{ state.health.version }}</template>
      </div>
    </nav>
    <main>
      <div v-if="canUpgrade && state.update" class="alert info" style="flex: none">
        <span class="grow">{{ $t("update.optional", { latest: state.update.latest, current: state.update.current }) }}</span>
        <a v-if="state.update.url" class="btn ghost sm" :href="state.update.url" target="_blank" rel="noopener noreferrer">{{ $t("update.download") }}</a>
        <button class="btn ghost sm" @click="later">{{ $t("update.later") }}</button>
      </div>
      <RouterView />
    </main>
    <nav class="tabbar" :aria-label="$t('nav.workbench')">
      <RouterLink class="link" :class="{ on: route.name === 'chat' || route.name === 'agents' }" to="/agents">{{ $t("nav.agents") }}</RouterLink>
      <RouterLink class="link" :class="{ on: route.name === 'builder' }" to="/builder">{{ $t("nav.builder") }}</RouterLink>
      <RouterLink class="link" :class="{ on: route.name === 'kb' }" to="/kb">{{ $t("nav.kb") }}</RouterLink>
      <RouterLink class="link" active-class="on" to="/settings/models">{{ $t("nav.models") }}</RouterLink>
      <RouterLink class="link" active-class="on" to="/settings/system">{{ $t("nav.system") }}</RouterLink>
    </nav>
  </div>
  <!-- 必须升级：盖住整个界面，没有关闭的办法；只能去下载新版，或重新检查 -->
  <div v-if="mustUpgrade && state.update" class="mask upgrade" role="alertdialog" aria-modal="true" :aria-label="$t('update.requiredTitle')">
    <div class="card">
      <h2>{{ $t("update.requiredTitle") }}</h2>
      <p>{{ $t("update.requiredBody", { current: state.update.current, minimum: state.update.minimum, latest: state.update.latest }) }}</p>
      <div v-if="state.update.notes" class="kv" style="font-family: inherit; white-space: pre-wrap"><span><b>{{ $t("update.notes") }}</b>　{{ state.update.notes }}</span></div>
      <p class="muted">{{ $t("update.how") }}</p>
      <div class="row">
        <a v-if="state.update.url" class="btn primary" :href="state.update.url" target="_blank" rel="noopener noreferrer">{{ $t("update.download") }}</a>
        <button class="btn ghost" :disabled="rechecking" @click="recheck">{{ rechecking ? $t("update.checking") : $t("update.recheck") }}</button>
      </div>
    </div>
  </div>
</template>
