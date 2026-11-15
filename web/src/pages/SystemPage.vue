<script setup lang="ts">
import { onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";

import { api } from "../api";
import { errorText, type Lang, LANGS, setLang } from "../i18n";
import { state } from "../state";
import { copyText } from "../util";

const { locale } = useI18n();
const JJ = window.JJTheme;
const theme = ref(JJ.get());
onMounted(() => JJ.onChange((s) => (theme.value = s)));

const code = ref("");
const shown = ref(false);
const notice = ref("");
const error = ref("");
const sys = ref<{ lan: boolean; lan_fixed: boolean; port: number; addresses: string[] } | null>(null);
const switching = ref(false);
onMounted(async () => {
  try {
    sys.value = await api.system();
  } catch (e) {
    error.value = errorText(e);
  }
});
async function toggleLan() {
  if (!sys.value || switching.value) return;
  const next = !sys.value.lan;
  switching.value = true;
  error.value = "";
  try {
    await api.setLan(next);
    // 服务按新设置重新监听需要一两秒；等它回来再读状态
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, 500));
      try {
        const now = await api.system();
        if (now.lan === next) {
          sys.value = now;
          state.health = await api.health();
          break;
        }
      } catch {
        /* 还在切换 */
      }
    }
  } catch (e) {
    error.value = errorText(e);
  } finally {
    switching.value = false;
  }
}

async function toggleCode() {
  error.value = "";
  if (shown.value) return void (shown.value = false);
  try {
    code.value = (await api.accessCode()).code;
    shown.value = true;
  } catch (e) {
    error.value = errorText(e);
  }
}
async function resetCode() {
  error.value = notice.value = "";
  try {
    code.value = (await api.resetAccessCode()).code;
    shown.value = true;
    notice.value = "system.resetDone";
  } catch (e) {
    error.value = errorText(e);
  }
}
</script>

<template>
  <header class="page-head">
    <div>
      <div class="eyebrow">{{ $t("nav.settings") }}</div>
      <h1>{{ $t("system.title") }}</h1>
    </div>
  </header>
  <div class="cols">
    <div class="card">
      <h2>{{ $t("system.lan") }}</h2>
      <p class="muted" style="margin-top: 6px">{{ sys?.lan ? $t("system.lanOnDesc") : $t("system.lanOffDesc") }}</p>
      <div v-if="error" class="alert err" role="alert" style="margin-top: 10px">{{ error }}</div>
      <div v-if="notice" class="alert ok" style="margin-top: 10px">{{ $t(notice) }}</div>
      <template v-if="state.health?.local">
        <div class="item">
          <div class="grow"><b>{{ $t("system.lanSwitch") }}</b><div v-if="sys?.lan_fixed" class="muted">{{ $t("system.lanFixed") }}</div></div>
          <button class="switch" :class="{ on: sys?.lan }" role="switch" :aria-checked="!!sys?.lan" :aria-label="$t('system.lanSwitch')" :disabled="!sys || sys.lan_fixed || switching" @click="toggleLan" />
        </div>
        <div v-if="sys?.lan" class="item">
          <div class="grow">
            <b>{{ $t("system.address") }}</b>
            <div v-for="ip in sys.addresses" :key="ip" class="kv" style="margin-top: 6px"><span>http://{{ ip }}:{{ sys.port }}</span><button class="btn ghost sm" @click="copyText(`http://${ip}:${sys.port}`)">{{ $t("common.copy") }}</button></div>
            <div class="muted" style="margin-top: 4px">{{ sys.addresses.length ? $t("system.addressHint") : $t("system.noAddress") }}</div>
          </div>
        </div>
        <div class="item">
          <div class="grow">
            <b>{{ $t("system.accessCode") }}</b>
            <div v-if="shown" class="kv" style="margin-top: 6px"><span style="letter-spacing: 0.2em; font-size: 15px">{{ code }}</span><button class="btn ghost sm" @click="copyText(code)">{{ $t("common.copy") }}</button></div>
          </div>
          <button class="btn ghost sm" @click="toggleCode">{{ shown ? $t("system.hide") : $t("system.show") }}</button>
          <button class="btn ghost sm" @click="resetCode">{{ $t("system.reset") }}</button>
        </div>
      </template>
      <div v-else class="item"><span class="muted">{{ $t("system.localOnly") }}</span></div>
      <div class="item">
        <div class="grow"><b>{{ $t("system.version") }}</b></div>
        <RouterLink class="btn ghost sm" to="/help">{{ $t("nav.help") }}</RouterLink>
        <span class="muted">CommonBot {{ state.health?.version }}</span>
      </div>
    </div>

    <div class="card">
      <h2>{{ $t("system.language") }}</h2>
      <div class="seg" style="margin: 10px 0 18px">
        <button v-for="l in LANGS" :key="l.key" :aria-pressed="locale === l.key" @click="setLang(l.key as Lang)">{{ l.label }}</button>
      </div>
      <h2>{{ $t("system.realm") }}</h2>
      <p class="muted" style="margin: 4px 0 10px">{{ $t("system.realmHint") }}</p>
      <div class="realms">
        <button v-for="r in JJ.REALMS" :key="r.key" class="realm" :aria-pressed="theme.current === r.key" :title="r.desc" @click="JJ.set({ realm: r.key, follow: false })">
          <span class="sw"><i v-for="c in r.swatch" :key="c" :style="{ background: c }" /></span>{{ r.name }}<template v-if="r.dark">（{{ $t("system.dark") }}）</template>
        </button>
      </div>
      <label class="row" style="margin-top: 12px"><input type="checkbox" :checked="theme.follow" @change="JJ.set({ follow: ($event.target as HTMLInputElement).checked })" />{{ $t("system.follow") }}</label>
    </div>
  </div>
</template>
