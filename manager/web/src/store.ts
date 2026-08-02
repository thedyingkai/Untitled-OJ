import { defineStore } from "pinia";
import { api, isAuthRequiredError } from "./api";
import {
  authRequired as authRequiredState,
  clearOrchestratorToken,
  hasOrchestratorToken,
  resolveAuthRequired,
  saveOrchestratorToken,
} from "./token";
import type {
  DeploymentRow,
  EndpointRow,
  HealthInfo,
  LayoutState,
  LinkRow,
  OperationRow,
  ServiceRow,
  StoreIndexResponse,
  StoreStatus,
} from "./types";

let pollTimer: ReturnType<typeof setInterval> | null = null;
let layoutTimer: ReturnType<typeof setTimeout> | null = null;

export interface Toast {
  id: number;
  kind: "ok" | "err" | "info";
  text: string;
}

let toastSeq = 1;

export const useOrchestrator = defineStore("orchestrator", {
  state: () => ({
    health: null as HealthInfo | null,
    services: [] as ServiceRow[],
    deployments: [] as DeploymentRow[],
    endpoints: [] as EndpointRow[],
    links: [] as LinkRow[],
    operations: [] as OperationRow[],
    layout: {} as LayoutState,
    layoutLoaded: false,
    storeStatus: null as StoreStatus | null,
    storeIndex: null as StoreIndexResponse | null,
    connected: true,
    loading: false,
    toasts: [] as Toast[],
  }),

  getters: {
    /**
     * 控制面是否要求令牌（收到过 401）。真实状态存在 token.ts 的 ref 里，
     * 避免 api.ts 反向依赖 store.ts 形成循环导入；这里以 getter 暴露给视图。
     */
    authRequired(): boolean {
      return authRequiredState.value;
    },
    /** 本地是否已保存令牌，用于侧边栏指示。 */
    tokenConfigured(): boolean {
      return hasOrchestratorToken();
    },
    runningOperations(state): OperationRow[] {
      return state.operations.filter((op) =>
        ["RUNNING", "PLANNED", "AWAITING_CONFIRMATION"].includes(op.status),
      );
    },
    serviceById(state) {
      return (id: string) =>
        state.services.find((service) => service.id === id);
    },
  },

  actions: {
    toast(kind: Toast["kind"], text: string) {
      const id = toastSeq++;
      this.toasts.push({ id, kind, text });
      setTimeout(() => {
        this.toasts = this.toasts.filter((toast) => toast.id !== id);
      }, kind === "err" ? 7000 : 3500);
    },

    async refreshCore() {
      try {
        const [health, services, deployments, endpoints, links, operations] =
          await Promise.all([
            api.health(),
            api.services(),
            api.deployments(),
            api.endpoints(),
            api.links(),
            api.operations(),
          ]);
        this.health = health;
        this.services = services;
        this.deployments = deployments;
        this.endpoints = endpoints;
        this.links = links;
        this.operations = operations;
        this.connected = true;
      } catch (err) {
        if (isAuthRequiredError(err)) {
          // 401 已由 TokenGate 全屏接管：daemon 其实是通的，轮询期间不再弹 toast。
          this.connected = true;
          return;
        }
        if (this.connected) {
          this.toast("err", String((err as Error).message ?? err));
        }
        this.connected = false;
      }
    },

    /** 保存控制面令牌并立即重试一次拉取；令牌不对会重新触发 401 → 门禁继续显示。 */
    async setToken(token: string) {
      saveOrchestratorToken(token);
      resolveAuthRequired();
      await this.refreshCore();
      if (!this.authRequired) {
        await this.loadLayout();
        this.toast("ok", "控制面令牌已保存");
      }
    },

    /** 清除本地令牌；下一次请求若仍需鉴权会重新弹出门禁。 */
    clearToken() {
      clearOrchestratorToken();
      resolveAuthRequired();
      this.toast("info", "已清除本地控制面令牌");
      void this.refreshCore();
    },

    async loadLayout() {
      try {
        this.layout = await api.getLayout();
      } catch {
        this.layout = {};
      }
      this.layoutLoaded = true;
    },

    setNodePosition(id: string, position: { x: number; y: number }) {
      if (!this.layout.positions) this.layout.positions = {};
      this.layout.positions[id] = {
        x: Math.round(position.x),
        y: Math.round(position.y),
      };
      if (layoutTimer) clearTimeout(layoutTimer);
      layoutTimer = setTimeout(() => {
        api.putLayout(this.layout).catch(() => {
          /* 布局保存失败不打断操作 */
        });
      }, 600);
    },

    async refreshStore(refresh = false) {
      try {
        this.storeStatus = await api.storeStatus();
      } catch {
        this.storeStatus = null;
      }
      try {
        this.storeIndex = await api.storeIndex(refresh);
      } catch (err) {
        this.storeIndex = null;
        // 401 交给 TokenGate，不叠加一条同义 toast。
        if (!isAuthRequiredError(err)) {
          this.toast("err", `商店索引加载失败：${(err as Error).message}`);
        }
      }
    },

    startPolling() {
      if (pollTimer) return;
      this.refreshCore();
      pollTimer = setInterval(() => {
        if (document.visibilityState === "visible") {
          this.refreshCore();
        }
      }, 4000);
    },

    stopPolling() {
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    },
  },
});
