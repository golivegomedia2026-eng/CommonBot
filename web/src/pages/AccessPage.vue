<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";

import { api } from "../api";
import { errorText } from "../i18n";
import { state } from "../state";

const router = useRouter();
const code = ref("");
const error = ref("");
const busy = ref(false);

async function submit() {
  busy.value = true;
  error.value = "";
  try {
    await api.enterAccessCode(code.value);
    state.health = await api.health();
    await router.replace("/agents");
  } catch (e) {
    error.value = errorText(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="center">
    <form class="card" @submit.prevent="submit">
      <div class="brand" style="padding: 0; color: var(--text)"><span class="logo">智</span><b>CommonBot</b></div>
      <h2>{{ $t("access.title") }}</h2>
      <p class="muted">{{ $t("access.hint") }}</p>
      <div v-if="error" class="alert err" role="alert">{{ error }}</div>
      <input v-model="code" type="text" required maxlength="16" autocomplete="off" autocapitalize="characters" spellcheck="false" :placeholder="$t('access.placeholder')" :aria-label="$t('access.title')" style="letter-spacing: 0.2em; text-transform: uppercase" />
      <button class="btn primary" type="submit" :disabled="busy || !code.trim()">{{ $t("access.enter") }}</button>
    </form>
  </div>
</template>
