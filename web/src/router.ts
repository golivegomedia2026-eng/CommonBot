import { createRouter, createWebHistory } from "vue-router";

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: "/", redirect: "/agents" },
    { path: "/agents", name: "agents", component: () => import("./pages/AgentsPage.vue") },
    { path: "/agents/:agentId/:id?", name: "chat", component: () => import("./pages/ChatPage.vue") },
    { path: "/kb/:folderId?", name: "kb", component: () => import("./pages/KbPage.vue") },
    { path: "/settings/models", name: "models", component: () => import("./pages/ModelsPage.vue") },
    { path: "/settings/system", name: "system", component: () => import("./pages/SystemPage.vue") },
    { path: "/access", name: "access", component: () => import("./pages/AccessPage.vue"), meta: { bare: true } },
    { path: "/:rest(.*)*", redirect: "/agents" },
  ],
});
