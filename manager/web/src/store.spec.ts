import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { apiMock } = vi.hoisted(() => ({
  apiMock: {
    health: vi.fn(),
    capabilities: vi.fn(),
    nodes: vi.fn(),
    deployments: vi.fn(),
    topologyList: vi.fn(),
    topology: vi.fn(),
    operations: vi.fn(),
    getLayout: vi.fn(),
    putLayout: vi.fn(),
    storeIndex: vi.fn(),
  },
}));

vi.mock("./api", () => ({
  api: apiMock,
  isAuthRequiredError: () => false,
  isRequestCancelled: (error: unknown) =>
    error instanceof Error && error.name === "RequestCancelledError",
}));

import { useOrchestrator } from "./store";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

const oldHealth = { service: "old", store: "memory", warnings: [] };
const newHealth = { service: "new", store: "memory", warnings: [] };

function resolveOtherQueries() {
  apiMock.capabilities.mockResolvedValue([]);
  apiMock.nodes.mockResolvedValue([]);
  apiMock.deployments.mockResolvedValue([]);
  apiMock.topologyList.mockResolvedValue([]);
  apiMock.topology.mockResolvedValue(null);
  apiMock.operations.mockResolvedValue([]);
  apiMock.getLayout.mockResolvedValue({});
  apiMock.putLayout.mockResolvedValue({});
  apiMock.storeIndex.mockResolvedValue({ index: { modules: [] }, installed: {} });
}

describe("orchestrator core refresh", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.clearAllMocks();
    resolveOtherQueries();
  });

  it("coalesces concurrent polling refreshes", async () => {
    const health = deferred<typeof oldHealth>();
    apiMock.health.mockReturnValue(health.promise);
    const store = useOrchestrator();
    store.activeTopologyId = "primary";

    const first = store.refreshCore();
    const second = store.refreshCore();
    expect(apiMock.health).toHaveBeenCalledTimes(1);
    expect(apiMock.nodes).toHaveBeenCalledTimes(1);

    health.resolve(oldHealth);
    await Promise.all([first, second]);
    expect(store.health?.service).toBe("old");
  });

  it("loads the selected arbitrary topology and joins endpoints by deployment identity", async () => {
    apiMock.health.mockResolvedValue(newHealth);
    apiMock.topologyList.mockResolvedValue([
      { topology_id: "primary", draft_revision_id: "rev-p" },
      { topology_id: "contest-a", draft_revision_id: "rev-a" },
    ]);
    apiMock.deployments.mockResolvedValue([
      {
        deployment_id: "judge-a",
        node_id: "node-a",
        service_id: "judge",
        status: "RUNNING",
        observed_state: "RUNNING",
        endpoint: "10.0.0.1:8080:judge",
        endpoints: ["10.0.0.1:8080:judge"],
      },
      {
        deployment_id: "judge-b",
        node_id: "node-b",
        service_id: "judge",
        status: "RUNNING",
        observed_state: "RUNNING",
        endpoint: "10.0.0.2:8080:judge",
        endpoints: ["10.0.0.2:8080:judge"],
      },
    ]);
    apiMock.topology.mockResolvedValue({
      heads: { topology_id: "contest-a", draft_revision_id: "rev-a" },
      draft: {
        revision_id: "rev-a",
        spec: {
          topology_id: "contest-a",
          root_endpoint: "10.0.0.2:8080:judge",
          authority: {
            root_endpoint: "10.0.0.2:8080:judge",
            exposure_policy: "internal",
          },
          endpoints: [
            {
              endpoint: "10.0.0.2:8080:judge",
              service_id: "judge",
              protocol: "http",
              health_path: "/healthz/ready",
              display_name: "judge-b",
              note: "",
              config: { deployment_id: "judge-b" },
            },
          ],
          links: [],
        },
      },
      status: { endpoints: [], links: [] },
    });
    const store = useOrchestrator();
    store.activeTopologyId = "contest-a";

    await store.refreshCore();

    expect(apiMock.topology).toHaveBeenCalledWith(
      "contest-a",
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
    expect(store.deployments.find((item) => item.deployment_id === "judge-b")?.endpoint)
      .toBe("10.0.0.2:8080:judge");
    expect(store.deployments.find((item) => item.deployment_id === "judge-a")?.endpoint)
      .toBe("10.0.0.1:8080:judge");
    expect(store.services.map((item) => item.id)).toEqual(["judge-a", "judge-b"]);
  });

  it("discards a stale response after a forced refresh", async () => {
    const staleHealth = deferred<typeof oldHealth>();
    apiMock.health
      .mockReturnValueOnce(staleHealth.promise)
      .mockResolvedValueOnce(newHealth);
    const store = useOrchestrator();

    const stale = store.refreshCore();
    const staleSignal = apiMock.health.mock.calls[0]?.[0]?.signal as AbortSignal;
    const current = store.refreshCore(true);
    expect(staleSignal.aborted).toBe(true);
    await current;
    expect(store.health?.service).toBe("new");

    staleHealth.resolve(oldHealth);
    await stale;
    expect(store.health?.service).toBe("new");
  });

  it("invalidates an in-flight refresh and clears loading when polling stops", async () => {
    const health = deferred<typeof oldHealth>();
    apiMock.health.mockReturnValue(health.promise);
    const store = useOrchestrator();

    const refresh = store.refreshCore();
    const signal = apiMock.health.mock.calls[0]?.[0]?.signal as AbortSignal;
    expect(store.loading).toBe(true);
    store.stopPolling();
    expect(signal.aborted).toBe(true);
    expect(store.loading).toBe(false);

    health.resolve(oldHealth);
    await refresh;
    expect(store.health).toBeNull();
    expect(store.loading).toBe(false);
  });

  it("makes layout persistence failures visible", async () => {
    apiMock.putLayout.mockRejectedValue(new Error("磁盘已满"));
    const store = useOrchestrator();
    store.activeTopologyId = "primary";
    store.layout = { positions: { endpoint: { x: 1, y: 2 } } };

    await store.saveLayout();

    expect(store.layoutStatus).toBe("error");
    expect(store.layoutError).toContain("磁盘已满");
    expect(store.toasts.at(-1)?.text).toContain("布局保存失败");
    store.dispose();
  });

  it("treats an unbootstrapped Desktop catalog as an empty ready store", async () => {
    const store = useOrchestrator();
    store.capabilities = [{ action: "catalog.register" } as never];

    await store.refreshStore();

    expect(apiMock.storeIndex).not.toHaveBeenCalled();
    expect(store.storeIndex).toBeNull();
    expect(store.storeLoadStatus).toBe("ready");
    expect(store.storeError).toBe("");
  });

  it("keeps a simulated 30 minute polling session bounded and disposable", async () => {
    vi.useFakeTimers();
    apiMock.health.mockResolvedValue(newHealth);
    const store = useOrchestrator();

    store.startPolling();
    await vi.advanceTimersByTimeAsync(30 * 60 * 1000);

    expect(apiMock.health.mock.calls.length).toBeGreaterThan(400);
    expect(vi.getTimerCount()).toBe(1);
    store.dispose();
    expect(vi.getTimerCount()).toBe(0);
    vi.useRealTimers();
  });
});
