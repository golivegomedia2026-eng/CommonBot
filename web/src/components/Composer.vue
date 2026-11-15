<script setup lang="ts">
// 统一的输入区。目前有模型选择；上传文件、知识范围、联网等选项随对应功能加入。
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";

import { folderPath, modelOptions, select, state } from "../state";
import FolderPicker from "./FolderPicker.vue";
import Icon from "./Icon.vue";

const props = defineProps<{
  modelValue: string;
  busy: boolean;
  placeholder: string;
  lockedModel?: string | null;
  /** 这次对话查哪些文件夹；null 表示按智能体的默认。不传则不显示范围选择。 */
  scope?: string[] | null;
  /** 智能体默认查的文件夹。 */
  defaultScope?: string[];
}>();
const emit = defineEmits<{ "update:modelValue": [string]; "update:scope": [string[] | null]; send: []; stop: [] }>();

const popOpen = ref(false);
const effective = computed(() => props.scope ?? props.defaultScope ?? []);
const scopeLabel = computed(() => {
  const names = effective.value.map((id) => folderPath(id).split(" / ").pop()).filter(Boolean);
  return names.length ? names.join("、") : "";
});
const mode = computed(() => (props.scope == null ? "default" : props.scope.length ? "pick" : "none"));
function setMode(m: string) {
  if (m === "default") emit("update:scope", null);
  else if (m === "none") emit("update:scope", []);
  else emit("update:scope", effective.value.length ? [...effective.value] : state.folders.filter((f) => !f.parent_id).slice(0, 1).map((f) => f.id));
}
function closePop(e: Event) {
  if (!(e.target as HTMLElement).closest(".pw")) popOpen.value = false;
}
watch(popOpen, (open) => (open ? document.addEventListener("click", closePop) : document.removeEventListener("click", closePop)));
onBeforeUnmount(() => document.removeEventListener("click", closePop));

const box = ref<HTMLTextAreaElement>();
const options = computed(modelOptions);
const picked = computed({
  get: () => (state.selection.model ? `${state.selection.providerId}\n${state.selection.model}` : ""),
  set: (v: string) => {
    const [providerId, model] = v.split("\n");
    select(providerId, model);
  },
});
const canSend = computed(() => !props.busy && props.modelValue.trim() !== "" && (!!props.lockedModel || state.selection.model !== ""));

function resize() {
  const el = box.value;
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
}
watch(() => props.modelValue, () => nextTick(resize));

function onKey(e: KeyboardEvent) {
  // 输入法正在选字时的回车不发送
  if (e.key !== "Enter" || e.shiftKey || e.isComposing || e.keyCode === 229) return;
  e.preventDefault();
  if (canSend.value) emit("send");
}
defineExpose({ focus: () => box.value?.focus() });
</script>

<template>
  <div class="composer">
    <textarea
      ref="box"
      rows="2"
      :value="modelValue"
      :placeholder="placeholder"
      :aria-label="placeholder"
      @input="emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
      @keydown="onKey"
    />
    <div class="tools">
      <span v-if="scope !== undefined && state.folders.length" class="pw">
        <div v-if="popOpen" class="pop">
          <b>{{ $t("kb.scopeTitle") }}</b>
          <label><input type="radio" :checked="mode === 'default'" @change="setMode('default')" />{{ $t("kb.scopeDefault") }}</label>
          <label><input type="radio" :checked="mode === 'pick'" @change="setMode('pick')" />{{ $t("kb.scopePick") }}</label>
          <FolderPicker v-if="mode === 'pick'" :model-value="scope ?? []" @update:model-value="emit('update:scope', $event)" />
          <label><input type="radio" :checked="mode === 'none'" @change="setMode('none')" />{{ $t("kb.scopeNone") }}</label>
        </div>
        <button type="button" class="chip" :aria-pressed="effective.length > 0" :aria-expanded="popOpen" :title="$t('kb.scopeTitle')" @click="popOpen = !popOpen">
          <Icon name="folder" /><span>{{ scopeLabel || $t("kb.scopeNone") }}</span> ▾
        </button>
      </span>
      <span v-if="lockedModel" class="chip" style="cursor: default" :title="$t('agents.lockedModel')"><Icon name="chip" />{{ lockedModel }}</span>
      <label v-else-if="options.length" class="chip" :title="$t('chat.pickModel')">
        <Icon name="chip" />
        <select v-model="picked" :aria-label="$t('chat.pickModel')">
          <option v-for="o in options" :key="`${o.providerId}\n${o.model}`" :value="`${o.providerId}\n${o.model}`">{{ o.label }}</option>
        </select>
      </label>
      <button v-if="busy" class="btn ghost sm send" @click="emit('stop')"><Icon name="stop" />{{ $t("chat.stop") }}</button>
      <button v-else class="btn primary sm send" :disabled="!canSend" @click="emit('send')"><Icon name="send" />{{ $t("chat.send") }}</button>
    </div>
  </div>
</template>
