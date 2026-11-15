import "./styles/app.css";

import { createApp } from "vue";

import { hooks } from "./api";
import App from "./App.vue";
import { i18n } from "./i18n";
import { router } from "./router";

hooks.onAccessRequired = () => {
  if (router.currentRoute.value.name !== "access") void router.replace({ name: "access" });
};

createApp(App)
  .use(i18n)
  .use(router)
  // 出现时把光标放进去（新建、重命名的输入框）
  .directive("focus", { mounted: (el: HTMLElement) => el.focus() })
  .mount("#app");
