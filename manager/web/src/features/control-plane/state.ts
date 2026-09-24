import {
  isAuthRequiredError,
  isRequestCancelled,
} from "../../shared/api/transport";
import { controlPlaneApi } from "./api";
import { nodesApi } from "../nodes/api";
import { deploymentsApi } from "../deployments/api";
import { topologyApi } from "../topology/api";
import { operationsApi } from "../operations/api";
import { layoutApi } from "../topology/layout-api";
import { storeApi } from "../store/api";
import { runtimeFor } from "./runtime";
import { projectControlPlane } from "./projection";
import { defineStore } from "pinia";

import { authRequired as authRequiredState } from "../../auth";
import type {
  DeploymentRow,
  CapabilityRow,
  EndpointRow,
  HealthInfo,
  LayoutState,
  LinkRow,
  NodeRow,
  OperationRow,
  ServiceRow,
  StoreIndexResponse,
  LoadStatus,
  TopologyDetail,
  TopologyHeads,
  TopologyRevision,
} from "../../types";

const MAX_TOASTS = 6;

export interface Toast {
  id: number;
  kind: "ok" | "err" | "info";
  text: string;
}

export const useOrchestrator = defineStore("orchestrator", {
  state: () => ({
    health: null as HealthInfo | null,
    nodes: [] as NodeRow[],
    services: [] as ServiceRow[],
    deployments: [] as DeploymentRow[],
    endpoints: [] as EndpointRow[],
    links: [] as LinkRow[],
    operations: [] as OperationRow[],
    topologyHeads: [] as TopologyHeads[],
    activeTopologyId: "",
    topology: null as TopologyDetail | null,
    topologyRevisions: [] as TopologyRevision[],
    capabilities: [] as CapabilityRow[],
    layout: {} as LayoutState,
    layoutLoaded: false,
    storeIndex: null as StoreIndexResponse | null,
    connected: false,
    loading: false,
    coreStatus: "idle" as LoadStatus,
    coreError: "",
    storeLoadStatus: "idle" as LoadStatus,
    storeError: "",
    layoutStatus: "idle" as LoadStatus | "saving",
    layoutError: "",
    toasts: [] as Toast[],
  }),

  getters: {
    /**
     * 控制面是否要求重新建立身份会话（收到过 401）。真实状态存在 auth.ts 的 ref 里，
     * 避免 api.ts 反向依赖 store.ts 形成循环导入；这里以 getter 暴露给视图。
     */
    authRequired(): boolean {
      return authRequiredState.value;
    },
    runningOperations(state): OperationRow[] {
      return state.operations.filter((op) =>
        ["PLANNED", "CONFIRMED", "ENQUEUING", "RUNNING", "CANCELLING"].includes(
          op.status,
        ),
      );
    },
    serviceByDeploymentId(state) {
      return (id: string) =>
        state.services.find((service) => service.deployment_id === id);
    },
    supportsAction(state) {
      return (action: string) =>
        state.capabilities.some((capability) => capability.action === action);
    },
  },

  actions: {
    ensureAction(action: string): boolean {
      if (this.supportsAction(action)) return true;
      const detail = this.capabilities.length
        ? `当前控制面未发布能力 ${action}`
        : "能力清单尚未就绪，请等待连接恢复后重试";
      this.toast("err", detail);
      return false;
    },

    toast(kind: Toast["kind"], text: string) {
      const runtime = runtimeFor(this);
      const id = runtime.toastSeq++;
      this.toasts.push({ id, kind, text });
      while (this.toasts.length > MAX_TOASTS) {
        const removed = this.toasts.shift();
        if (removed) {
          const timer = runtime.toastTimers.get(removed.id);
          if (timer) clearTimeout(timer);
          runtime.toastTimers.delete(removed.id);
        }
      }
      const timer = setTimeout(
        () => {
          this.toasts = this.toasts.filter((toast) => toast.id !== id);
          runtime.toastTimers.delete(id);
        },
        kind === "err" ? 7000 : 3500,
      );
      runtime.toastTimers.set(id, timer);
    },

    async refreshCore(force = false) {
      const runtime = runtimeFor(this);
      if (runtime.coreRefresh && !force) return runtime.coreRefresh;

      if (force) runtime.coreRefreshController?.abort("superseded");
      const controller = new AbortController();
      runtime.coreRefreshController = controller;

      const generation = ++runtime.coreRefreshGeneration;
      const refresh = (async () => {
        this.loading = true;
        this.coreStatus = "loading";
        try {
          const [
            health,
            capabilities,
            nodes,
            deployments,
            topologyHeads,
            operations,
          ] = await Promise.all([
            controlPlaneApi.health({ signal: controller.signal }),
            controlPlaneApi.capabilities({ signal: controller.signal }),
            nodesApi.nodes({ signal: controller.signal }),
            deploymentsApi.deployments({ signal: controller.signal }),
            topologyApi.topologyList({ signal: controller.signal }),
            operationsApi.operations({ signal: controller.signal }),
          ]);
          const topologyIds = new Set(
            topologyHeads.map((heads) => heads.topology_id),
          );
          const activeTopologyId = topologyIds.has(this.activeTopologyId)
            ? this.activeTopologyId
            : (topologyHeads[0]?.topology_id ?? "");
          const topology = activeTopologyId
            ? await topologyApi.topology(activeTopologyId, {
                signal: controller.signal,
              })
            : null;
          if (generation !== runtime.coreRefreshGeneration) return;
          this.health = health;
          this.capabilities = capabilities;
          this.nodes = nodes;
          this.topologyHeads = topologyHeads;
          this.activeTopologyId = activeTopologyId;
          this.topology = topology;

          const {
            services: serviceRows,
            deployments: enrichedDeployments,
            endpoints: endpointRows,
            links: linkRows,
          } = projectControlPlane(nodes, deployments, topology);
          this.services = serviceRows;
          this.deployments = enrichedDeployments;
          this.endpoints = endpointRows;
          this.links = linkRows;
          this.operations = operations;
          this.connected = true;
          this.coreStatus = "ready";
          this.coreError = "";
        } catch (err) {
          if (generation !== runtime.coreRefreshGeneration) return;
          if (isRequestCancelled(err)) return;
          if (isAuthRequiredError(err)) {
            // 401 会触发 OIDC 重定向；daemon 其实是通的，轮询期间不再弹 toast。
            this.connected = true;
            this.coreStatus = "error";
            this.coreError = err.message;
            return;
          }
          const message = String((err as Error).message ?? err);
          if (this.connected || this.coreError !== message) {
            this.toast("err", message);
          }
          this.connected = false;
          this.coreStatus = "error";
          this.coreError = message;
        } finally {
          if (generation === runtime.coreRefreshGeneration)
            this.loading = false;
        }
      })();

      runtime.coreRefresh = refresh;
      try {
        await refresh;
      } finally {
        if (runtime.coreRefresh === refresh) runtime.coreRefresh = null;
        if (runtime.coreRefreshController === controller)
          runtime.coreRefreshController = null;
      }
    },

    async loadLayout() {
      const runtime = runtimeFor(this);
      runtime.layoutLoadController?.abort("superseded");
      const controller = new AbortController();
      runtime.layoutLoadController = controller;
      this.layoutStatus = "loading";
      try {
        const topologyId = this.activeTopologyId;
        if (!topologyId) {
          this.layout = {};
          this.layoutStatus = "ready";
          this.layoutError = "";
        } else {
          const layout = await layoutApi.getLayout(topologyId, {
            signal: controller.signal,
          });
          if (runtime.layoutLoadController !== controller) return;
          this.layout = layout;
          this.layoutStatus = "ready";
          this.layoutError = "";
        }
      } catch (err) {
        if (
          runtime.layoutLoadController !== controller ||
          isRequestCancelled(err)
        )
          return;
        this.layout = {};
        this.layoutStatus = "error";
        this.layoutError = `布局加载失败：${(err as Error).message}`;
        if (!isAuthRequiredError(err)) this.toast("err", this.layoutError);
      } finally {
        if (runtime.layoutLoadController === controller)
          runtime.layoutLoadController = null;
      }
      this.layoutLoaded = true;
    },

    setNodePosition(id: string, position: { x: number; y: number }) {
      const runtime = runtimeFor(this);
      if (!this.layout.positions) this.layout.positions = {};
      this.layout.positions[id] = {
        x: Math.round(position.x),
        y: Math.round(position.y),
      };
      if (runtime.layoutTimer) clearTimeout(runtime.layoutTimer);
      runtime.layoutSaveController?.abort("superseded");
      runtime.layoutTimer = setTimeout(() => {
        runtime.layoutTimer = null;
        void this.saveLayout();
      }, 600);
    },

    async saveLayout() {
      const runtime = runtimeFor(this);
      runtime.layoutSaveController?.abort("superseded");
      const controller = new AbortController();
      runtime.layoutSaveController = controller;
      const snapshot = JSON.parse(JSON.stringify(this.layout)) as LayoutState;
      const topologyId = this.activeTopologyId;
      if (!topologyId) {
        if (runtime.layoutSaveController === controller)
          runtime.layoutSaveController = null;
        this.layoutStatus = "error";
        this.layoutError = "必须先选择 Topology 才能保存布局";
        this.toast("err", this.layoutError);
        return;
      }
      this.layoutStatus = "saving";
      try {
        await layoutApi.putLayout(topologyId, snapshot, {
          signal: controller.signal,
        });
        if (runtime.layoutSaveController !== controller) return;
        this.layoutStatus = "ready";
        this.layoutError = "";
      } catch (err) {
        if (
          runtime.layoutSaveController !== controller ||
          isRequestCancelled(err)
        )
          return;
        this.layoutStatus = "error";
        this.layoutError = `布局保存失败：${(err as Error).message}`;
        if (!isAuthRequiredError(err)) this.toast("err", this.layoutError);
      } finally {
        if (runtime.layoutSaveController === controller)
          runtime.layoutSaveController = null;
      }
    },

    async selectTopology(topologyId: string) {
      const selected = topologyId.trim();
      if (
        selected &&
        !this.topologyHeads.some((heads) => heads.topology_id === selected)
      ) {
        this.toast("err", `Topology ${selected} 不在当前集合中`);
        return;
      }
      if (selected === this.activeTopologyId) return;
      this.activeTopologyId = selected;
      this.topology = null;
      this.endpoints = [];
      this.links = [];
      this.layout = {};
      this.layoutLoaded = false;
      await this.refreshCore(true);
      await this.loadLayout();
    },

    async refreshStore(refresh = false) {
      const runtime = runtimeFor(this);
      runtime.storeRefreshController?.abort("superseded");
      const controller = new AbortController();
      runtime.storeRefreshController = controller;
      // A fresh Desktop registry is a valid empty state. Until the first trusted
      // catalog is registered, catalog.search is deliberately not published.
      if (!this.supportsAction("catalog.search")) {
        this.storeIndex = null;
        this.storeLoadStatus = "ready";
        this.storeError = "";
        if (runtime.storeRefreshController === controller)
          runtime.storeRefreshController = null;
        return;
      }
      this.storeLoadStatus = "loading";
      try {
        const index = await storeApi.storeIndex(refresh, {
          signal: controller.signal,
        });
        if (runtime.storeRefreshController !== controller) return;
        this.storeIndex = index;
        this.storeLoadStatus = "ready";
        this.storeError = "";
      } catch (err) {
        if (
          runtime.storeRefreshController !== controller ||
          isRequestCancelled(err)
        )
          return;
        this.storeIndex = null;
        this.storeLoadStatus = "error";
        this.storeError = `商店加载失败：${(err as Error).message}`;
        // 401 交给身份重定向，不叠加一条同义 toast。
        if (!isAuthRequiredError(err)) {
          this.toast("err", this.storeError);
        }
      } finally {
        if (runtime.storeRefreshController === controller)
          runtime.storeRefreshController = null;
      }
    },

    startPolling() {
      const runtime = runtimeFor(this);
      if (runtime.pollTimer) return;
      void this.refreshCore(true);
      runtime.visibilityHandler = () => {
        if (document.visibilityState === "visible") {
          void this.refreshCore();
        }
      };
      document.addEventListener("visibilitychange", runtime.visibilityHandler);
      runtime.pollTimer = setInterval(() => {
        if (document.visibilityState === "visible") {
          void this.refreshCore();
        }
      }, 4000);
    },

    stopPolling() {
      const runtime = runtimeFor(this);
      if (runtime.pollTimer) {
        clearInterval(runtime.pollTimer);
        runtime.pollTimer = null;
      }
      if (runtime.visibilityHandler) {
        document.removeEventListener(
          "visibilitychange",
          runtime.visibilityHandler,
        );
        runtime.visibilityHandler = null;
      }
      // 取消在途请求；即使响应已经到达，也不能覆盖 stop 之后的状态。
      runtime.coreRefreshController?.abort("polling stopped");
      runtime.coreRefreshController = null;
      runtime.coreRefreshGeneration += 1;
      this.loading = false;
      if (this.coreStatus === "loading") this.coreStatus = "idle";
    },

    dispose() {
      const runtime = runtimeFor(this);
      this.stopPolling();
      runtime.storeRefreshController?.abort("application disposed");
      runtime.storeRefreshController = null;
      runtime.layoutLoadController?.abort("application disposed");
      runtime.layoutLoadController = null;
      runtime.layoutSaveController?.abort("application disposed");
      runtime.layoutSaveController = null;
      if (runtime.layoutTimer) {
        clearTimeout(runtime.layoutTimer);
        runtime.layoutTimer = null;
      }
      for (const timer of runtime.toastTimers.values()) clearTimeout(timer);
      runtime.toastTimers.clear();
    },
  },
});
