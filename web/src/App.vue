<script setup lang="ts">
import { onMounted, ref } from "vue";
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
    state.health = await api.health();
    if (!state.health.authed) await router.replace({ name: "access" });
  } catch (e) {
    failed.value = errorText(e);
  }
}
onMounted(boot);
</script>

<template>
  <div v-if="failed" class="center">
    <div class="card">
      <div class="alert err">{{ failed }}</div>
      <button class="btn primary" @click="boot">{{ $t("common.retry") }}</button>
    </div>
  </div>
  <RouterView v-else-if="route.meta.bare" />
  <div v-else-if="state.health?.authed" class="shell">
    <nav class="side" :aria-label="$t('nav.workbench')">
      <div class="brand"><span class="logo">智</span><b>CommonBot</b></div>
      <span class="group">{{ $t("nav.workbench") }}</span>
      <RouterLink class="link" :class="{ on: route.name === 'chat' }" to="/chat">{{ $t("nav.chat") }}</RouterLink>
      <span class="group">{{ $t("nav.settings") }}</span>
      <RouterLink class="link" active-class="on" to="/settings/models">{{ $t("nav.models") }}</RouterLink>
      <RouterLink class="link" active-class="on" to="/settings/system">{{ $t("nav.system") }}</RouterLink>
      <div class="host">
        <b><span class="dot" />{{ state.health.local ? $t("nav.thisHost") : $t("nav.remote") }}</b>
        <template v-if="state.health.local">{{ state.health.lan ? $t("nav.lanOn") : $t("nav.lanOff") }}</template>
        <template v-else>CommonBot {{ state.health.version }}</template>
      </div>
    </nav>
    <main><RouterView /></main>
    <nav class="tabbar" :aria-label="$t('nav.workbench')">
      <RouterLink class="link" :class="{ on: route.name === 'chat' }" to="/chat">{{ $t("nav.chat") }}</RouterLink>
      <RouterLink class="link" active-class="on" to="/settings/models">{{ $t("nav.models") }}</RouterLink>
      <RouterLink class="link" active-class="on" to="/settings/system">{{ $t("nav.system") }}</RouterLink>
    </nav>
  </div>
</template>
