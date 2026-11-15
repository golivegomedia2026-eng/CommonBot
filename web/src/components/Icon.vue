<script setup lang="ts">
defineProps<{ name: keyof typeof PATHS }>();
</script>
<script lang="ts">
const PATHS = {
  chip: '<rect x="4" y="4" width="8" height="8" rx="1.5"/><path d="M6.5 1.5v2M9.5 1.5v2M6.5 12.5v2M9.5 12.5v2M1.5 6.5h2M1.5 9.5h2M12.5 6.5h2M12.5 9.5h2"/>',
  send: '<path d="M8 13V3M3.5 7.5 8 3l4.5 4.5"/>',
  stop: '<rect x="4" y="4" width="8" height="8" rx="1.5"/>',
  plus: '<path d="M8 3v10M3 8h10"/>',
  x: '<path d="M4 4l8 8M12 4l-8 8"/>',
  list: '<path d="M2.5 4h11M2.5 8h11M2.5 12h11"/>',
  gear: '<circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/>',
  back: '<path d="M9.5 3.5 5 8l4.5 4.5"/>',
  folder: '<path d="M1.5 4.5v8h13v-7H8L6.5 3.5h-5z"/>',
  clip: '<path d="M13 7.5 8 12.5a3 3 0 0 1-4.2-4.2l5.4-5.4a2 2 0 0 1 2.8 2.8L6.7 11a1 1 0 0 1-1.4-1.4L10 5"/>',
  search: '<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5 14 14"/>',
  globe: '<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/>',
  copy: '<rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 5.5v-2a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2"/>',
};
</script>
<template>
  <svg class="icon" viewBox="0 0 16 16" aria-hidden="true" v-html="PATHS[name]" />
</template>
