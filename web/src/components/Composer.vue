<script setup lang="ts">
// 统一的输入区。目前有模型选择；上传文件、知识范围、联网等选项随对应功能加入。
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";

import type { Attachment } from "../api";
import { folderPath, modelOptions, select, state } from "../state";
import FolderPicker from "./FolderPicker.vue";
import Icon from "./Icon.vue";

// 布尔类型的属性不传时会被当成 false；这里显式设为「没传」，才能区分「不显示开关」和「关着」
const props = withDefaults(defineProps<{
  modelValue: string;
  busy: boolean;
  placeholder: string;
  lockedModel?: string | null;
  /** 这次对话查哪些文件夹；null 表示按智能体的默认。不传则不显示范围选择。 */
  scope?: string[] | null;
  /** 智能体默认查的文件夹。 */
  defaultScope?: string[];
  /** 这次对话是否联网；null 表示按智能体的默认。不传则不显示开关。 */
  web?: boolean | null;
  defaultWeb?: boolean;
  /** 已读好、等着随下一条消息发出的文件。不传则不显示上传。 */
  attachments?: Attachment[];
  attaching?: boolean;
}>(), { web: undefined, defaultWeb: undefined, lockedModel: undefined, scope: undefined });
const emit = defineEmits<{
  "update:modelValue": [string];
  "update:scope": [string[] | null];
  "update:web": [boolean | null];
  attach: [File[]];
  detach: [string];
  send: [];
  stop: [];
}>();

const filePicker = ref<HTMLInputElement>();
const dragging = ref(false);
const webOn = computed(() => props.web ?? props.defaultWeb ?? false);
function toggleWeb() {
  // 点回和智能体默认一致的状态时，恢复为「按默认」
  const next = !webOn.value;
  emit("update:web", next === (props.defaultWeb ?? false) ? null : next);
}
function chosen(files: FileList | null | undefined) {
  const list = Array.from(files ?? []);
  if (list.length) emit("attach", list);
  if (filePicker.value) filePicker.value.value = "";
}
function pasted(e: ClipboardEvent) {
  // 粘贴截图或文件时当作上传；粘贴文字不受影响
  const files = Array.from(e.clipboardData?.files ?? []);
  if (files.length && props.attachments) {
    e.preventDefault();
    emit("attach", files);
  }
}

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
function closeOnEsc(e: KeyboardEvent) {
  if (e.key === "Escape") popOpen.value = false;
}
// 浮层打开时：点别处或按 Esc 关闭
watch(popOpen, (open) => {
  const toggle = open ? document.addEventListener : document.removeEventListener;
  toggle.call(document, "click", closePop);
  toggle.call(document, "keydown", closeOnEsc as EventListener);
});
onBeforeUnmount(() => {
  document.removeEventListener("click", closePop);
  document.removeEventListener("keydown", closeOnEsc as EventListener);
});

const box = ref<HTMLTextAreaElement>();
const options = computed(modelOptions);
const picked = computed({
  get: () => (state.selection.model ? `${state.selection.providerId}\n${state.selection.model}` : ""),
  set: (v: string) => {
    const [providerId, model] = v.split("\n");
    select(providerId, model);
  },
});
const canSend = computed(() => !props.busy && !props.attaching && props.modelValue.trim() !== "" && (!!props.lockedModel || state.selection.model !== ""));

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
  <div class="composer" :class="{ over: dragging }" @dragover.prevent="dragging = !!attachments" @dragleave="dragging = false" @drop.prevent="dragging = false; attachments && chosen($event.dataTransfer?.files)">
    <div v-if="attachments?.length || attaching" class="atts">
      <span v-for="f in attachments" :key="f.id" class="att" :title="f.skipped_pages ? $t('web.skipped', { n: f.skipped_pages }) : f.name">
        <em>{{ f.name.split(".").pop()?.toUpperCase().slice(0, 4) }}</em><span>{{ f.name }}</span><i v-if="f.skipped_pages" class="warn">!</i>
        <button type="button" class="x" :aria-label="$t('web.remove')" @click="emit('detach', f.id)"><Icon name="x" /></button>
      </span>
      <span v-if="attaching" class="att"><span>{{ $t("web.uploading") }}</span></span>
    </div>
    <textarea
      ref="box"
      rows="2"
      :value="modelValue"
      :placeholder="placeholder"
      :aria-label="placeholder"
      @input="emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)"
      @keydown="onKey"
      @paste="pasted"
    />
    <div class="tools">
      <template v-if="attachments">
        <button type="button" class="chip" :disabled="attaching" @click="filePicker?.click()"><Icon name="clip" />{{ $t("web.upload") }}</button>
        <input ref="filePicker" type="file" multiple hidden @change="chosen(($event.target as HTMLInputElement).files)" />
      </template>
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
      <button v-if="web !== undefined" type="button" class="chip" :aria-pressed="webOn" @click="toggleWeb"><Icon name="globe" />{{ $t("web.chip") }}</button>
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
