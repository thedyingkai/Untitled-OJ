import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  RequestCancelledError,
  RequestTimeoutError,
  api,
  normalizeStoreValidation,
  parseOperationEventStream,
  request,
} from "./api";

function abortablePendingFetch(_input: RequestInfo | URL, init?: RequestInit) {
  return new Promise<Response>((_resolve, reject) => {
    const signal = init?.signal;
    if (signal?.aborted) {
      reject(new DOMException("aborted", "AbortError"));
      return;
    }
    signal?.addEventListener(
      "abort",
      () => reject(new DOMException("aborted", "AbortError")),
      { once: true },
    );
  });
}

function v1Response(data: unknown, requestId = "req-test", status = 200) {
  return new Response(
    JSON.stringify({
      data,
      meta: { request_id: requestId, api_version: "v1" },
    }),
    { status, headers: { "content-type": "application/json" } },
  );
}

describe("bounded orchestrator API requests", () => {
  beforeEach(() => {
    vi.useRealTimers();
    delete window.__OJOS_AUTH_READY__;
    delete window.__OJOS_CSRF_TOKEN__;
    vi.restoreAllMocks();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("times out a daemon request that never settles", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("fetch", vi.fn(abortablePendingFetch));

    const pending = request("GET", "/api/v1/healthz/ready", undefined, { timeoutMs: 50 });
    const rejected = expect(pending).rejects.toBeInstanceOf(RequestTimeoutError);
    await vi.advanceTimersByTimeAsync(51);
    await rejected;
  });

  it("propagates caller cancellation without reporting the daemon offline", async () => {
    vi.stubGlobal("fetch", vi.fn(abortablePendingFetch));
    const controller = new AbortController();

    const pending = request("GET", "/api/v1/operations", undefined, {
      signal: controller.signal,
    });
    controller.abort("component unmounted");

    await expect(pending).rejects.toBeInstanceOf(RequestCancelledError);
  });

  it("surfaces problem+json details and the request id", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            type: "about:blank",
            title: "Conflict",
            detail: "revision is stale",
            code: "REVISION_CONFLICT",
          }),
          {
            status: 409,
            headers: {
              "content-type": "application/problem+json",
              "x-request-id": "req-test",
            },
          },
        ),
      ),
    );

    const error = await request("GET", "/api/v1/topologies/default")
      .then(() => null)
      .catch((value) => value as ApiError);
    expect(error).toBeInstanceOf(ApiError);
    expect(error?.message).toBe("revision is stale");
    expect(error?.code).toBe("REVISION_CONFLICT");
    expect(error?.requestId).toBe("req-test");
  });

  it("adds memory CSRF and idempotency headers to mutations", async () => {
    window.__OJOS_CSRF_TOKEN__ = "csrf-memory-only";
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ data: {}, meta: { request_id: "req-1", api_version: "v1" } }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await request("POST", "/api/v1/deployments/deployment-1:start", {});

    const init = fetchMock.mock.calls[0]?.[1] as RequestInit;
    const headers = init.headers as Record<string, string>;
    expect(headers["x-csrf-token"]).toBe("csrf-memory-only");
    expect(headers["Idempotency-Key"]).toBeTruthy();
  });

  it("forwards a write-only Catalog bootstrap public key", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(v1Response({ source: { id: "bootstrap" } }, "req-catalog", 201));
    vi.stubGlobal("fetch", fetchMock);

    await api.registerCatalog({
      id: "bootstrap",
      url: "https://catalog.example/v2.json",
      required_key_id: "bootstrap-key",
      public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    });

    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/store/catalogs");
    const init = fetchMock.mock.calls[0]?.[1] as RequestInit;
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      id: "bootstrap",
      url: "https://catalog.example/v2.json",
      required_key_id: "bootstrap-key",
      public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    });
  });

  it("rejects a legacy-shaped success instead of treating it as an empty v1 result", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            status: "ok",
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      ),
    );

    await expect(api.capabilities()).rejects.toMatchObject({
      code: "INVALID_V1_ENVELOPE",
    });
  });

  it("parses resumable SSE cursors and retry hints", () => {
    const batch = parseOperationEventStream(
      'id: cursor-1\nevent: job\ndata: {"event":{"job_id":"job-1","sequence":1}}\n\nretry: 1500\n\n',
    );
    expect(batch.lastEventId).toBe("cursor-1");
    expect(batch.retryMs).toBe(1500);
    expect(batch.events).toHaveLength(1);
    expect(batch.events[0]?.event).toBe("job");
  });

  it("reads operations from the v1 envelope", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          data: {
            items: [
              {
                operation_id: "op-1",
                action: "deployment.restart",
                target_id: "deployment-1",
                status: "RUNNING",
                result: {},
                planned_jobs: [],
                created_at_ms: 1000,
                updated_at_ms: 2000,
              },
            ],
            next_cursor: null,
          },
          meta: { request_id: "req-op", api_version: "v1" },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    const operations = await api.operations();
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/operations?limit=200");
    expect(operations[0]).toMatchObject({
      operation_id: "op-1",
      target: "deployment-1",
      status: "RUNNING",
    });
  });

  it("normalizes binding candidates without silently recommending an ambiguous provider", () => {
    const result = normalizeStoreValidation({
      valid: false,
      catalog_id: "production",
      target_platform: { os: "linux", arch: "amd64" },
      requirements: [
        {
          name: "judge_control",
          api_id: "judge.worker.control",
          version: ">=1.0.0 <2.0.0",
          candidates: [
            {
              deployment_id: "judge-a",
              service_id: "judge-api",
              node_id: "node-a",
              api_version: "1.0.0",
              healthy: true,
            },
            {
              deployment_id: "judge-b",
              service_id: "judge-api",
              node_id: "node-b",
              api_version: "1.0.0",
              healthy: true,
            },
          ],
        },
      ],
      side_effects: {},
      runtime: {
        node_id: "node-b",
        contract: {
          id: "judge-sandbox-v1",
          profile_sha256: `sha256:${"a".repeat(64)}`,
        },
        facts: {
          report_id: "report-node-b-1",
          observed_at_ms: 123,
          agent_version: "1.0.0",
          runtime_policy_sha256: `sha256:${"b".repeat(64)}`,
          allowed_contracts: [],
          judge_sandbox_allowed_images: [
            `registry.example/judge-worker@sha256:${"c".repeat(64)}`,
          ],
          inventory_complete: true,
          inventory_error: "",
          docker: {
            engine: "docker",
            server_version: "28.0.0",
            os_type: "linux",
            architecture: "amd64",
            cgroup_version: "2",
          },
        },
      },
    });

    expect(result.requirements[0]).toMatchObject({
      name: "judge_control",
      ambiguous: true,
      recommended_provider_deployment_id: "",
    });
    expect(result.requirements[0]?.candidates).toHaveLength(2);
    expect(result.runtime?.selected_contract?.id).toBe("judge-sandbox-v1");
    expect(result.runtime?.docker.cgroup_version).toBe("2");
    expect(result.runtime?.report_id).toBe("report-node-b-1");
    expect(result.runtime?.inventory_complete).toBe(true);
    expect(result.runtime?.judge_sandbox_allowed_images).toHaveLength(1);
  });

  it("uses the deployment binding endpoint and preserves generations and drift", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      v1Response({
        deployment_id: "worker-b",
        service_id: "judge-worker",
        items: [
          {
            binding_id: "binding-1",
            requirement_name: "judge_control",
            api_id: "judge.worker.control",
            provider_deployment_id: "judge-a",
            credential_generation: 4,
            context_generation: 7,
            health: "DEGRADED",
            drift: ["provider endpoint changed"],
          },
        ],
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const result = await api.deploymentBindings("worker-b");

    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/v1/deployments/worker-b/bindings",
    );
    expect(result.items[0]).toMatchObject({
      provider_deployment_id: "judge-a",
      credential_generation: 4,
      context_generation: 7,
      drift: ["provider endpoint changed"],
    });
  });

  it("sends explicit bindings and topology revision in validation and install", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        v1Response({
          valid: true,
          target_platform: { os: "linux", arch: "amd64" },
          bindings: [],
          side_effects: {},
        }),
      )
      .mockResolvedValueOnce(v1Response({ operation_id: "op-install" }, "req-install", 202));
    vi.stubGlobal("fetch", fetchMock);
    const selection = {
      bindings: [
        { name: "judge_control", provider_deployment_id: "judge-a" },
      ],
      topology_id: "primary",
      topology_etag: '"revision-7"',
    };
    const pipeline = {
      start: false,
      migration_policy: "DRY_RUN" as const,
      gateway_node_id: "gateway-a",
      config: { namespace: "contest" },
      secret_refs: { signing_key: "secrets/judge/signing-key" },
    };

    await api.storeValidate({
      service_id: "judge-worker",
      target_node_id: "node-b",
      ...selection,
      ...pipeline,
    });
    await api.storeInstall({
      service_id: "judge-worker",
      target_node_id: "node-b",
      ...selection,
      ...pipeline,
    });

    for (const call of fetchMock.mock.calls) {
      expect(JSON.parse(String((call[1] as RequestInit).body))).toMatchObject({
        ...selection,
        ...pipeline,
      });
      expect(((call[1] as RequestInit).headers as Record<string, string>)["Idempotency-Key"])
        .toBeTruthy();
    }
  });

  it("uses the published Node enrollment and certificate revocation routes", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        v1Response({
          code_id: "enroll-1",
          node_id: "edge-1",
          enrollment_code: "ojos_enroll_secret",
          expires_at_ms: 1234,
        }, "req-enroll", 201),
      )
      .mockResolvedValueOnce(
        v1Response({
          node_id: "edge-1",
          certificate_status: "REVOKED",
          revoked_certificates: 1,
        }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await api.createNodeEnrollment({
      node_id: "edge-1",
      host_ip: "10.0.0.21",
      ttl_seconds: 600,
    });
    await api.revokeNodeCertificates("edge-1", "node retired");

    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/nodes/enrollment-codes");
    expect(JSON.parse((fetchMock.mock.calls[0]?.[1] as RequestInit).body as string)).toMatchObject({
      node_id: "edge-1",
      host_ip: "10.0.0.21",
    });
    expect(fetchMock.mock.calls[1]?.[0]).toBe(
      "/api/v1/nodes/edge-1:revoke-certificates",
    );
    expect((fetchMock.mock.calls[1]?.[1] as RequestInit).method).toBe("POST");
    expect(
      JSON.parse((fetchMock.mock.calls[1]?.[1] as RequestInit).body as string),
    ).toEqual({ reason: "node retired" });
  });

  it("installs a catalog release as Managed with start=true by default", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      v1Response(
        { operation_id: "op-install", deployment_id: "deployment-1" },
        "req-install",
        202,
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    const result = await api.storeInstall({
      service_id: "judge-api",
      version: "1.2.3",
      catalog_source_id: "official",
      channel: "stable",
      target_node_id: "node-1",
    });

    expect(result.operation_id).toBe("op-install");
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/store/releases:install");
    const init = fetchMock.mock.calls[0]?.[1] as RequestInit;
    expect(init.method).toBe("POST");
    expect(init.headers).toMatchObject({ "Idempotency-Key": expect.any(String) });
    expect(JSON.parse(String(init.body))).toEqual({
      mode: "MANAGED",
      start: true,
      migration_policy: "APPLY",
      service_id: "judge-api",
      version: "1.2.3",
      catalog_source_id: "official",
      channel: "stable",
      target_node_id: "node-1",
      config: {},
      secret_refs: {},
    });
  });

  it("validates a trusted Catalog release without runtime side effects", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      v1Response({
        valid: true,
        catalog_source_id: "official",
        catalog_id: "ojos-official",
        verified_key_ids: ["release-key-1"],
        target_platform: { os: "linux", arch: "x86_64" },
        plan: { order: ["judge-api"] },
        metadata: [],
        side_effects: {
          release_imports: 0,
          operations: 0,
          jobs: 0,
          runtime_calls: 0,
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const result = await api.storeValidate({
      service_id: "judge-api",
      version: "1.2.3",
      catalog_source_id: "official",
      channel: "stable",
      target_node_id: "node-1",
    });

    expect(result.valid).toBe(true);
    expect(result.side_effects.runtime_calls).toBe(0);
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/store/releases:validate");
    expect((fetchMock.mock.calls[0]?.[1] as RequestInit).headers).toMatchObject({
      "Idempotency-Key": expect.any(String),
    });
    expect(JSON.parse(String((fetchMock.mock.calls[0]?.[1] as RequestInit).body))).toMatchObject({
      start: true,
      migration_policy: "APPLY",
      config: {},
      secret_refs: {},
    });
  });

  it("imports and deletes Release metadata separately from replacement sagas", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(v1Response({ imported: [] }, "req-import", 201))
      .mockResolvedValueOnce(v1Response({ operation_id: "op-upgrade" }, "req-upgrade", 202))
      .mockResolvedValueOnce(v1Response({ operation_id: "op-rollback" }, "req-rollback", 202))
      .mockResolvedValueOnce(v1Response({ deleted: true }, "req-delete", 200));
    vi.stubGlobal("fetch", fetchMock);

    await api.storeImport({
      service_id: "judge-api",
      version: "1.2.3",
      catalog_source_id: "official",
      channel: "stable",
      target_node_id: "node-1",
    });
    await api.storeUpgrade({
      deployment_id: "deployment-1",
      bindings: [
        { name: "storage_get", provider_deployment_id: "storage-a" },
      ],
    });
    await api.storeRollback({ deployment_id: "deployment-2" });
    await api.deleteRelease("judge-api", "1.2.3");

    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "/api/v1/store/releases:import",
      "/api/v1/store/releases:upgrade",
      "/api/v1/store/releases:rollback",
      "/api/v1/store/releases:delete",
    ]);
    expect(JSON.parse(String((fetchMock.mock.calls[0]?.[1] as RequestInit).body))).toEqual({
      service_id: "judge-api",
      version: "1.2.3",
      catalog_source_id: "official",
      channel: "stable",
      target_node_id: "node-1",
    });
    expect(JSON.parse(String((fetchMock.mock.calls[3]?.[1] as RequestInit).body))).toEqual({
      service_id: "judge-api",
      version: "1.2.3",
    });
    expect(JSON.parse(String((fetchMock.mock.calls[1]?.[1] as RequestInit).body))).toEqual({
      deployment_id: "deployment-1",
      bindings: [
        { name: "storage_get", provider_deployment_id: "storage-a" },
      ],
    });
    for (const call of fetchMock.mock.calls) {
      expect((call[1] as RequestInit).headers).toMatchObject({
        "Idempotency-Key": expect.any(String),
      });
    }
  });

  it("uses immutable topology revisions, If-Match, diff, apply, rollback and status", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(v1Response({ revision: { revision_id: "rev-2" } }, "req-r2", 201))
      .mockResolvedValueOnce(
        v1Response({ valid: true, content_sha256: "sha256:spec" }, "req-validate"),
      )
      .mockResolvedValueOnce(
        v1Response({ diff: { from_revision_id: "rev-1", to_revision_id: "rev-2", changes: [] } }),
      )
      .mockResolvedValueOnce(v1Response({ operation_id: "op-apply", revision_id: "rev-2" }, "req-apply", 202))
      .mockResolvedValueOnce(v1Response({ operation_id: "op-rollback", revision_id: "rev-3" }, "req-rollback", 202))
      .mockResolvedValueOnce(
        v1Response({
          status: {
            topology_id: "primary",
            desired_revision_id: "rev-2",
            observed_revision_id: "rev-2",
            state: "IN_SYNC",
            deployments: [],
            endpoints: [],
            links: [],
            drift: [],
            last_operation_id: "op-apply",
            updated_at: "unix-ms:1",
          },
        }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const spec = {
      api_version: "v1" as const,
      topology_id: "primary",
      root_endpoint: "127.0.0.1:8080:judge-api",
      authority: {
        root_endpoint: "127.0.0.1:8080:judge-api",
        exposure_policy: "internal",
      },
      endpoints: [
        {
          endpoint: "127.0.0.1:8080:judge-api",
          service_id: "judge-api",
          protocol: "http",
          health_path: "/health",
          display_name: "judge-api",
          note: "",
          config: {},
        },
      ],
      links: [],
    };

    await api.topologyCreateRevision("primary", spec, "rev-1");
    await api.topologyValidate("primary", spec);
    await api.topologyDiff("primary", {
      from_revision_id: "rev-1",
      to_revision_id: "rev-2",
    });
    await api.topologyApply("primary", "rev-2");
    await api.topologyRollback("primary", "rev-2", "rev-1");
    const status = await api.topologyStatus("primary");

    expect(status.state).toBe("IN_SYNC");
    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "/api/v1/topologies/primary/revisions",
      "/api/v1/topologies/primary:validate",
      "/api/v1/topologies/primary:diff",
      "/api/v1/topologies/primary:apply",
      "/api/v1/topologies/primary:rollback",
      "/api/v1/topologies/primary/status",
    ]);
    const revisionHeaders = (fetchMock.mock.calls[0]?.[1] as RequestInit)
      .headers as Record<string, string>;
    const applyHeaders = (fetchMock.mock.calls[3]?.[1] as RequestInit)
      .headers as Record<string, string>;
    const rollbackHeaders = (fetchMock.mock.calls[4]?.[1] as RequestInit)
      .headers as Record<string, string>;
    expect(revisionHeaders["If-Match"]).toBe('"rev-1"');
    expect(applyHeaders["If-Match"]).toBe('"rev-2"');
    expect(rollbackHeaders["If-Match"]).toBe('"rev-2"');
    expect(JSON.parse(String((fetchMock.mock.calls[4]?.[1] as RequestInit).body))).toEqual({
      revision_id: "rev-1",
    });
  });

  it("follows deployment cursors and normalizes durable runtime projections", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        v1Response({
          items: [
            {
              node_id: "node-1",
              instance: {
                deployment_id: "deployment-1",
                service_id: "judge-api",
                container_id: "container-1",
                artifact_digest: `sha256:${"a".repeat(64)}`,
                desired_state: "RUNNING",
                observed_state: "RUNNING",
                health: "HEALTHY",
              },
              last_observed_at_ms: 1_000,
              drift_reason: "",
              credential_expires_at_ms: 901_000,
              credential_last_success_at_ms: 1_000,
              credential_last_error: "",
              updated_at: "unix-ms:1",
            },
          ],
          next_cursor: "deployment-1",
        }),
      )
      .mockResolvedValueOnce(v1Response({ items: [], next_cursor: null }));
    vi.stubGlobal("fetch", fetchMock);

    const deployments = await api.deployments();
    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "/api/v1/deployments?limit=200",
      "/api/v1/deployments?limit=200&cursor=deployment-1",
    ]);
    expect(deployments[0]).toMatchObject({
      deployment_id: "deployment-1",
      node_id: "node-1",
      service_id: "judge-api",
      observed_state: "RUNNING",
      endpoint_health: "HEALTHY",
      last_observed_at_ms: 1_000,
      credential_expires_at_ms: 901_000,
      credential_last_success_at_ms: 1_000,
    });
  });

  it("uses only v1 Node list and lifecycle routes", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        v1Response({
          nodes: [
            {
              node_id: "node-1",
              host_ip: "127.0.0.1",
              role: "worker",
              labels: { runtime: "docker" },
              status: "READY",
            },
          ],
        }),
      )
      .mockResolvedValueOnce(v1Response({ operation_id: "op-drain" }, "req-drain", 202))
      .mockResolvedValueOnce(v1Response({ operation_id: "op-remove" }, "req-remove", 202));
    vi.stubGlobal("fetch", fetchMock);

    expect((await api.nodes())[0]?.status).toBe("READY");
    expect((await api.nodeDrain("node-1")).operation_id).toBe("op-drain");
    expect((await api.nodeRemove("node-1")).operation_id).toBe("op-remove");
    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "/api/v1/nodes?limit=200",
      "/api/v1/nodes/node-1:drain",
      "/api/v1/nodes/node-1",
    ]);
  });

  it("persists per-user topology layout only through the v1 envelope", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        v1Response({ layout: { positions: { endpoint: { x: 10, y: 20 } } } }),
      )
      .mockResolvedValueOnce(
        v1Response({ layout: { positions: { endpoint: { x: 30, y: 40 } } } }),
      );
    vi.stubGlobal("fetch", fetchMock);

    expect(await api.getLayout("secondary-1")).toEqual({
      positions: { endpoint: { x: 10, y: 20 } },
    });
    await api.putLayout("secondary-1", {
      positions: { endpoint: { x: 30, y: 40 } },
    });

    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "/api/v1/ui/layout?topology_id=secondary-1",
      "/api/v1/ui/layout?topology_id=secondary-1",
    ]);
    const put = fetchMock.mock.calls[1]?.[1] as RequestInit;
    expect(put.method).toBe("PUT");
    expect(put.headers).toMatchObject({ "Idempotency-Key": expect.any(String) });
  });

  it("creates an honest current-topology diagnostic without overloading operation_id", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      v1Response({ action_result: { status: "SUCCEEDED" } }, "req-diagnostic", 201),
    );
    vi.stubGlobal("fetch", fetchMock);

    await api.createDiagnostic();

    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/diagnostics");
    const init = fetchMock.mock.calls[0]?.[1] as RequestInit;
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({});
    expect(JSON.parse(String(init.body))).not.toHaveProperty("operation_id");
  });
});
