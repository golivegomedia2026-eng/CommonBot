<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useRouter } from "vue-router";

import { type Agent, type AgentSummary, api } from "../api";
import AgentConfig from "../components/AgentConfig.vue";
import Icon from "../components/Icon.vue";
import { errorText } from "../i18n";

const router = useRouter();
const agents = ref<AgentSummary[]>([]);
const loaded = ref(false);
const error = ref("");
const creating = ref(false);

onMounted(async () => {
  try {
    agents.value = await api.agents();
  } catch (e) {
    error.value = errorText(e);
  } finally {
    loaded.value = true;
  }
});

function created(a: Agent) {
  void router.push(`/agents/${a.id}`);
}
</script>

<template>
  <header class="page-head">
    <div class="grow">
      <div class="eyebrow">{{ $t("nav.workbench") }}</div>
      <h1>{{ $t("agents.title") }}</h1>
    </div>
    <button class="btn primary" @click="creating = true"><Icon name="plus" />{{ $t("agents.new") }}</button>
  </header>
  <div v-if="error" class="alert err" role="alert">{{ error }}</div>
  <div v-if="loaded && !agents.length && !error" class="card empty">
    <h2>{{ $t("agents.empty") }}</h2>
    <p>{{ $t("agents.emptyHint") }}</p>
  </div>
  <div class="agents">
    <RouterLink v-for="a in agents" :key="a.id" class="card agent" :to="`/agents/${a.id}`">
      <div class="top">
        <span class="seal" aria-hidden="true">{{ [...a.name][0] }}</span>
        <div>
          <h2 :title="a.name">{{ a.name }}</h2>
          <span class="muted">{{ a.conversations ? $t("agents.uses", { n: a.conversations }) : $t("agents.unused") }}</span>
        </div>
      </div>
      <p v-if="a.description || a.duty">{{ a.description || a.duty }}</p>
      <div v-if="a.model" class="row"><span class="tag">{{ a.model }}</span></div>
    </RouterLink>
  </div>
  <AgentConfig v-if="creating" :agent="null" @close="creating = false" @saved="created" />
</template>
