import type {
  AsyncOperationResult,
  DeploymentRow,
  CapabilityRow,
  HealthInfo,
  LayoutState,
  NodeRow,
  OperationLog,
  OperationRow,
  StoreModule,
  StoreIndexResponse,
  StoreValidationResult,
  TopologyDetail,
  TopologyDiff,
  TopologyHeads,
  TopologyRevision,
  TopologySpec,
  TopologyStatus,
} from "./types";
import { markAuthRequired } from "./auth";

declare global {
  interface Window {
    __OJOS_AUTH_READY__?: Promise<void>;
    __OJOS_CSRF_TOKEN__?: string;
  }
}

export interface ApiCallOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
  idempotencyKey?: string;
  ifMatch?: string;
  changeMessage?: string;
}

export const DEFAULT_READ_TIMEOUT_MS = 12_000;
export const DEFAULT_MUTATION_TIMEOUT_MS = 45_000;
export const MAX_OPERATION_LOGS = 500;

let idempotencySequence = 0;

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status = 0,
    readonly code = "REQUEST_FAILED",
    readonly requestId = "",
    readonly details?: unknown,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export class RequestTimeoutError extends ApiError {
  constructor(path: string, timeoutMs: number) {
    super(
      `请求 ${path} 超过 ${Math.ceil(timeoutMs / 1000)} 秒未响应`,
      0,
      "REQUEST_TIMEOUT",
    );
    this.name = "RequestTimeoutError";
  }
}

export class RequestCancelledError extends ApiError {
  constructor(path: string) {
    super(`请求 ${path} 已取消`, 0, "REQUEST_CANCELLED");
    this.name = "RequestCancelledError";
  }
}

export function isRequestCancelled(err: unknown): boolean {
  return err instanceof RequestCancelledError;
}

/**
 * HttpOnly 会话缺失或过期时 daemon 返回的 401。单独成类，方便调用方（尤其是轮询）
 * 区分“需要重新登录”和“连不上/业务失败”，避免重复弹 toast。
 */
export class AuthRequiredError extends Error {
  readonly status = 401;

  constructor(message = "编排器身份会话缺失或已过期") {
    super(message);
    this.name = "AuthRequiredError";
  }
}

export function isAuthRequiredError(err: unknown): err is AuthRequiredError {
  return err instanceof AuthRequiredError;
}

function idempotencyKey(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  idempotencySequence += 1;
  return `web-${Date.now().toString(36)}-${idempotencySequence.toString(36)}`;
}

function isMutation(method: string): boolean {
  return !["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase());
}

function waitForPromise<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) return Promise.reject(signal.reason);
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(signal.reason);
    signal.addEventListener("abort", abort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener("abort", abort);
        resolve(value);
      },
      (error) => {
        signal.removeEventListener("abort", abort);
        reject(error);
      },
    );
  });
}

function responseMessage(data: any, status: number): string {
  for (const value of [data?.detail, data?.message, data?.title, data?.error]) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return `HTTP ${status}`;
}

function arrayOrEmpty<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

function textOr(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function numberOr(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function booleanOr(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function stringsOrEmpty(value: unknown): string[] {
  return arrayOrEmpty<unknown>(value).filter(
    (item): item is string => typeof item === "string",
  );
}

function normalizeDeployment(value: unknown): DeploymentRow {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  const instance =
    row.instance && typeof row.instance === "object" && !Array.isArray(row.instance)
      ? (row.instance as Record<string, unknown>)
      : row;
  const observedState = textOr(
    instance.observed_state,
    textOr(row.status, "UNKNOWN"),
  );
  const health = textOr(
    instance.health,
    textOr(row.endpoint_health, "UNKNOWN"),
  );
  return {
    deployment_id: textOr(instance.deployment_id, textOr(row.deployment_id)),
    node_id: textOr(row.node_id),
    service_id: textOr(instance.service_id, textOr(row.service_id)),
    name: textOr(row.name),
    version: textOr(row.version),
    kind: textOr(row.kind, "container"),
    runtime: textOr(row.runtime, "docker"),
    host_ip: textOr(row.host_ip, textOr(row.node_id)),
    status: observedState,
    endpoint: textOr(row.endpoint),
    protocol: textOr(row.protocol),
    health_path: textOr(row.health_path),
    endpoint_health: health,
    reachable: booleanOr(row.reachable, health.toUpperCase() === "HEALTHY"),
    endpoint_count: numberOr(row.endpoint_count),
    endpoints: stringsOrEmpty(row.endpoints),
    container_id: textOr(instance.container_id),
    artifact_digest: textOr(instance.artifact_digest),
    desired_state: textOr(instance.desired_state, "UNKNOWN"),
    observed_state: observedState,
    updated_at: textOr(row.updated_at),
  };
}

function normalizeNode(value: unknown): NodeRow {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  const labels =
    row.labels && typeof row.labels === "object" && !Array.isArray(row.labels)
      ? (row.labels as Record<string, unknown>)
      : {};
  return {
    node_id: textOr(row.node_id),
    host_ip: textOr(row.host_ip),
    parent_node_id: textOr(row.parent_node_id),
    role: textOr(row.role, "worker"),
    labels,
    status: textOr(row.status, "UNKNOWN"),
    created_at: textOr(row.created_at),
    updated_at: textOr(row.updated_at),
  };
}

function normalizeOperation(value: unknown): OperationRow {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  const target = textOr(row.target, textOr(row.target_id));
  const result =
    typeof row.result === "string"
      ? row.result
      : row.result === undefined
        ? ""
        : JSON.stringify(row.result);
  const createdAtMs = numberOr(row.created_at_ms);
  const updatedAtMs = numberOr(row.updated_at_ms);
  const status = textOr(row.status, "UNKNOWN");
  return {
    operation_id: textOr(row.operation_id),
    action: textOr(row.action, "unknown"),
    target,
    status,
    risk: textOr(row.risk, "UNKNOWN"),
    plan_required: textOr(row.plan_required),
    mode: textOr(row.mode),
    requires_confirmation:
      booleanOr(row.requires_confirmation) || status === "PLANNED",
    driver_authorized: booleanOr(row.driver_authorized),
    rollback_available:
      booleanOr(row.rollback_available) ||
      (status === "SUCCEEDED" && arrayOrEmpty(row.planned_jobs).length > 0),
    fields: textOr(row.fields),
    preview_target: textOr(row.preview_target),
    preview_steps: textOr(row.preview_steps),
    preview_confirmation: textOr(row.preview_confirmation),
    result,
    error: textOr(row.error, textOr(row.error_message)),
    log_count: numberOr(row.log_count),
    summary: textOr(row.summary, `${textOr(row.action, "operation")} ${target}`.trim()),
    created_at: textOr(
      row.created_at,
      createdAtMs > 0 ? new Date(createdAtMs).toISOString() : "",
    ),
    updated_at: textOr(
      row.updated_at,
      updatedAtMs > 0 ? new Date(updatedAtMs).toISOString() : "",
    ),
  };
}

export function normalizeOperationLog(value: unknown): OperationLog {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  const createdAtMs = numberOr(row.created_at_ms);
  return {
    ...row,
    operation_id: textOr(row.operation_id),
    step_id: textOr(row.step_id, textOr(row.job_id, textOr(row.event_type, "runtime"))),
    level: textOr(row.level, "info").toLowerCase(),
    message: textOr(row.message),
    created_at: textOr(
      row.created_at,
      createdAtMs > 0 ? new Date(createdAtMs).toISOString() : "",
    ),
  };
}

function normalizeHealth(value: unknown): HealthInfo {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  return {
    status: textOr(row.status, "unknown"),
    service: textOr(row.service, "orchestrator"),
    store: textOr(row.store, "unknown"),
    warnings: stringsOrEmpty(row.warnings),
  };
}

function normalizeStoreIndex(value: unknown): StoreIndexResponse {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  const modules = arrayOrEmpty<unknown>(row.items).map((value) => {
    const module =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)
        : {};
    return {
      id: textOr(module.module_id, textOr(module.id)),
      name: textOr(module.name, textOr(module.module_id, textOr(module.id))),
      description: textOr(module.description),
      kind: textOr(module.kind, "unknown"),
      tags: stringsOrEmpty(module.tags),
      repo: "",
      source_url: "",
      checksum: textOr(module.metadata_sha256, textOr(module.checksum)),
      version: textOr(module.version),
      channel: textOr(module.channel, "stable"),
      platforms: arrayOrEmpty<unknown>(module.platforms)
        .map((platform) => {
          const item =
            platform && typeof platform === "object" && !Array.isArray(platform)
              ? (platform as Record<string, unknown>)
              : {};
          return { os: textOr(item.os), arch: textOr(item.arch) };
        })
        .filter((platform) => platform.os && platform.arch),
      min_orchestrator_version: textOr(module.min_orchestrator_version),
      oci_image: textOr(module.oci_image),
      source_id: textOr(module.source_id),
      catalog_id: textOr(module.catalog_id),
    } satisfies StoreModule;
  });
  const installed: StoreIndexResponse["installed"] = {};
  const rawInstalled =
    row.installed && typeof row.installed === "object"
      ? (row.installed as Record<string, unknown>)
      : {};
  for (const [id, value] of Object.entries(rawInstalled)) {
    const item =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)
        : {};
    installed[id] = {
      version: textOr(item.version),
      versions: stringsOrEmpty(item.versions),
      kind: textOr(item.kind, "unknown"),
      deployments: arrayOrEmpty<unknown>(item.deployments).map((value) => {
        const deployment =
          value && typeof value === "object"
            ? (value as Record<string, unknown>)
            : {};
        return {
          deployment_id: textOr(deployment.deployment_id),
          node_id: textOr(deployment.node_id),
          version: textOr(deployment.version),
          host_ip: textOr(deployment.host_ip),
          status: textOr(deployment.status, "unknown"),
        };
      }),
    };
  }
  return {
    index_url: textOr(row.index_url, "trusted-catalog-v2"),
    cached: booleanOr(row.cached),
    index: {
      schema_version: 2,
      name: "Trusted Catalog v2",
      description: "Signed and digest-pinned release catalog",
      updated_at: "",
      modules,
    },
    installed,
  };
}

export async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  options: ApiCallOptions = {},
): Promise<T> {
  const timeoutMs =
    options.timeoutMs ??
    (isMutation(method) ? DEFAULT_MUTATION_TIMEOUT_MS : DEFAULT_READ_TIMEOUT_MS);
  const controller = new AbortController();
  let timedOut = false;
  const timeout = window.setTimeout(() => {
    timedOut = true;
    controller.abort("timeout");
  }, Math.max(1, timeoutMs));
  const abortFromCaller = () => controller.abort(options.signal?.reason);
  if (options.signal?.aborted) abortFromCaller();
  else options.signal?.addEventListener("abort", abortFromCaller, { once: true });

  const init: RequestInit = {
    method,
    credentials: "same-origin",
    signal: controller.signal,
  };
  const headers: Record<string, string> = {};
  try {
    if (window.__OJOS_AUTH_READY__) {
      await waitForPromise(window.__OJOS_AUTH_READY__, controller.signal);
    }
    // Desktop/OIDC 的会话凭据只存在于 HttpOnly cookie；脚本仅发送内存 CSRF。
    if (isMutation(method)) {
      headers["Idempotency-Key"] = options.idempotencyKey || idempotencyKey();
      if (window.__OJOS_CSRF_TOKEN__) {
        headers["x-csrf-token"] = window.__OJOS_CSRF_TOKEN__;
      }
    }
    if (options.ifMatch) {
      const revision = options.ifMatch.replace(/^"|"$/g, "");
      headers["If-Match"] = `"${revision}"`;
    }
    if (options.changeMessage?.trim()) {
      headers["X-Change-Message"] = options.changeMessage.trim();
    }
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    if (Object.keys(headers).length) init.headers = headers;

    const response = await fetch(path, init);
    const text = await response.text();
    let data: any = {};
    let parsed = true;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        parsed = false;
      }
    }
    const requestId =
      response.headers.get("x-request-id") || data?.meta?.request_id || data?.request_id || "";
    if (response.status === 401) {
      markAuthRequired();
      throw new AuthRequiredError(
        responseMessage(data, response.status) !== `HTTP ${response.status}`
          ? `控制面未授权：${responseMessage(data, response.status)}`
          : undefined,
      );
    }
    if (!parsed) {
      throw new ApiError(
        `响应不是 JSON（HTTP ${response.status}）`,
        response.status,
        "INVALID_RESPONSE",
        requestId,
      );
    }
    if (!response.ok || data?.status === "error") {
      throw new ApiError(
        responseMessage(data, response.status),
        response.status,
        typeof data?.code === "string" ? data.code : "REQUEST_FAILED",
        requestId,
        data,
      );
    }
    return data as T;
  } catch (err) {
    if (err instanceof ApiError || err instanceof AuthRequiredError) throw err;
    if (controller.signal.aborted) {
      if (timedOut) throw new RequestTimeoutError(path, timeoutMs);
      throw new RequestCancelledError(path);
    }
    throw new ApiError(
      `无法连接编排器 daemon：${err instanceof Error ? err.message : String(err)}`,
      0,
      "NETWORK_ERROR",
    );
  } finally {
    window.clearTimeout(timeout);
    options.signal?.removeEventListener("abort", abortFromCaller);
  }
}

interface V1Envelope<T> {
  data: T;
  meta: {
    request_id: string;
    api_version: string;
  };
}

/**
 * Every JSON v1 success is an envelope. Rejecting a legacy-shaped 2xx here is
 * intentional: otherwise the UI can silently render an empty projection while
 * believing a mutation or read succeeded.
 */
export async function v1Request<T>(
  method: string,
  path: string,
  body?: unknown,
  options: ApiCallOptions = {},
): Promise<T> {
  if (!path.startsWith("/api/v1")) {
    throw new ApiError(
      `v1 request must use /api/v1: ${path}`,
      0,
      "INVALID_V1_PATH",
    );
  }
  const envelope = await request<V1Envelope<T>>(method, path, body, options);
  if (
    !envelope ||
    typeof envelope !== "object" ||
    !("data" in envelope) ||
    !envelope.meta ||
    typeof envelope.meta.request_id !== "string" ||
    !envelope.meta.request_id
  ) {
    throw new ApiError(
      `响应不符合 /api/v1 envelope：${path}`,
      0,
      "INVALID_V1_ENVELOPE",
      "",
      envelope,
    );
  }
  return envelope.data;
}

function withCursor(path: string, cursor: string): string {
  const separator = path.includes("?") ? "&" : "?";
  const suffix = new URLSearchParams({ limit: "200" });
  if (cursor) suffix.set("cursor", cursor);
  return `${path}${separator}${suffix.toString()}`;
}

async function collectCursorItems<T>(
  path: string,
  extract: (data: Record<string, unknown>) => unknown,
  options: ApiCallOptions = {},
): Promise<{ items: T[]; pages: Record<string, unknown>[] }> {
  const items: T[] = [];
  const pages: Record<string, unknown>[] = [];
  const seen = new Set<string>();
  let cursor = "";
  for (let page = 0; page < 100; page += 1) {
    const data = await v1Request<Record<string, unknown>>(
      "GET",
      withCursor(path, cursor),
      undefined,
      options,
    );
    pages.push(data);
    items.push(...arrayOrEmpty<T>(extract(data)));
    const next = textOr(data.next_cursor);
    if (!next) return { items, pages };
    if (seen.has(next)) {
      throw new ApiError(
        `集合游标重复：${path}`,
        0,
        "INVALID_CURSOR",
        "",
        { cursor: next },
      );
    }
    seen.add(next);
    cursor = next;
  }
  throw new ApiError(
    `集合分页超过 100 页：${path}`,
    0,
    "PAGINATION_LIMIT_EXCEEDED",
  );
}

export interface OperationStreamEvent {
  id: string;
  event: "job" | "operation" | string;
  data: Record<string, unknown>;
}

export interface OperationEventBatch {
  events: OperationStreamEvent[];
  lastEventId: string;
  retryMs: number;
}

export async function operationEventBatch(
  operationId: string,
  lastEventId = "",
  options: ApiCallOptions = {},
): Promise<OperationEventBatch> {
  const path = `/api/v1/operations/${encodeURIComponent(operationId)}/events`;
  const timeoutMs = options.timeoutMs ?? DEFAULT_READ_TIMEOUT_MS;
  const controller = new AbortController();
  let timedOut = false;
  const timeout = window.setTimeout(() => {
    timedOut = true;
    controller.abort("timeout");
  }, Math.max(1, timeoutMs));
  const abortFromCaller = () => controller.abort(options.signal?.reason);
  if (options.signal?.aborted) abortFromCaller();
  else options.signal?.addEventListener("abort", abortFromCaller, { once: true });
  try {
    if (window.__OJOS_AUTH_READY__) {
      await waitForPromise(window.__OJOS_AUTH_READY__, controller.signal);
    }
    const headers: Record<string, string> = { Accept: "text/event-stream" };
    if (lastEventId) headers["Last-Event-ID"] = lastEventId;
    const response = await fetch(path, {
      method: "GET",
      credentials: "same-origin",
      signal: controller.signal,
      headers,
    });
    const text = await response.text();
    const requestId = response.headers.get("x-request-id") || "";
    if (response.status === 401) {
      markAuthRequired();
      throw new AuthRequiredError();
    }
    if (!response.ok) {
      let details: unknown = text;
      try {
        details = text ? JSON.parse(text) : {};
      } catch {
        // Preserve the response text for diagnostics.
      }
      throw new ApiError(
        responseMessage(details, response.status),
        response.status,
        typeof (details as any)?.code === "string"
          ? (details as any).code
          : "OPERATION_EVENTS_FAILED",
        requestId,
        details,
      );
    }
    if (!response.headers.get("content-type")?.startsWith("text/event-stream")) {
      throw new ApiError(
        "Operation events response is not text/event-stream",
        response.status,
        "INVALID_EVENT_STREAM",
        requestId,
      );
    }
    if (text.length > 2 * 1024 * 1024) {
      throw new ApiError(
        "Operation event batch exceeded 2 MiB",
        response.status,
        "EVENT_STREAM_TOO_LARGE",
        requestId,
      );
    }
    return parseOperationEventStream(text, lastEventId);
  } catch (err) {
    if (err instanceof ApiError || err instanceof AuthRequiredError) throw err;
    if (controller.signal.aborted) {
      if (timedOut) throw new RequestTimeoutError(path, timeoutMs);
      throw new RequestCancelledError(path);
    }
    throw new ApiError(
      `Unable to read Operation events: ${err instanceof Error ? err.message : String(err)}`,
      0,
      "NETWORK_ERROR",
    );
  } finally {
    window.clearTimeout(timeout);
    options.signal?.removeEventListener("abort", abortFromCaller);
  }
}

export function parseOperationEventStream(
  text: string,
  initialLastEventId = "",
): OperationEventBatch {
  const events: OperationStreamEvent[] = [];
  let lastEventId = initialLastEventId;
  let retryMs = 1000;
  for (const block of text.split(/\r?\n\r?\n/)) {
    if (!block.trim()) continue;
    let id = "";
    let event = "message";
    const data: string[] = [];
    for (const line of block.split(/\r?\n/)) {
      if (line.startsWith(":")) continue;
      const [field, ...rest] = line.split(":");
      const value = rest.join(":").replace(/^ /, "");
      if (field === "id") id = value;
      if (field === "event") event = value;
      if (field === "data") data.push(value);
      if (field === "retry") {
        const parsed = Number(value);
        if (Number.isFinite(parsed) && parsed >= 250 && parsed <= 30_000) {
          retryMs = parsed;
        }
      }
    }
    if (id) lastEventId = id;
    if (!data.length) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(data.join("\n"));
    } catch {
      throw new ApiError(
        "Operation event contains invalid JSON",
        200,
        "INVALID_EVENT_STREAM",
      );
    }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      throw new ApiError(
        "Operation event data must be an object",
        200,
        "INVALID_EVENT_STREAM",
      );
    }
    events.push({ id, event, data: parsed as Record<string, unknown> });
  }
  return { events, lastEventId, retryMs };
}

export const api = {
  health: (options?: ApiCallOptions) =>
    v1Request<HealthInfo>(
      "GET",
      "/api/v1/healthz/ready",
      undefined,
      options,
    ).then(normalizeHealth),
  capabilities: (options?: ApiCallOptions) =>
    v1Request<{ actions?: CapabilityRow[] }>(
      "GET",
      "/api/v1/capabilities",
      undefined,
      options,
    ).then((data) =>
      arrayOrEmpty<CapabilityRow>(data.actions).filter(
        (capability) =>
          typeof capability?.action === "string" &&
          capability.capability_status?.toUpperCase() !== "UNSUPPORTED",
      ),
    ),

  catalogs: (options?: ApiCallOptions) =>
    collectCursorItems<Record<string, unknown>>(
      "/api/v1/store/catalogs",
      (data) => data.items,
      options,
    ).then(({ items }) => items),
  registerCatalog: (
    source: {
      id: string;
      url: string;
      required_key_id: string;
      auth_secret_ref?: string;
      public_key?: string;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/store/catalogs",
      source,
      options,
    ),
  removeCatalog: (sourceId: string, options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "DELETE",
      `/api/v1/store/catalogs/${encodeURIComponent(sourceId)}`,
      {},
      options,
    ),

  nodes: (options?: ApiCallOptions) =>
    collectCursorItems<unknown>(
      "/api/v1/nodes",
      (data) => data.items ?? data.nodes,
      options,
    ).then(({ items }) => items.map(normalizeNode)),
  node: (nodeId: string, options?: ApiCallOptions) =>
    v1Request<{ node?: unknown }>(
      "GET",
      `/api/v1/nodes/${encodeURIComponent(nodeId)}`,
      undefined,
      options,
    ).then((data) => normalizeNode(data.node)),
  nodeHealth: (nodeId: string, options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "GET",
      `/api/v1/nodes/${encodeURIComponent(nodeId)}/health`,
      undefined,
      options,
    ),
  createNodeEnrollment: (
    requestBody: {
      node_id: string;
      host_ip: string;
      role?: string;
      parent_node_id?: string;
      labels?: Record<string, unknown>;
      ttl_seconds?: number;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<{
      code_id: string;
      node_id: string;
      enrollment_code: string;
      expires_at_ms: number;
    }>("POST", "/api/v1/nodes/enrollment-codes", requestBody, options),
  revokeNodeCertificates: (
    nodeId: string,
    reason: string,
    options?: ApiCallOptions,
  ) =>
    v1Request<{
      node_id: string;
      certificate_status: string;
      revoked_certificates: number;
    }>(
      "POST",
      `/api/v1/nodes/${encodeURIComponent(nodeId)}:revoke-certificates`,
      { reason },
      options,
    ),
  nodeDrain: (nodeId: string, options?: ApiCallOptions) =>
    v1Request<AsyncOperationResult>(
      "POST",
      `/api/v1/nodes/${encodeURIComponent(nodeId)}:drain`,
      {},
      options,
    ),
  nodeRemove: (nodeId: string, options?: ApiCallOptions) =>
    v1Request<AsyncOperationResult>(
      "DELETE",
      `/api/v1/nodes/${encodeURIComponent(nodeId)}`,
      {},
      options,
    ),

  deployments: (options?: ApiCallOptions) =>
    collectCursorItems<unknown>(
      "/api/v1/deployments",
      (data) => data.items,
      options,
    ).then(({ items }) => items.map(normalizeDeployment)),
  deployment: (deploymentId: string, options?: ApiCallOptions) =>
    v1Request<{ deployment?: unknown }>(
      "GET",
      `/api/v1/deployments/${encodeURIComponent(deploymentId)}`,
      undefined,
      options,
    ).then((data) => normalizeDeployment(data.deployment)),
  deploymentHealth: (deploymentId: string, options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "GET",
      `/api/v1/deployments/${encodeURIComponent(deploymentId)}/health`,
      undefined,
      options,
    ),
  deploymentAction: (
    deploymentId: string,
    action: "start" | "stop" | "restart" | "uninstall",
    options?: ApiCallOptions,
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      `/api/v1/deployments/${encodeURIComponent(deploymentId)}:${action}`,
      {},
      options,
    ),

  operations: (options?: ApiCallOptions) =>
    collectCursorItems<unknown>(
      "/api/v1/operations",
      (data) => data.items,
      options,
    ).then(({ items }): OperationRow[] => items.map(normalizeOperation)),
  operation: (operationId: string, options?: ApiCallOptions) =>
    v1Request<{ operation: unknown }>(
      "GET",
      `/api/v1/operations/${encodeURIComponent(operationId)}`,
      undefined,
      options,
    ).then((data) => normalizeOperation(data.operation)),
  operationPlan: (
    plan: Record<string, unknown>,
    options?: ApiCallOptions,
  ) =>
    v1Request<{ operation: unknown }>(
      "POST",
      "/api/v1/operations:plan",
      plan,
      options,
    ).then((data) => normalizeOperation(data.operation)),
  operationLogs: (operationId: string, options?: ApiCallOptions) =>
    collectCursorItems<unknown>(
      `/api/v1/operations/${encodeURIComponent(operationId)}/logs`,
      (data) => data.items,
      options,
    ).then(({ items }) =>
      items.map(normalizeOperationLog).slice(-MAX_OPERATION_LOGS),
    ),
  operationEvents: (
    operationId: string,
    lastEventId = "",
    options?: ApiCallOptions,
  ) => operationEventBatch(operationId, lastEventId, options),
  operationConfirm: (id: string) =>
    v1Request<Record<string, unknown>>(
      "POST",
      `/api/v1/operations/${encodeURIComponent(id)}:confirm`,
      {},
    ),
  operationCancel: (id: string) =>
    v1Request<Record<string, unknown>>(
      "POST",
      `/api/v1/operations/${encodeURIComponent(id)}:cancel`,
      {},
    ),
  operationRetry: (id: string) =>
    v1Request<Record<string, unknown>>(
      "POST",
      `/api/v1/operations/${encodeURIComponent(id)}:retry`,
      {},
    ),
  operationApply: (id: string, fields: Record<string, string> = {}) =>
    v1Request<Record<string, unknown>>(
      "POST",
      `/api/v1/operations/${encodeURIComponent(id)}:apply`,
      fields,
    ),
  operationRollback: (id: string, fields: Record<string, string> = {}) =>
    v1Request<Record<string, unknown>>(
      "POST",
      `/api/v1/operations/${encodeURIComponent(id)}:rollback`,
      fields,
    ),

  topology: async (topologyId = "primary", options?: ApiCallOptions) => {
    try {
      return await v1Request<TopologyDetail>(
        "GET",
        `/api/v1/topologies/${encodeURIComponent(topologyId)}`,
        undefined,
        options,
      );
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) return null;
      throw error;
    }
  },
  topologyList: (options?: ApiCallOptions) =>
    collectCursorItems<TopologyHeads>(
      "/api/v1/topologies",
      (data) => data.items,
      options,
    ).then(({ items }) => items),
  topologyRevisions: (topologyId = "primary", options?: ApiCallOptions) =>
    collectCursorItems<TopologyRevision>(
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/revisions`,
      (data) => data.items,
      options,
    ).then(({ items }) => items),
  topologyStatus: (topologyId = "primary", options?: ApiCallOptions) =>
    v1Request<{ status: TopologyStatus }>(
      "GET",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/status`,
      undefined,
      options,
    ).then((data) => data.status),
  topologyCreate: (spec: TopologySpec, options?: ApiCallOptions) =>
    v1Request<{ revision: TopologyRevision }>(
      "POST",
      "/api/v1/topologies",
      spec,
      options,
    ).then((data) => data.revision),
  topologyCreateRevision: (
    topologyId: string,
    spec: TopologySpec,
    expectedRevisionId: string,
    options: ApiCallOptions = {},
  ) =>
    v1Request<{ revision: TopologyRevision }>(
      "POST",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/revisions`,
      spec,
      { ...options, ifMatch: expectedRevisionId },
    ).then((data) => data.revision),
  topologyPutEndpoint: (
    topologyId: string,
    endpointId: string,
    endpoint: TopologySpec["endpoints"][number],
    expectedRevisionId: string,
    options: ApiCallOptions = {},
  ) =>
    v1Request<{ revision: TopologyRevision }>(
      "PUT",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/draft/endpoints/${encodeURIComponent(endpointId)}`,
      endpoint,
      { ...options, ifMatch: expectedRevisionId },
    ),
  topologyDeleteEndpoint: (
    topologyId: string,
    endpointId: string,
    expectedRevisionId: string,
    options: ApiCallOptions = {},
  ) =>
    v1Request<{ revision: TopologyRevision }>(
      "DELETE",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/draft/endpoints/${encodeURIComponent(endpointId)}`,
      {},
      { ...options, ifMatch: expectedRevisionId },
    ),
  topologyPutLink: (
    topologyId: string,
    sourceEndpoint: string,
    targetEndpoint: string,
    link: TopologySpec["links"][number],
    expectedRevisionId: string,
    options: ApiCallOptions = {},
  ) =>
    v1Request<{ revision: TopologyRevision }>(
      "PUT",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/draft/links/${encodeURIComponent(sourceEndpoint)}/${encodeURIComponent(targetEndpoint)}`,
      link,
      { ...options, ifMatch: expectedRevisionId },
    ),
  topologyDeleteLink: (
    topologyId: string,
    sourceEndpoint: string,
    targetEndpoint: string,
    expectedRevisionId: string,
    options: ApiCallOptions = {},
  ) =>
    v1Request<{ revision: TopologyRevision }>(
      "DELETE",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/draft/links/${encodeURIComponent(sourceEndpoint)}/${encodeURIComponent(targetEndpoint)}`,
      {},
      { ...options, ifMatch: expectedRevisionId },
    ),
  topologyValidate: (
    topologyId: string,
    spec: TopologySpec,
    options?: ApiCallOptions,
  ) =>
    v1Request<{ valid: boolean; content_sha256: string }>(
      "POST",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}:validate`,
      spec,
      options,
    ),
  topologyDiff: (
    topologyId: string,
    revisions: { from_revision_id?: string; to_revision_id?: string } = {},
    options?: ApiCallOptions,
  ) =>
    v1Request<{ diff: TopologyDiff }>(
      "POST",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}:diff`,
      revisions,
      options,
    ).then((data) => data.diff),
  topologyApply: (
    topologyId: string,
    revisionId: string,
    options: ApiCallOptions = {},
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}:apply`,
      {},
      { ...options, ifMatch: revisionId },
    ),
  topologyRollback: (
    topologyId: string,
    expectedRevisionId: string,
    revisionId: string,
    options: ApiCallOptions = {},
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      `/api/v1/topologies/${encodeURIComponent(topologyId)}:rollback`,
      { revision_id: revisionId },
      { ...options, ifMatch: expectedRevisionId },
    ),

  storeIndex: (_refresh = false, options?: ApiCallOptions) =>
    collectCursorItems<unknown>(
      "/api/v1/store/packages",
      (data) => data.items,
      options,
    ).then(({ items, pages }) => {
      const installed: Record<string, unknown> = {};
      for (const page of pages) {
        if (page.installed && typeof page.installed === "object") {
          Object.assign(installed, page.installed);
        }
      }
      return normalizeStoreIndex({ items, installed });
    }),
  storeImport: (
    payload: {
      service_id: string;
      target_node_id: string;
      version?: string;
      catalog_source_id?: string;
      channel?: string;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/store/releases:import",
      payload,
      options,
    ),
  storeValidate: (
    payload: {
      service_id: string;
      target_node_id: string;
      version?: string;
      catalog_source_id?: string;
      channel?: string;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<StoreValidationResult>(
      "POST",
      "/api/v1/store/releases:validate",
      payload,
      options,
    ),
  storeInstall: (
    payload: {
      service_id: string;
      version?: string;
      catalog_source_id?: string;
      channel?: string;
      target_node_id: string;
      mode?: "MANAGED" | "EXTERNAL";
      endpoint?: string;
      start?: boolean;
      migration_policy?: string;
      gateway_node_id?: string;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      "/api/v1/store/releases:install",
      { mode: "MANAGED", start: true, migration_policy: "APPLY", ...payload },
      options,
    ),
  deleteRelease: (
    serviceId: string,
    version: string,
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/store/releases:delete",
      { service_id: serviceId, version },
      options,
    ),
  storeUpgrade: (payload: Record<string, unknown>, options?: ApiCallOptions) =>
    v1Request<AsyncOperationResult>(
      "POST",
      "/api/v1/store/releases:upgrade",
      payload,
      options,
    ),
  storeRollback: (payload: Record<string, unknown>, options?: ApiCallOptions) =>
    v1Request<AsyncOperationResult>(
      "POST",
      "/api/v1/store/releases:rollback",
      payload,
      options,
    ),

  diagnostics: (options?: ApiCallOptions) =>
    collectCursorItems<Record<string, unknown>>(
      "/api/v1/diagnostics",
      (data) => data.items,
      options,
    ).then(({ items }) => items),
  createDiagnostic: (options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/diagnostics",
      {},
      options,
    ),
  diagnostic: (diagnosticId: string, options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "GET",
      `/api/v1/diagnostics/${encodeURIComponent(diagnosticId)}`,
      undefined,
      options,
    ),
  exportDiagnostic: (
    diagnosticId: string,
    format: "json" | "md",
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "GET",
      `/api/v1/diagnostics/${encodeURIComponent(diagnosticId)}.${format}`,
      undefined,
      options,
    ),

  getLayout: (options?: ApiCallOptions) =>
    v1Request<{ layout: LayoutState }>(
      "GET",
      "/api/v1/ui/layout",
      undefined,
      options,
    ).then((data) =>
      data.layout && typeof data.layout === "object" ? data.layout : {},
    ),
  putLayout: (layout: LayoutState, options?: ApiCallOptions) =>
    v1Request<{ layout: LayoutState }>(
      "PUT",
      "/api/v1/ui/layout",
      layout,
      options,
    ),
};

/** 在任意 action_result JSON 里递归找 operation_id。 */
export function findOperationId(value: unknown): string | null {
  if (!value || typeof value !== "object") return null;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findOperationId(item);
      if (found) return found;
    }
    return null;
  }
  const record = value as Record<string, unknown>;
  if (typeof record.operation_id === "string" && record.operation_id) {
    return record.operation_id;
  }
  for (const key of Object.keys(record)) {
    const found = findOperationId(record[key]);
    if (found) return found;
  }
  return null;
}
