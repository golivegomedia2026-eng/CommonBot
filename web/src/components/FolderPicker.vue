<script setup lang="ts">
// 勾选文件夹（树形）。选中父文件夹即包含其子文件夹，所以子项跟着显示为选中且不可单独取消。
import { computed } from "vue";

import { folderTree } from "../state";

const props = defineProps<{ modelValue: string[] }>();
const emit = defineEmits<{ "update:modelValue": [string[]] }>();

const tree = computed(folderTree);

function covered(id: string): boolean {
  // 有没有上级文件夹被选中
  let cur = tree.value.find((t) => t.folder.id === id)?.folder.parent_id ?? null;
  while (cur) {
    if (props.modelValue.includes(cur)) return true;
    cur = tree.value.find((t) => t.folder.id === cur)?.folder.parent_id ?? null;
  }
  return false;
}

function toggle(id: string, on: boolean) {
  const next = props.modelValue.filter((x) => x !== id);
  if (on) next.push(id);
  emit("update:modelValue", next);
}
</script>

<template>
  <div class="picker">
    <label v-for="t in tree" :key="t.folder.id" :style="{ paddingLeft: `${6 + t.depth * 18}px` }">
      <input type="checkbox" :checked="modelValue.includes(t.folder.id) || covered(t.folder.id)" :disabled="covered(t.folder.id)" @change="toggle(t.folder.id, ($event.target as HTMLInputElement).checked)" />
      <span>{{ t.folder.name }}</span>
    </label>
    <div v-if="!tree.length" class="muted" style="padding: 4px 6px">{{ $t("kb.empty") }}</div>
  </div>
</template>
