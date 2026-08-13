import { createServer } from "node:http";
import { readFileSync, statSync } from "node:fs";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseYaml } from "yaml";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const dist = resolve(root, "dist");
const actionMatrix = parseYaml(
  readFileSync(
    resolve(root, "../../platform/schemas/orchestrator/actions-v1.yaml"),
    "utf8",
  ),
);
const port = Number(process.env.OJOS_E2E_PORT || 4174);

const mimeTypes = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
};

function topologySpec(version) {
  const gateway = {
    endpoint: "127.0.0.1:8080:gateway",
    service_id: "gateway",
    protocol: "http",
    health_path: "/health",
    display_name: "Gateway",
    note: "",
    config: { deployment_id: "dep-gateway" },
  };
  const database = {
    endpoint: "127.0.0.1:5432:database",
    service_id: "database",
    protocol: "postgres",
    health_path: "",
    display_name: "Database",
    note: "",
    config: { deployment_id: "dep-database" },
  };
  return {
    api_version: "v1",
    topology_id: "primary",
    root_endpoint: gateway.endpoint,
    authority: {
      root_endpoint: gateway.endpoint,
      exposure_policy: "internal",
    },
    endpoints: version === 1 ? [gateway] : [gateway, database],
    links:
      version === 1
        ? []
        : [
            {
              source_endpoint: gateway.endpoint,
              target_endpoint: database.endpoint,
              protocol: "postgres",
              auth_mode: "secret_ref",
              scope: "read-write",
              enabled: true,
              config_ref: "",
              secret_ref: "db/main",
              policy: {},
              api_bindings: [
                {
                  requirement: "database_control",
                  api_id: "database.control",
                  version: ">=1.0.0 <2.0.0",
                  optional: false,
                  provider_deployment_id: "dep-database",
                  selection: "explicit",
                },
              ],
            },
          ],
  };
}

function makeRevision(number, spec, overrides = {}) {
  return {
    topology_id: "primary",
    revision_number: number,
    revision_id: `rev-${number}`,
    parent_revision_id: number > 1 ? `rev-${number - 1}` : null,
    rollback_of_revision_id: null,
    content_sha256: `sha256-revision-${number}`,
    spec: structuredClone(spec),
    created_at: `2026-08-03T00:00:0${number}Z`,
    created_by: "e2e-admin",
    message: number === 1 ? "initial topology" : "database link draft",
    ...overrides,
  };
}

function initialMetrics() {
  return {
    activeRequests: 0,
    maxConcurrentRequests: 0,
    requestCount: 0,
    abortedRequests: 0,
    corePollRequests: 0,
    eventRequests: 0,
    paths: {},
    layoutPutPaths: [],
  };
}

function initialState() {
  const rev1 = makeRevision(1, topologySpec(1));
  const rev2 = makeRevision(2, topologySpec(2));
  const secondarySpec = structuredClone(topologySpec(1));
  secondarySpec.topology_id = "contest-a";
  const secondaryRevision = {
    ...makeRevision(1, secondarySpec),
    topology_id: "contest-a",
    revision_id: "contest-rev-1",
    content_sha256: "sha256-contest-revision-1",
    spec: secondarySpec,
    message: "contest A topology",
  };
  const secondaryHeads = {
    topology_id: "contest-a",
    draft_revision_id: "contest-rev-1",
    applied_revision_id: "contest-rev-1",
    applying_revision_id: null,
    applying_operation_id: null,
    last_operation_id: null,
  };
  const secondaryStatus = {
    topology_id: "contest-a",
    desired_revision_id: "contest-rev-1",
    observed_revision_id: "contest-rev-1",
    state: "IN_SYNC",
    deployments: [],
    endpoints: [],
    links: [],
    drift: [],
    last_operation_id: null,
    updated_at: "2026-08-03T00:00:00Z",
  };
  return {
    scenario: {
      failNextInstall: false,
      failLayoutSave: false,
      activeBindingConflict: true,
      denyPath: "",
      sseDelayMs: 0,
    },
    nodes: [
      {
        node_id: "desktop-local",
        host_ip: "127.0.0.1",
        parent_node_id: "",
        role: "standalone",
        labels: { platform: "linux/amd64" },
        status: "READY",
        created_at: "2026-08-03T00:00:00Z",
        updated_at: "2026-08-03T00:00:00Z",
      },
      {
        node_id: "edge-node-01",
        host_ip: "10.0.0.21",
        parent_node_id: "",
        role: "standalone",
        labels: { platform: "linux/amd64" },
        status: "READY",
        created_at: "2026-08-03T00:00:00Z",
        updated_at: "2026-08-03T00:00:00Z",
      },
    ],
    catalogs: [
      {
        id: "trusted-e2e",
        url: "https://catalog.example/e2e-v2.json",
        required_key_id: "e2e-ed25519-key",
        auth_secret_ref: "",
        enabled: true,
      },
    ],
    deployments: [
      {
        deployment_id: "dep-gateway",
        node_id: "desktop-local",
        service_id: "gateway",
        name: "Gateway",
        version: "1.0.0",
        kind: "gateway",
        runtime: "docker",
        status: "RUNNING",
        container_id: "container-gateway",
        artifact_digest: "sha256:gateway",
        desired_state: "RUNNING",
        observed_state: "RUNNING",
        health: "HEALTHY",
        endpoint: "127.0.0.1:8080:gateway",
        endpoints: ["127.0.0.1:8080:gateway"],
        updated_at: "2026-08-03T00:00:00Z",
      },
      {
        deployment_id: "dep-database",
        node_id: "desktop-local",
        service_id: "database",
        name: "Database",
        version: "16.4.0",
        kind: "database",
        runtime: "docker",
        status: "RUNNING",
        container_id: "container-database",
        artifact_digest: "sha256:database",
        desired_state: "RUNNING",
        observed_state: "RUNNING",
        health: "HEALTHY",
        endpoint: "127.0.0.1:5432:database",
        endpoints: ["127.0.0.1:5432:database"],
        updated_at: "2026-08-03T00:00:00Z",
      },
      {
        deployment_id: "dep-worker",
        node_id: "desktop-local",
        service_id: "worker",
        name: "Worker",
        version: "1.1.0",
        kind: "backend-worker",
        runtime: "docker",
        status: "RUNNING",
        container_id: "container-worker",
        artifact_digest: "sha256:worker",
        desired_state: "RUNNING",
        observed_state: "RUNNING",
        health: "HEALTHY",
        updated_at: "2026-08-03T00:00:00Z",
      },
    ],
    packages: [
      {
        module_id: "e2e-api",
        name: "E2E API",
        description: "Signed catalog fixture used by the browser release saga",
        kind: "backend-api",
        tags: ["e2e", "signed"],
        metadata_sha256:
          "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        version: "1.2.3",
        channel: "stable",
        platforms: [{ os: "linux", arch: "amd64" }],
        min_orchestrator_version: "1.0.0",
        oci_image: "registry.example/e2e-api@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        source_id: "trusted-e2e",
        catalog_id: "catalog-e2e-v2",
      },
      {
        module_id: "e2e-provider",
        name: "E2E Provider",
        description: "Signed provider-only fixture with no required API bindings",
        kind: "backend-api",
        tags: ["e2e", "provider"],
        metadata_sha256:
          "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        version: "1.0.0",
        channel: "stable",
        platforms: [{ os: "linux", arch: "amd64" }],
        min_orchestrator_version: "1.0.0",
        oci_image: "registry.example/e2e-provider@sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        source_id: "trusted-e2e",
        catalog_id: "catalog-e2e-v2",
      },
    ],
    revisions: [rev1, rev2],
    secondaryRevision,
    secondaryHeads,
    secondaryStatus,
    heads: {
      topology_id: "primary",
      draft_revision_id: "rev-2",
      applied_revision_id: "rev-1",
      applying_revision_id: null,
      applying_operation_id: null,
      last_operation_id: null,
    },
    topologyStatus: {
      topology_id: "primary",
      desired_revision_id: "rev-2",
      observed_revision_id: "rev-1",
      state: "DRIFTED",
      deployments: [],
      endpoints: [
        {
          endpoint: "127.0.0.1:8080:gateway",
          health: "HEALTHY",
          reachable: true,
          latency_ms: 4,
          message: "ready",
          observed_at: "2026-08-03T00:00:00Z",
        },
        {
          endpoint: "127.0.0.1:5432:database",
          health: "HEALTHY",
          reachable: true,
          latency_ms: 3,
          message: "ready",
          observed_at: "2026-08-03T00:00:00Z",
        },
      ],
      links: [
        {
          source_endpoint: "127.0.0.1:8080:gateway",
          target_endpoint: "127.0.0.1:5432:database",
          health: "HEALTHY",
          latency_ms: 8,
          message: "reachable",
          observed_at: "2026-08-03T00:00:00Z",
        },
      ],
      drift: [
        {
          resource_kind: "TopologyRevision",
          resource_id: "primary",
          kind: "REVISION_MISMATCH",
          detail: "draft rev-2 has not been applied",
        },
      ],
      last_operation_id: null,
      updated_at: "2026-08-03T00:00:00Z",
    },
    operations: [
      {
        operation_id: "op-failed",
        action: "release.install",
        target: "failed-service",
        status: "FAILED",
        risk: "MEDIUM",
        requires_confirmation: false,
        rollback_available: false,
        error: "health probe failed; compensation completed",
        log_count: 2,
        summary: "failed install was compensated",
        created_at: "2026-08-03T00:00:00Z",
        updated_at: "2026-08-03T00:00:02Z",
      },
      {
        operation_id: "op-running",
        action: "deployment.restart",
        target: "dep-worker",
        status: "RUNNING",
        risk: "LOW",
        requires_confirmation: false,
        rollback_available: false,
        log_count: 1,
        summary: "worker restart in progress",
        created_at: "2026-08-03T00:00:00Z",
        updated_at: "2026-08-03T00:00:01Z",
      },
    ],
    logs: {
      "op-failed": [
        {
          operation_id: "op-failed",
          step_id: "health",
          level: "error",
          message: "health probe failed",
          created_at: "2026-08-03T00:00:01Z",
        },
        {
          operation_id: "op-failed",
          step_id: "compensate",
          level: "info",
          message: "container and endpoint compensation completed",
          created_at: "2026-08-03T00:00:02Z",
        },
      ],
      "op-running": [
        {
          operation_id: "op-running",
          step_id: "restart",
          level: "info",
          message: "restart lease is active",
          created_at: "2026-08-03T00:00:01Z",
        },
      ],
    },
    layouts: {
      "e2e-admin:primary": { positions: {} },
      "e2e-admin:contest-a": { positions: {} },
    },
    diagnostics: [
      {
        report_id: "diag-1",
        operation_id: "op-failed",
        status: "READY",
        summary: "failed install and topology drift snapshot",
        created_at: "2026-08-03T00:00:03Z",
        topology_id: "primary",
      },
    ],
    compensations: [],
    capturedMutations: [],
    contractViolations: [],
    eventSequence: 0,
  };
}

let state = initialState();
let metrics = initialMetrics();
let requestSequence = 0;

function latestRevision() {
  return state.revisions.find(
    (revision) => revision.revision_id === state.heads.draft_revision_id,
  );
}

function trackRequest(req, res, pathname) {
  if (pathname.startsWith("/__e2e/")) return;
  metrics.activeRequests += 1;
  metrics.requestCount += 1;
  metrics.maxConcurrentRequests = Math.max(
    metrics.maxConcurrentRequests,
    metrics.activeRequests,
  );
  metrics.paths[pathname] = (metrics.paths[pathname] || 0) + 1;
  if (
    [
      "/api/v1/healthz/ready",
      "/api/v1/capabilities",
      "/api/v1/nodes",
      "/api/v1/deployments",
      "/api/v1/topologies/primary",
      "/api/v1/operations",
    ].includes(pathname)
  ) {
    metrics.corePollRequests += 1;
  }
  if (/\/api\/v1\/operations\/[^/]+\/events$/.test(pathname)) {
    metrics.eventRequests += 1;
  }
  let complete = false;
  res.once("finish", () => {
    complete = true;
    metrics.activeRequests -= 1;
  });
  res.once("close", () => {
    if (!complete) {
      metrics.activeRequests -= 1;
      metrics.abortedRequests += 1;
    }
  });
}

function sendJson(res, status, body, contentType = "application/json; charset=utf-8") {
  const json = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": contentType,
    "content-length": Buffer.byteLength(json),
    "cache-control": "no-store",
    "x-request-id": `e2e-${++requestSequence}`,
  });
  res.end(json);
}

function envelope(res, data, status = 200) {
  sendJson(res, status, {
    data,
    meta: { request_id: `e2e-envelope-${requestSequence + 1}`, api_version: "v1" },
  });
}

function problem(res, status, code, detail) {
  sendJson(
    res,
    status,
    {
      type: `urn:ojos:problem:${code.toLowerCase()}`,
      title: code,
      status,
      code,
      detail,
      request_id: `e2e-problem-${requestSequence + 1}`,
    },
    "application/problem+json; charset=utf-8",
  );
}

async function readJson(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  if (!chunks.length) return {};
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function installedProjection() {
  const installed = {};
  for (const deployment of state.deployments) {
    const entry = (installed[deployment.service_id] ||= {
      version: deployment.version,
      versions: [deployment.version],
      kind: deployment.kind,
      deployments: [],
    });
    entry.deployments.push({
      deployment_id: deployment.deployment_id,
      node_id: deployment.node_id,
      version: deployment.version,
      host_ip: "127.0.0.1",
      status: deployment.observed_state,
    });
  }
  return installed;
}

function appendOperation(operation) {
  state.operations.unshift(operation);
  state.logs[operation.operation_id] ||= [];
}

function asyncOperation(id, action, target, status = "SUCCEEDED") {
  const operation = {
    operation_id: id,
    action,
    target,
    status,
    risk: "MEDIUM",
    requires_confirmation: false,
    rollback_available: status === "SUCCEEDED",
    log_count: 1,
    summary: `${action} ${status.toLowerCase()}`,
    created_at: "2026-08-03T00:00:10Z",
    updated_at: "2026-08-03T00:00:11Z",
  };
  appendOperation(operation);
  state.logs[id] = [
    {
      operation_id: id,
      step_id: "runtime",
      level: "info",
      message: `${action} reached ${status}`,
      created_at: "2026-08-03T00:00:11Z",
    },
  ];
  return operation;
}

function captureMutation(req, pathname, body) {
  const entry = {
    method: req.method,
    path: pathname,
    body,
    idempotency_key: req.headers["idempotency-key"] || "",
    if_match: req.headers["if-match"] || "",
  };
  state.capturedMutations.push(entry);
  if (!entry.idempotency_key) {
    state.contractViolations.push(`${req.method} ${pathname} omitted Idempotency-Key`);
  }
  return entry;
}

async function handleApi(req, res, url) {
  const { pathname } = url;
  const mutation = !["GET", "HEAD", "OPTIONS"].includes(req.method || "GET");
  let body = {};
  if (mutation) {
    body = await readJson(req);
    captureMutation(req, pathname, body);
    if (state.scenario.denyPath === pathname) {
      state.scenario.denyPath = "";
      problem(
        res,
        403,
        "RBAC_DENIED",
        "viewer role cannot mutate this topology",
      );
      return;
    }
  }

  if (req.method === "GET" && pathname === "/api/v1/auth/config") {
    envelope(res, { mode: "development" });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/healthz/ready") {
    envelope(res, {
      status: "ready",
      service: "orchestrator",
      store: "persistent",
      warnings: [],
    });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/contributions/snapshot") {
    envelope(res, {
      schema_version: "ojos.dev/contribution-snapshot/v1",
      digest: `sha256:${"0".repeat(64)}`,
      scope_id: "default",
      acknowledgements: [],
      revisions: [],
      api_surfaces: [],
      gateway_routes: [],
      permission_definitions: [],
      user_frontend_modules: [],
      admin_frontend_modules: [],
    });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/capabilities") {
    envelope(res, {
      actions: actionMatrix.actions.map((action) => ({
        action: action.action,
        target_type: action.target_type,
        capability_status: "AVAILABLE",
        required_permission: actionMatrix.roles[action.role],
      })),
    });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/nodes") {
    envelope(res, { items: state.nodes, next_cursor: null });
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/nodes/enrollment-codes") {
    envelope(
      res,
      {
        code_id: `enrollment-${body.node_id}`,
        node_id: body.node_id,
        enrollment_code: `one-time-${body.node_id}`,
        expires_at_ms: Date.parse("2026-08-03T01:00:00Z"),
      },
      201,
    );
    return;
  }
  const nodeHealthMatch = pathname.match(/^\/api\/v1\/nodes\/([^/]+)\/health$/);
  if (req.method === "GET" && nodeHealthMatch) {
    const nodeId = decodeURIComponent(nodeHealthMatch[1]);
    const node = state.nodes.find((candidate) => candidate.node_id === nodeId);
    if (!node) {
      problem(res, 404, "NODE_NOT_FOUND", "node does not exist");
      return;
    }
    envelope(res, {
      node_id: nodeId,
      status: node.status,
      reachable: true,
      last_heartbeat_at: "2026-08-03T00:00:30Z",
    });
    return;
  }
  const nodeRevokeMatch = pathname.match(
    /^\/api\/v1\/nodes\/([^/]+):revoke-certificates$/,
  );
  if (req.method === "POST" && nodeRevokeMatch) {
    const nodeId = decodeURIComponent(nodeRevokeMatch[1]);
    if (!state.nodes.some((candidate) => candidate.node_id === nodeId)) {
      problem(res, 404, "NODE_NOT_FOUND", "node does not exist");
      return;
    }
    envelope(res, {
      node_id: nodeId,
      certificate_status: "REVOKED",
      revoked_certificates: 1,
      reason: body.reason,
    });
    return;
  }
  const nodeDrainMatch = pathname.match(/^\/api\/v1\/nodes\/([^/]+):drain$/);
  if (req.method === "POST" && nodeDrainMatch) {
    const nodeId = decodeURIComponent(nodeDrainMatch[1]);
    const node = state.nodes.find((candidate) => candidate.node_id === nodeId);
    if (!node) {
      problem(res, 404, "NODE_NOT_FOUND", "node does not exist");
      return;
    }
    node.status = "DRAINED";
    const operationId = `op-node-drain-${state.operations.length + 1}`;
    asyncOperation(operationId, "node.drain", nodeId);
    envelope(res, { operation_id: operationId, node_id: nodeId }, 202);
    return;
  }
  const nodeRemoveMatch = pathname.match(/^\/api\/v1\/nodes\/([^/]+)$/);
  if (req.method === "DELETE" && nodeRemoveMatch) {
    const nodeId = decodeURIComponent(nodeRemoveMatch[1]);
    const node = state.nodes.find((candidate) => candidate.node_id === nodeId);
    if (!node || node.status !== "DRAINED") {
      problem(res, 409, "NODE_NOT_DRAINED", "node must be drained before removal");
      return;
    }
    state.nodes = state.nodes.filter((candidate) => candidate.node_id !== nodeId);
    const operationId = `op-node-remove-${state.operations.length + 1}`;
    asyncOperation(operationId, "node.remove", nodeId);
    envelope(res, { operation_id: operationId, node_id: nodeId }, 202);
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/deployments") {
    envelope(res, { items: state.deployments, next_cursor: null });
    return;
  }
  const deploymentReadMatch = pathname.match(
    /^\/api\/v1\/deployments\/([^/]+)(?:\/(health|bindings))?$/,
  );
  if (req.method === "GET" && deploymentReadMatch) {
    const deploymentId = decodeURIComponent(deploymentReadMatch[1]);
    const resource = deploymentReadMatch[2];
    const deployment = state.deployments.find(
      (candidate) => candidate.deployment_id === deploymentId,
    );
    if (!deployment) {
      problem(res, 404, "DEPLOYMENT_NOT_FOUND", "deployment does not exist");
      return;
    }
    if (resource === "health") {
      envelope(res, {
        deployment_id: deploymentId,
        health: deployment.health,
        observed_state: deployment.observed_state,
        updated_at: deployment.updated_at,
        evidence: {
          source: "docker-health",
          status: deployment.health,
        },
      });
      return;
    }
    if (resource === "bindings") {
      envelope(res, {
        deployment_id: deploymentId,
        service_id: deployment.service_id,
        items:
          deployment.service_id === "e2e-api"
            ? [
                {
                  binding_id: "binding-e2e-gateway",
                  requirement_name: "gateway_control",
                  api_id: "gateway.control",
                  api_version: "1.0.0",
                  consumer_deployment_id: deploymentId,
                  consumer_service_id: "e2e-api",
                  consumer_node_id: deployment.node_id,
                  provider_deployment_id: "dep-gateway",
                  provider_service_id: "gateway",
                  provider_node_id: "desktop-local",
                  provider_endpoint: "127.0.0.1:8080:gateway",
                  provider_path: "/api/control",
                  virtual_endpoint: "/internal/apis/gateway.control",
                  protocol: "http",
                  methods: ["GET", "POST"],
                  auth_mode: "workload",
                  provider_auth_mode: "workload",
                  permission: "gateway.control",
                  topology_id: "primary",
                  topology_revision_id: "rev-1",
                  link_source_endpoint: "127.0.0.1:18081:e2e-api",
                  link_target_endpoint: "127.0.0.1:8080:gateway",
                  credential_generation: 2,
                  context_generation: 3,
                  desired_state: "ACTIVE",
                  observed_state: "ACTIVE",
                  health: "HEALTHY",
                  drift: [],
                  state: "ACTIVE",
                  optional: false,
                  updated_at: deployment.updated_at,
                },
              ]
            : [],
        provider_items: [],
      });
      return;
    }
    envelope(res, {
      deployment: {
        node_id: deployment.node_id,
        instance: {
          deployment_id: deployment.deployment_id,
          service_id: deployment.service_id,
          release_version: deployment.version,
          container_id: deployment.container_id,
          artifact_digest: deployment.artifact_digest,
          runtime_contract: {
            id: "standard-container-v1",
            profile_sha256:
              "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
          },
          runtime_policy_sha256:
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          effective_runtime_sha256:
            "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
          runtime_attested: true,
          desired_state: deployment.desired_state,
          observed_state: deployment.observed_state,
          health: deployment.health,
        },
        management_mode: "MANAGED",
        endpoint: "",
        updated_at: deployment.updated_at,
      },
    });
    return;
  }
  const deploymentActionMatch = pathname.match(
    /^\/api\/v1\/deployments\/([^/]+):(start|stop|restart|uninstall)$/,
  );
  if (req.method === "POST" && deploymentActionMatch) {
    const deploymentId = decodeURIComponent(deploymentActionMatch[1]);
    const action = deploymentActionMatch[2];
    const deployment = state.deployments.find(
      (candidate) => candidate.deployment_id === deploymentId,
    );
    if (!deployment) {
      problem(res, 404, "DEPLOYMENT_NOT_FOUND", "deployment does not exist");
      return;
    }
    if (
      action === "uninstall" &&
      deployment.service_id === "e2e-api" &&
      state.scenario.activeBindingConflict
    ) {
      problem(
        res,
        409,
        "DEPLOYMENT_ACTIVE_BINDINGS",
        "deployment still participates in active API Bindings; remove the Topology Link and apply first",
      );
      return;
    }
    const operationId = `op-deployment-${action}-${state.operations.length + 1}`;
    asyncOperation(operationId, `deployment.${action}`, deploymentId);
    if (action === "uninstall") {
      state.deployments = state.deployments.filter(
        (candidate) => candidate.deployment_id !== deploymentId,
      );
    } else {
      deployment.desired_state = action === "stop" ? "STOPPED" : "RUNNING";
      deployment.observed_state = deployment.desired_state;
      deployment.status = deployment.desired_state;
    }
    envelope(res, { operation_id: operationId, deployment_id: deploymentId }, 202);
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/operations:plan") {
    const operationId = `op-plan-${state.operations.length + 1}`;
    const operation = {
      operation_id: operationId,
      action: body.action,
      target: body.fields?.deployment_id || body.fields?.target_id || "planned-target",
      status: "PLANNED",
      risk: "LOW",
      requires_confirmation: false,
      rollback_available: false,
      log_count: 0,
      summary: "validated immutable plan",
      created_at: "2026-08-03T00:00:12Z",
      updated_at: "2026-08-03T00:00:12Z",
    };
    appendOperation(operation);
    envelope(res, { operation }, 201);
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/operations") {
    envelope(res, { items: state.operations, next_cursor: null });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/store/packages") {
    envelope(res, {
      items: state.packages,
      installed: installedProjection(),
      next_cursor: null,
    });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/store/catalogs") {
    envelope(res, { items: state.catalogs, next_cursor: null });
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/store/catalogs") {
    const source = {
      ...Object.fromEntries(
        Object.entries(body).filter(([property]) => property !== "public_key"),
      ),
      enabled: true,
    };
    state.catalogs = state.catalogs.filter((candidate) => candidate.id !== source.id);
    state.catalogs.push(source);
    envelope(res, { source }, 201);
    return;
  }
  const catalogDeleteMatch = pathname.match(/^\/api\/v1\/store\/catalogs\/([^/]+)$/);
  if (req.method === "DELETE" && catalogDeleteMatch) {
    const sourceId = decodeURIComponent(catalogDeleteMatch[1]);
    if (!state.catalogs.some((candidate) => candidate.id === sourceId)) {
      problem(res, 404, "CATALOG_NOT_FOUND", "catalog source does not exist");
      return;
    }
    state.catalogs = state.catalogs.filter((candidate) => candidate.id !== sourceId);
    envelope(res, { removed: true, source_id: sourceId });
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/store/releases:validate") {
    const providerOnly = body.service_id === "e2e-provider";
    if (
      !providerOnly &&
      (body.topology_id !== "primary" || body.topology_etag !== '"rev-1"')
    ) {
      state.contractViolations.push(
        "release validation omitted the explicit applied Topology revision",
      );
    }
    envelope(res, {
      valid: true,
      catalog_source_id: body.catalog_source_id,
      catalog_id: "catalog-e2e-v2",
      verified_key_ids: ["e2e-ed25519-key"],
      target_platform: { os: "linux", arch: "amd64" },
      plan: { providers: ["docker", "health"] },
      metadata: [],
      requirements: providerOnly ? [] : [
        {
          requirement_name: "gateway_control",
          api_id: "gateway.control",
          version: ">=1.0.0 <2.0.0",
          optional: false,
          selection: "nearest-healthy",
          candidates: [
            {
              deployment_id: "dep-gateway",
              service_id: "gateway",
              node_id: "desktop-local",
              endpoint: "127.0.0.1:8080:gateway",
              path: "/api/control",
              api_id: "gateway.control",
              api_version: "1.0.0",
              protocol: "http",
              methods: ["GET", "POST"],
              auth_mode: "workload",
              permission: "gateway.control",
              healthy: true,
            },
          ],
          recommended_provider_deployment_id: "dep-gateway",
          ambiguous: false,
          missing: false,
        },
      ],
      bindings: providerOnly ? [] : [
        {
          binding_id: "binding-e2e-gateway",
          requirement_name: "gateway_control",
          api_id: "gateway.control",
          api_version: "1.0.0",
          provider_deployment_id: "dep-gateway",
          provider_service_id: "gateway",
          provider_node_id: "desktop-local",
          provider_endpoint: "127.0.0.1:8080:gateway",
          provider_path: "/api/control",
          virtual_endpoint: "/internal/apis/gateway.control",
          protocol: "http",
          methods: ["GET", "POST"],
          auth_mode: "workload",
          provider_auth_mode: "workload",
          permission: "gateway.control",
          credential_generation: 1,
          context_generation: 1,
          desired_state: "ACTIVE",
          observed_state: "RESOLVED",
          health: "HEALTHY",
          drift: [],
          state: "RESOLVED",
          optional: false,
        },
      ],
      runtime: {
        node_id: body.target_node_id,
        contract: {
          id: "standard-container-v1",
          profile_sha256:
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        },
        facts: {
          schema_version: 1,
          observed_at_ms: Date.now(),
          agent_version: "1.0.0-e2e",
          runtime_policy_sha256:
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
          allowed_contracts: [],
          docker: {
            engine: "docker",
            server_version: "28.0.0-e2e",
            operating_system: "E2E Linux",
            os_type: "linux",
            architecture: "amd64",
            cgroup_version: "2",
            memory_limit: true,
            pids_limit: true,
            rootless: false,
            apparmor: true,
            seccomp: true,
            security_options: ["name=seccomp"],
          },
        },
      },
      topology: providerOnly
        ? null
        : {
            topology_id: body.topology_id,
            revision_id: String(body.topology_etag || "").replace(/^"|"$/g, ""),
          },
      topology_diff: providerOnly
        ? null
        : {
            topology_id: body.topology_id,
            from_revision_id: "rev-1",
            to_revision_id: null,
            from_sha256: "sha256-revision-1",
            to_sha256: "sha256-prospective-install",
            changes: [
              {
                kind: "ADD_API_BINDING",
                requirement: "gateway_control",
                provider_deployment_id: "dep-gateway",
              },
            ],
          },
      side_effects: {
        release_imports: 0,
        operations: 0,
        jobs: 0,
        runtime_calls: 0,
      },
    });
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/store/releases:install") {
    const providerOnly = body.service_id === "e2e-provider";
    if (
      !providerOnly &&
      !Array.isArray(body.bindings) ||
      (!providerOnly && body.bindings.length !== 1) ||
      (!providerOnly && body.bindings[0]?.name !== "gateway_control") ||
      (!providerOnly && body.bindings[0]?.provider_deployment_id !== "dep-gateway")
    ) {
      state.contractViolations.push(
        "release install omitted the validated explicit API Binding",
      );
    }
    if (
      !providerOnly &&
      (body.topology_id !== "primary" || body.topology_etag !== '"rev-1"')
    ) {
      state.contractViolations.push(
        "release install omitted the explicit applied Topology revision",
      );
    }
    if (state.scenario.failNextInstall) {
      state.scenario.failNextInstall = false;
      state.compensations.push({
        service_id: body.service_id,
        container_removed: true,
        endpoint_removed: true,
        deployment_promoted: false,
      });
      problem(
        res,
        503,
        "INSTALL_HEALTH_FAILED",
        "health probe failed; container and Endpoint compensation completed; Deployment was not promoted",
      );
      return;
    }
    const operationId = `op-install-${state.operations.length + 1}`;
    const deploymentId = `dep-${body.service_id}`;
    if (!state.deployments.some((item) => item.deployment_id === deploymentId)) {
      state.deployments.push({
        deployment_id: deploymentId,
        node_id: body.target_node_id,
        service_id: body.service_id,
        name: providerOnly ? "E2E Provider" : "E2E API",
        version: body.version || (providerOnly ? "1.0.0" : "1.2.3"),
        kind: "backend-api",
        runtime: "docker",
        status: "RUNNING",
        container_id: providerOnly ? "container-e2e-provider" : "container-e2e-api",
        artifact_digest:
          providerOnly
            ? "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
            : "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        desired_state: "RUNNING",
        observed_state: "RUNNING",
        health: "HEALTHY",
        updated_at: "2026-08-03T00:00:11Z",
      });
    }
    asyncOperation(operationId, "release.install", body.service_id);
    envelope(res, { operation_id: operationId, deployment_id: deploymentId }, 202);
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/store/releases:import") {
    envelope(res, { imported: true, service_id: body.service_id }, 201);
    return;
  }
  if (
    req.method === "POST" &&
    ["/api/v1/store/releases:upgrade", "/api/v1/store/releases:rollback"].includes(
      pathname,
    )
  ) {
    const action = pathname.endsWith(":upgrade") ? "upgrade" : "rollback";
    const deployment = state.deployments.find(
      (candidate) => candidate.deployment_id === body.deployment_id,
    );
    if (!deployment) {
      problem(res, 404, "DEPLOYMENT_NOT_FOUND", "deployment does not exist");
      return;
    }
    if (
      deployment.service_id === "e2e-api" &&
      (!Array.isArray(body.bindings) ||
        body.bindings[0]?.name !== "gateway_control" ||
        body.bindings[0]?.provider_deployment_id !== "dep-gateway")
    ) {
      state.contractViolations.push(`release ${action} dropped active API Bindings`);
    }
    deployment.version = action === "upgrade" ? "1.3.0" : "1.2.3";
    deployment.health = "HEALTHY";
    deployment.observed_state = "RUNNING";
    const operationId = `op-release-${action}-${state.operations.length + 1}`;
    asyncOperation(operationId, `release.${action}`, deployment.deployment_id);
    envelope(res, { operation_id: operationId, deployment_id: deployment.deployment_id }, 202);
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/store/releases:delete") {
    if (
      state.deployments.some(
        (deployment) => deployment.service_id === body.service_id,
      )
    ) {
      problem(res, 409, "RELEASE_IN_USE", "release still has a Deployment");
      return;
    }
    state.packages = state.packages.filter(
      (item) => !(item.module_id === body.service_id && item.version === body.version),
    );
    envelope(res, { deleted: true, service_id: body.service_id, version: body.version });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/topologies/primary") {
    envelope(res, {
      heads: state.heads,
      draft: latestRevision(),
      status: state.topologyStatus,
    });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/topologies/contest-a") {
    envelope(res, {
      heads: state.secondaryHeads,
      draft: state.secondaryRevision,
      status: state.secondaryStatus,
    });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/topologies") {
    envelope(res, {
      items: [state.heads, state.secondaryHeads],
      next_cursor: null,
    });
    return;
  }
  if (
    req.method === "GET" &&
    pathname === "/api/v1/topologies/primary/revisions"
  ) {
    envelope(res, {
      items: [...state.revisions].reverse(),
      next_cursor: null,
    });
    return;
  }
  if (
    req.method === "GET" &&
    pathname === "/api/v1/topologies/contest-a/revisions"
  ) {
    envelope(res, { items: [state.secondaryRevision], next_cursor: null });
    return;
  }
  if (
    req.method === "POST" &&
    pathname === "/api/v1/topologies/primary/revisions"
  ) {
    const expected = `"${state.heads.draft_revision_id}"`;
    if (req.headers["if-match"] !== expected) {
      state.contractViolations.push("topology revision omitted current If-Match");
      problem(res, 412, "REVISION_CONFLICT", `expected ${expected}`);
      return;
    }
    const number = Math.max(...state.revisions.map((item) => item.revision_number)) + 1;
    const revision = makeRevision(number, body, {
      parent_revision_id: state.heads.draft_revision_id,
      message: req.headers["x-change-message"] || "browser draft",
    });
    state.revisions.push(revision);
    state.heads.draft_revision_id = revision.revision_id;
    state.topologyStatus.desired_revision_id = revision.revision_id;
    state.topologyStatus.state = "DRIFTED";
    state.topologyStatus.drift = [
      {
        resource_kind: "TopologyRevision",
        resource_id: "primary",
        kind: "REVISION_MISMATCH",
        detail: `${revision.revision_id} has not been applied`,
      },
    ];
    envelope(res, { revision }, 201);
    return;
  }
  if (
    req.method === "POST" &&
    pathname === "/api/v1/topologies/primary:validate"
  ) {
    envelope(res, {
      valid: true,
      content_sha256: `sha256-validated-${state.heads.draft_revision_id}`,
    });
    return;
  }
  if (
    req.method === "POST" &&
    pathname === "/api/v1/topologies/primary:diff"
  ) {
    envelope(res, {
      diff: {
        topology_id: "primary",
        from_revision_id: body.from_revision_id || null,
        to_revision_id: body.to_revision_id || state.heads.draft_revision_id,
        from_sha256: "sha256-applied",
        to_sha256: latestRevision().content_sha256,
        changes: [
          {
            kind: "ENDPOINT_ADDED",
            resource_id: latestRevision().spec.endpoints.at(-1).endpoint,
          },
        ],
      },
    });
    return;
  }
  if (
    req.method === "POST" &&
    pathname === "/api/v1/topologies/primary:apply"
  ) {
    const expected = `"${state.heads.draft_revision_id}"`;
    if (req.headers["if-match"] !== expected) {
      state.contractViolations.push("topology apply omitted current If-Match");
      problem(res, 412, "REVISION_CONFLICT", `expected ${expected}`);
      return;
    }
    const operationId = `op-topology-apply-${state.operations.length + 1}`;
    state.heads.applied_revision_id = state.heads.draft_revision_id;
    state.heads.last_operation_id = operationId;
    state.topologyStatus.desired_revision_id = state.heads.draft_revision_id;
    state.topologyStatus.observed_revision_id = state.heads.draft_revision_id;
    state.topologyStatus.state = "IN_SYNC";
    state.topologyStatus.drift = [];
    state.topologyStatus.last_operation_id = operationId;
    asyncOperation(operationId, "topology.apply", "primary");
    envelope(
      res,
      {
        operation_id: operationId,
        revision_id: state.heads.draft_revision_id,
        topology_id: "primary",
      },
      202,
    );
    return;
  }
  if (
    req.method === "POST" &&
    pathname === "/api/v1/topologies/primary:rollback"
  ) {
    const expected = `"${state.heads.draft_revision_id}"`;
    if (req.headers["if-match"] !== expected) {
      state.contractViolations.push("topology rollback omitted current If-Match");
      problem(res, 412, "REVISION_CONFLICT", `expected ${expected}`);
      return;
    }
    const target = state.revisions.find(
      (item) => item.revision_id === body.revision_id,
    );
    if (!target) {
      problem(res, 404, "REVISION_NOT_FOUND", "rollback target does not exist");
      return;
    }
    const number = Math.max(...state.revisions.map((item) => item.revision_number)) + 1;
    const revision = makeRevision(number, target.spec, {
      parent_revision_id: state.heads.draft_revision_id,
      rollback_of_revision_id: target.revision_id,
      message: `rollback of ${target.revision_id}`,
    });
    state.revisions.push(revision);
    const operationId = `op-topology-rollback-${state.operations.length + 1}`;
    state.heads.draft_revision_id = revision.revision_id;
    state.heads.applied_revision_id = revision.revision_id;
    state.heads.last_operation_id = operationId;
    state.topologyStatus.desired_revision_id = revision.revision_id;
    state.topologyStatus.observed_revision_id = revision.revision_id;
    state.topologyStatus.state = "IN_SYNC";
    state.topologyStatus.drift = [];
    state.topologyStatus.last_operation_id = operationId;
    asyncOperation(operationId, "topology.rollback", "primary");
    envelope(
      res,
      {
        operation_id: operationId,
        revision_id: revision.revision_id,
        topology_id: "primary",
      },
      202,
    );
    return;
  }
  if (
    req.method === "GET" &&
    pathname === "/api/v1/topologies/primary/status"
  ) {
    envelope(res, { status: state.topologyStatus });
    return;
  }
  if (req.method === "GET" && pathname === "/api/v1/diagnostics") {
    envelope(res, { items: state.diagnostics, next_cursor: null });
    return;
  }
  if (req.method === "POST" && pathname === "/api/v1/diagnostics") {
    const report = {
      report_id: `diag-${state.diagnostics.length + 1}`,
      operation_id: state.operations[0]?.operation_id || "",
      status: "READY",
      summary: "current topology and operation snapshot",
      created_at: "2026-08-03T00:00:40Z",
      topology_id: "primary",
    };
    state.diagnostics.unshift(report);
    envelope(res, { diagnostic_report: report }, 201);
    return;
  }
  const diagnosticExportMatch = pathname.match(
    /^\/api\/v1\/diagnostics\/([^/.]+)\.(json|md)$/,
  );
  if (req.method === "GET" && diagnosticExportMatch) {
    const reportId = decodeURIComponent(diagnosticExportMatch[1]);
    const format = diagnosticExportMatch[2];
    const report = state.diagnostics.find((candidate) => candidate.report_id === reportId);
    if (!report) {
      problem(res, 404, "DIAGNOSTIC_NOT_FOUND", "diagnostic does not exist");
      return;
    }
    envelope(res, {
      report_id: reportId,
      format,
      content: format === "md" ? `# Diagnostic ${reportId}\n` : report,
    });
    return;
  }
  const diagnosticMatch = pathname.match(/^\/api\/v1\/diagnostics\/([^/]+)$/);
  if (req.method === "GET" && diagnosticMatch) {
    const reportId = decodeURIComponent(diagnosticMatch[1]);
    const report = state.diagnostics.find((candidate) => candidate.report_id === reportId);
    if (!report) {
      problem(res, 404, "DIAGNOSTIC_NOT_FOUND", "diagnostic does not exist");
      return;
    }
    envelope(res, { diagnostic_report: report });
    return;
  }
  const operationMatch = pathname.match(
    /^\/api\/v1\/operations\/([^/]+)(?:\/(logs|events)|:(confirm|apply|cancel|retry|rollback))$/,
  );
  if (operationMatch) {
    const operationId = decodeURIComponent(operationMatch[1]);
    const collection = operationMatch[2];
    const action = operationMatch[3];
    const operation = state.operations.find(
      (candidate) => candidate.operation_id === operationId,
    );
    if (!operation) {
      problem(res, 404, "OPERATION_NOT_FOUND", "operation does not exist");
      return;
    }
    if (req.method === "GET" && collection === "logs") {
      envelope(res, { items: state.logs[operationId] || [], next_cursor: null });
      return;
    }
    if (req.method === "GET" && collection === "events") {
      const emitEvent = () => {
        if (res.destroyed) return;
        state.eventSequence += 1;
        const log =
          state.logs[operationId]?.at(-1) ||
          {
            operation_id: operationId,
            step_id: "runtime",
            level: "info",
            message: `${operation.action} is ${operation.status}`,
            created_at: "2026-08-03T00:00:12Z",
          };
        const event = {
          ...log,
          job_id: `${operationId}-job`,
          sequence: state.eventSequence,
        };
        const text = `retry: 750\nid: ${state.eventSequence}\nevent: job\ndata: ${JSON.stringify({ event })}\n\n`;
        res.writeHead(200, {
          "content-type": "text/event-stream; charset=utf-8",
          "cache-control": "no-store",
          "content-length": Buffer.byteLength(text),
          "x-request-id": `e2e-sse-${++requestSequence}`,
        });
        res.end(text);
      };
      if (state.scenario.sseDelayMs > 0) {
        setTimeout(emitEvent, state.scenario.sseDelayMs);
      } else {
        emitEvent();
      }
      return;
    }
    if (req.method === "POST" && action) {
      if (action === "retry") {
        operation.status = "SUCCEEDED";
        operation.error = "";
        operation.summary = "retry completed without duplicate side effects";
        operation.updated_at = "2026-08-03T00:00:20Z";
        state.logs[operationId].push({
          operation_id: operationId,
          step_id: "retry",
          level: "info",
          message: "retry reused the idempotent attempt and succeeded",
          created_at: "2026-08-03T00:00:20Z",
        });
      }
      if (action === "cancel") {
        operation.status = "CANCELLED";
        operation.summary = "operation cancelled and lease released";
        operation.updated_at = "2026-08-03T00:00:21Z";
      }
      envelope(res, { operation_id: operationId, operation }, 202);
      return;
    }
  }
  if (req.method === "GET" && pathname === "/api/v1/ui/layout") {
    const topologyId = url.searchParams.get("topology_id");
    if (!topologyId) {
      state.contractViolations.push("layout read omitted topology_id");
      problem(res, 422, "LAYOUT_TOPOLOGY_REQUIRED", "topology_id is required");
      return;
    }
    envelope(res, { layout: state.layouts[`e2e-admin:${topologyId}`] || {} });
    return;
  }
  if (req.method === "PUT" && pathname === "/api/v1/ui/layout") {
    const topologyId = url.searchParams.get("topology_id");
    metrics.layoutPutPaths.push(`${pathname}?topology_id=${encodeURIComponent(topologyId || "")}`);
    if (!topologyId) {
      state.contractViolations.push("layout write omitted topology_id");
      problem(res, 422, "LAYOUT_TOPOLOGY_REQUIRED", "topology_id is required");
      return;
    }
    if (state.scenario.failLayoutSave) {
      problem(res, 507, "LAYOUT_PERSISTENCE_FAILED", "layout database is full");
      return;
    }
    state.layouts[`e2e-admin:${topologyId}`] = structuredClone(body);
    envelope(res, { layout: state.layouts[`e2e-admin:${topologyId}`] });
    return;
  }

  problem(res, 404, "MOCK_ROUTE_NOT_FOUND", `${req.method} ${pathname}`);
}

function serveStatic(req, res, url) {
  const requested = url.pathname === "/" ? "/index.html" : url.pathname;
  let file = resolve(dist, `.${requested}`);
  if (!file.startsWith(`${dist}${sep}`)) {
    res.writeHead(404).end();
    return;
  }
  try {
    if (!statSync(file).isFile()) throw new Error("not a file");
  } catch {
    file = resolve(dist, "index.html");
  }
  const content = readFileSync(file);
  res.writeHead(200, {
    "content-type": mimeTypes[extname(file)] || "application/octet-stream",
    "content-length": content.length,
    "cache-control": "no-store",
  });
  if (req.method === "HEAD") res.end();
  else res.end(content);
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url || "/", `http://${req.headers.host || "localhost"}`);
  trackRequest(req, res, url.pathname);
  try {
    if (req.method === "POST" && url.pathname === "/__e2e/reset") {
      state = initialState();
      metrics = initialMetrics();
      envelope(res, { reset: true });
      return;
    }
    if (req.method === "POST" && url.pathname === "/__e2e/scenario") {
      const body = await readJson(req);
      Object.assign(state.scenario, body);
      envelope(res, { scenario: state.scenario });
      return;
    }
    if (req.method === "GET" && url.pathname === "/__e2e/state") {
      envelope(res, { state, metrics });
      return;
    }
    if (req.method === "GET" && url.pathname === "/__e2e/metrics") {
      envelope(res, metrics);
      return;
    }
    if (url.pathname.startsWith("/api/v1/")) {
      await handleApi(req, res, url);
      return;
    }
    serveStatic(req, res, url);
  } catch (error) {
    if (!res.headersSent) {
      problem(
        res,
        500,
        "MOCK_INTERNAL_ERROR",
        error instanceof Error ? error.message : String(error),
      );
    } else {
      res.destroy(error instanceof Error ? error : undefined);
    }
  }
});

server.listen(port, "127.0.0.1", () => {
  process.stdout.write(`OJOS Web E2E mock listening on http://127.0.0.1:${port}\n`);
});

function shutdown() {
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(1), 2_000).unref();
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
