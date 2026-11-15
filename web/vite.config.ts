import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [vue()],
  server: { proxy: { "/api": "http://127.0.0.1:8600" } },
  build: { target: "es2020", chunkSizeWarningLimit: 600 },
  test: { environment: "node" },
});
