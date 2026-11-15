<script setup lang="ts">
// 统一的输入区。目前有模型选择；上传文件、知识范围、联网等选项随对应功能加入。
import { computed, nextTick, ref, watch } from "vue";

import { modelOptions, select, state } from "../state";
import Icon from "./Icon.vue";

const props = defineProps<{ modelValue: string; busy: boolean; placeholder: string }>();
const emit = defineEmits<{ "update:modelValue": [string]; send: []; stop: [] }>();

const box = ref<HTMLTextAreaElement>();
const options = computed(modelOptions);
const picked = computed({
  get: () => (state.selection.model ? `${state.selection.providerId}\n${state.selection.model}` : ""),
  set: (v: string) => {
    const [providerId, model] = v.split("\n");
    select(providerId, model);
  },
});
const canSend = computed(() => !props.busy && props.modelValue.trim() !== "" && state.selection.model !== "");

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
      <label v-if="options.length" class="chip" :title="$t('chat.pickModel')">
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
