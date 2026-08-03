import { defineStore } from "pinia";
import {
  api,
  isAuthRequiredError,
  isRequestCancelled,
} from "./api";
import { authRequired as authRequiredState } from "./auth";
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
  TopologyRevision,
} from "./types";

let pollTimer: ReturnType<typeof setInterval> | null = null;
let layoutTimer: ReturnType<typeof setTimeout> | null = null;
let visibilityHandler: (() => void) | null = null;
let coreRefresh: Promise<void> | null = null;
let coreRefreshGeneration = 0;
let coreRefreshController: AbortController | null = null;
let storeRefreshController: AbortController | null = null;
let layoutLoadController: AbortController | null = null;
let layoutSaveController: AbortController | null = null;
const toastTimers = new Map<number, ReturnType<typeof setTimeout>>();

const MAX_TOASTS = 6;

export interface Toast {
  id: number;
  kind: "ok" | "err" | "info";
  text: string;
}

let toastSeq = 1;

export const useOrchestrator = defineStore("orchestrator", {
  state: () => ({
    health: null as HealthInfo | null,
    nodes: [] as NodeRow[],
    services: [] as ServiceRow[],
    deployments: [] as DeploymentRow[],
    endpoints: [] as EndpointRow[],
    links: [] as LinkRow[],
    operations: [] as OperationRow[],
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
    serviceById(state) {
      return (id: string) =>
        state.services.find((service) => service.id === id);
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
      const id = toastSeq++;
      this.toasts.push({ id, kind, text });
      while (this.toasts.length > MAX_TOASTS) {
        const removed = this.toasts.shift();
        if (removed) {
          const timer = toastTimers.get(removed.id);
          if (timer) clearTimeout(timer);
          toastTimers.delete(removed.id);
        }
      }
      const timer = setTimeout(() => {
        this.toasts = this.toasts.filter((toast) => toast.id !== id);
        toastTimers.delete(id);
      }, kind === "err" ? 7000 : 3500);
      toastTimers.set(id, timer);
    },

    async refreshCore(force = false) {
      if (coreRefresh && !force) return coreRefresh;

      if (force) coreRefreshController?.abort("superseded");
      const controller = new AbortController();
      coreRefreshController = controller;

      const generation = ++coreRefreshGeneration;
      const refresh = (async () => {
        this.loading = true;
        this.coreStatus = "loading";
        try {
          const [
            health,
            capabilities,
            nodes,
            deployments,
            topology,
            operations,
          ] =
            await Promise.all([
              api.health({ signal: controller.signal }),
              api.capabilities({ signal: controller.signal }),
              api.nodes({ signal: controller.signal }),
              api.deployments({ signal: controller.signal }),
              api.topology("primary", { signal: controller.signal }),
              api.operations({ signal: controller.signal }),
            ]);
          if (generation !== coreRefreshGeneration) return;
          this.health = health;
          this.capabilities = capabilities;
          this.nodes = nodes;
          this.topology = topology;

          const endpointStatuses = new Map(
            (topology?.status?.endpoints ?? []).map((status) => [
              status.endpoint,
              status,
            ]),
          );
          const linkStatuses = new Map(
            (topology?.status?.links ?? []).map((status) => [
              `${status.source_endpoint}\0${status.target_endpoint}`,
              status,
            ]),
          );
          const endpointRows: EndpointRow[] =
            topology?.draft.spec.endpoints.map((endpoint) => {
              const status = endpointStatuses.get(endpoint.endpoint);
              return {
                endpoint: endpoint.endpoint,
                service_id: endpoint.service_id,
                protocol: endpoint.protocol,
                expose:
                  endpoint.endpoint === topology.draft.spec.root_endpoint
                    ? topology.draft.spec.authority.exposure_policy
                    : "",
                source: "topology-draft",
                health_path: endpoint.health_path,
                health: status?.health ?? "UNKNOWN",
                reachable: status?.reachable ?? false,
                display_name: endpoint.display_name,
                note: endpoint.note,
              };
            }) ?? [];
          const linkRows: LinkRow[] =
            topology?.draft.spec.links.map((link) => ({
              from: link.source_endpoint,
              to: link.target_endpoint,
              protocol: link.protocol,
              auth_mode: link.auth_mode,
              scope: link.scope,
              enabled: link.enabled ? "enabled" : "disabled",
              source: "topology-draft",
              health:
                linkStatuses.get(
                  `${link.source_endpoint}\0${link.target_endpoint}`,
                )?.health ?? "UNKNOWN",
            })) ?? [];
          const nodeById = new Map(nodes.map((node) => [node.node_id, node]));
          const enrichedDeployments = deployments.map((deployment) => {
            const matchingEndpoints = endpointRows.filter(
              (endpoint) => endpoint.service_id === deployment.service_id,
            );
            const primaryEndpoint = matchingEndpoints[0];
            return {
              ...deployment,
              host_ip:
                nodeById.get(deployment.node_id)?.host_ip || deployment.node_id,
              endpoint: primaryEndpoint?.endpoint ?? "",
              protocol: primaryEndpoint?.protocol ?? "",
              health_path: primaryEndpoint?.health_path ?? "",
              endpoint_health: primaryEndpoint?.health ?? deployment.endpoint_health,
              reachable: primaryEndpoint?.reachable ?? deployment.reachable,
              endpoint_count: matchingEndpoints.length,
              endpoints: matchingEndpoints.map((endpoint) => endpoint.endpoint),
            };
          });
          const serviceRows = new Map<string, ServiceRow>();
          for (const deployment of enrichedDeployments) {
            if (!deployment.service_id || serviceRows.has(deployment.service_id)) continue;
            serviceRows.set(deployment.service_id, {
              id: deployment.service_id,
              name: deployment.service_id,
              version: deployment.version,
              kind: deployment.kind,
              endpoint: deployment.endpoint,
              runtime: deployment.runtime,
              ui: "",
              health: deployment.endpoint_health,
            });
          }
          this.services = [...serviceRows.values()];
          this.deployments = enrichedDeployments;
          this.endpoints = endpointRows;
          this.links = linkRows;
          this.operations = operations;
          this.connected = true;
          this.coreStatus = "ready";
          this.coreError = "";
        } catch (err) {
          if (generation !== coreRefreshGeneration) return;
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
          if (generation === coreRefreshGeneration) this.loading = false;
        }
      })();

      coreRefresh = refresh;
      try {
        await refresh;
      } finally {
        if (coreRefresh === refresh) coreRefresh = null;
        if (coreRefreshController === controller) coreRefreshController = null;
      }
    },

    async loadLayout() {
      layoutLoadController?.abort("superseded");
      const controller = new AbortController();
      layoutLoadController = controller;
      this.layoutStatus = "loading";
      try {
        this.layout = await api.getLayout({ signal: controller.signal });
        if (layoutLoadController !== controller) return;
        this.layoutStatus = "ready";
        this.layoutError = "";
      } catch (err) {
        if (layoutLoadController !== controller || isRequestCancelled(err)) return;
        this.layout = {};
        this.layoutStatus = "error";
        this.layoutError = `布局加载失败：${(err as Error).message}`;
        if (!isAuthRequiredError(err)) this.toast("err", this.layoutError);
      } finally {
        if (layoutLoadController === controller) layoutLoadController = null;
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
      layoutSaveController?.abort("superseded");
      layoutTimer = setTimeout(() => {
        layoutTimer = null;
        void this.saveLayout();
      }, 600);
    },

    async saveLayout() {
      layoutSaveController?.abort("superseded");
      const controller = new AbortController();
      layoutSaveController = controller;
      const snapshot = JSON.parse(JSON.stringify(this.layout)) as LayoutState;
      this.layoutStatus = "saving";
      try {
        await api.putLayout(snapshot, { signal: controller.signal });
        if (layoutSaveController !== controller) return;
        this.layoutStatus = "ready";
        this.layoutError = "";
      } catch (err) {
        if (layoutSaveController !== controller || isRequestCancelled(err)) return;
        this.layoutStatus = "error";
        this.layoutError = `布局保存失败：${(err as Error).message}`;
        if (!isAuthRequiredError(err)) this.toast("err", this.layoutError);
      } finally {
        if (layoutSaveController === controller) layoutSaveController = null;
      }
    },

    async refreshStore(refresh = false) {
      storeRefreshController?.abort("superseded");
      const controller = new AbortController();
      storeRefreshController = controller;
      // A fresh Desktop registry is a valid empty state. Until the first trusted
      // catalog is registered, catalog.search is deliberately not published.
      if (!this.supportsAction("catalog.search")) {
        this.storeIndex = null;
        this.storeLoadStatus = "ready";
        this.storeError = "";
        if (storeRefreshController === controller) storeRefreshController = null;
        return;
      }
      this.storeLoadStatus = "loading";
      try {
        const index = await api.storeIndex(refresh, {
          signal: controller.signal,
        });
        if (storeRefreshController !== controller) return;
        this.storeIndex = index;
        this.storeLoadStatus = "ready";
        this.storeError = "";
      } catch (err) {
        if (storeRefreshController !== controller || isRequestCancelled(err)) return;
        this.storeIndex = null;
        this.storeLoadStatus = "error";
        this.storeError = `商店加载失败：${(err as Error).message}`;
        // 401 交给身份重定向，不叠加一条同义 toast。
        if (!isAuthRequiredError(err)) {
          this.toast("err", this.storeError);
        }
      } finally {
        if (storeRefreshController === controller) storeRefreshController = null;
      }
    },

    startPolling() {
      if (pollTimer) return;
      void this.refreshCore(true);
      visibilityHandler = () => {
        if (document.visibilityState === "visible") {
          void this.refreshCore();
        }
      };
      document.addEventListener("visibilitychange", visibilityHandler);
      pollTimer = setInterval(() => {
        if (document.visibilityState === "visible") {
          void this.refreshCore();
        }
      }, 4000);
    },

    stopPolling() {
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
      if (visibilityHandler) {
        document.removeEventListener("visibilitychange", visibilityHandler);
        visibilityHandler = null;
      }
      // 已发出的请求不能取消，但它完成后也不得覆盖 stop 之后的状态。
      coreRefreshController?.abort("polling stopped");
      coreRefreshController = null;
      coreRefreshGeneration += 1;
      this.loading = false;
      if (this.coreStatus === "loading") this.coreStatus = "idle";
    },

    dispose() {
      this.stopPolling();
      storeRefreshController?.abort("application disposed");
      storeRefreshController = null;
      layoutLoadController?.abort("application disposed");
      layoutLoadController = null;
      layoutSaveController?.abort("application disposed");
      layoutSaveController = null;
      if (layoutTimer) {
        clearTimeout(layoutTimer);
        layoutTimer = null;
      }
      for (const timer of toastTimers.values()) clearTimeout(timer);
      toastTimers.clear();
    },
  },
});
