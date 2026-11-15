<script setup lang="ts">
// 使用说明：随程序内置，离线可看。内容与安装包里的说明文档是同一份。
import MarkdownIt from "markdown-it";
import { computed } from "vue";
import { useI18n } from "vue-i18n";

const guides = import.meta.glob("../guide/*.md", { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const { locale } = useI18n();
// 说明文档是我们自己写的，这里允许表格等完整语法；不含任何外部内容
const md = new MarkdownIt({ html: false, linkify: false });
const html = computed(() => md.render(guides[`../guide/${locale.value}.md`] ?? guides["../guide/zh-CN.md"] ?? ""));
</script>

<template>
  <article class="card guide md" v-html="html" />
</template>
