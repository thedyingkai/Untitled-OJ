import { createApp } from "vue";
import { createPinia } from "pinia";
import { createRouter, createWebHashHistory } from "vue-router";
import App from "./App.vue";
import "./styles/theme.css";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", redirect: "/topology" },
    {
      path: "/topology",
      name: "topology",
      component: () => import("./views/TopologyView.vue"),
    },
    {
      path: "/market",
      name: "market",
      component: () => import("./views/StoreView.vue"),
    },
    {
      path: "/services",
      name: "services",
      component: () => import("./views/ServicesView.vue"),
    },
    {
      path: "/operations",
      name: "operations",
      component: () => import("./views/OperationsView.vue"),
    },
  ],
});

createApp(App).use(createPinia()).use(router).mount("#app");
