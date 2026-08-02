import type {
  DeploymentRow,
  EndpointRow,
  GithubRelease,
  HealthInfo,
  LayoutState,
  LinkRow,
  OperationLog,
  OperationRow,
  ServiceRow,
  StoreIndexResponse,
  StoreStatus,
  TopologyData,
} from "./types";
import {
  ORCHESTRATOR_TOKEN_HEADER,
  markAuthRequired,
  orchestratorToken,
} from "./token";

type OperationApiRow = Omit<OperationRow, "rollback_available"> & {
  rollback_available?: boolean;
};

/**
 * 控制面令牌缺失/错误时 daemon 返回的 401。单独成类，方便调用方（尤其是轮询）
 * 区分“未授权”和“连不上/业务失败”，避免重复弹 toast。
 */
export class AuthRequiredError extends Error {
  readonly status = 401;

  constructor(message = "编排器启用了控制面令牌，请先填写访问令牌") {
    super(message);
    this.name = "AuthRequiredError";
  }
}

export function isAuthRequiredError(err: unknown): err is AuthRequiredError {
  return err instanceof AuthRequiredError;
}

/**
 * core dispatcher 判定「没做 / 没执行成 / 被拦下」时用的 action_result.status。
 * 取值来自 orchestrator core：`dispatch_unsupported` 固定写 `UNSUPPORTED`，
 * executor 失败分支固定写 `FAILED`（`OrchestratorError::Blocked` 也会落在这里）；
 * `BLOCKED` 目前 core 不产出，作为同义状态一并防御。
 */
const FAILED_ACTION_STATUSES = new Set(["UNSUPPORTED", "FAILED", "BLOCKED"]);

/** 递归查找嵌套的 action_result 时的最大层数，避免在超大响应体上空转。 */
const ACTION_RESULT_SCAN_DEPTH = 6;

/**
 * 在响应体里找出「失败的」action_result。
 *
 * 只认 `action_result` 这个键：`/operations/{id}` 这类查询返回的 operation 里同样有
 * status（SUCCEEDED / FAILED / PLANNED …），递归匹配任意 status 会把只读查询误判成失败。
 * 命中的 action_result 自身不再向下递归——它的 result / logs 里全是业务 JSON，
 * 只会带来噪声。
 */
function findFailedActionResult(
  value: unknown,
  depth = 0,
): Record<string, unknown> | null {
  if (depth > ACTION_RESULT_SCAN_DEPTH || !value || typeof value !== "object") {
    return null;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findFailedActionResult(item, depth + 1);
      if (found) return found;
    }
    return null;
  }
  const record = value as Record<string, unknown>;
  const candidate = record.action_result;
  if (candidate && typeof candidate === "object" && !Array.isArray(candidate)) {
    const status = (candidate as Record<string, unknown>).status;
    if (
      typeof status === "string" &&
      FAILED_ACTION_STATUSES.has(status.trim().toUpperCase())
    ) {
      return candidate as Record<string, unknown>;
    }
  }
  for (const [key, nested] of Object.entries(record)) {
    if (key === "action_result") continue;
    const found = findFailedActionResult(nested, depth + 1);
    if (found) return found;
  }
  return null;
}

function actionResultErrorMessage(
  actionResult: Record<string, unknown>,
): string {
  const text = (key: string) => {
    const value = actionResult[key];
    return typeof value === "string" ? value.trim() : "";
  };
  const status = text("status") || "FAILED";
  const detail =
    text("message") || text("error") || `${text("action_id") || "action"} 未能执行`;
  return `${status}: ${detail}`;
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const init: RequestInit = { method };
  const headers: Record<string, string> = {};
  // 配置了令牌才发头：未启用令牌的开发 daemon 依然 fail-open。
  const token = orchestratorToken();
  if (token) headers[ORCHESTRATOR_TOKEN_HEADER] = token;
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  if (Object.keys(headers).length) init.headers = headers;
  let response: Response;
  try {
    response = await fetch(path, init);
  } catch (err) {
    throw new Error(`无法连接编排器 daemon：${String(err)}`);
  }
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
  if (response.status === 401) {
    markAuthRequired();
    throw new AuthRequiredError(
      typeof data?.message === "string" && data.message
        ? `控制面未授权：${data.message}`
        : undefined,
    );
  }
  if (!parsed) {
    throw new Error(`响应不是 JSON（HTTP ${response.status}）`);
  }
  if (!response.ok || data?.status === "error") {
    throw new Error(data?.message || `HTTP ${response.status}`);
  }
  // HTTP 200 + 顶层 status=ok 不代表 action 真的执行了：core 把 UNSUPPORTED / FAILED
  // 放在 action_result.status 里，daemon 只负责把它原样透出。这里补一道判定，
  // 让 ServicesView 的启停、StoreView 的卸载等调用走 catch 分支。
  const failedAction = findFailedActionResult(data);
  if (failedAction) {
    throw new Error(actionResultErrorMessage(failedAction));
  }
  return data as T;
}

export const api = {
  health: () => request<HealthInfo>("GET", "/health"),
  services: () =>
    request<{ services: ServiceRow[] }>("GET", "/services").then(
      (data) => data.services ?? [],
    ),
  deployments: () =>
    request<{ deployments: DeploymentRow[] }>("GET", "/deployments").then(
      (data) => data.deployments ?? [],
    ),
  endpoints: () =>
    request<{ endpoints: EndpointRow[] }>("GET", "/endpoints").then(
      (data) => data.endpoints ?? [],
    ),
  links: () =>
    request<{ links: LinkRow[] }>("GET", "/links").then(
      (data) => data.links ?? [],
    ),
  operations: () =>
    request<{ operations: OperationApiRow[] }>("GET", "/operations").then(
      (data): OperationRow[] =>
        (data.operations ?? []).map((operation) => ({
          ...operation,
          // 兼容旧 daemon，以及不带运行记录的目录行和目录错误行。
          rollback_available: operation.rollback_available === true,
        })),
    ),
  operationLogs: (operationId: string) =>
    request<{ logs: OperationLog[] }>(
      "GET",
      `/operations/${encodeURIComponent(operationId)}/logs`,
    ).then((data) => data.logs ?? []),
  topology: () =>
    request<{ topology: TopologyData }>("GET", "/topology").then(
      (data) => data.topology,
    ),

  createEndpoint: (payload: {
    endpoint: string;
    service_id: string;
    protocol: string;
    health_path?: string;
  }) => request<any>("POST", "/endpoints", payload),
  deleteEndpoint: (endpoint: string) =>
    request<any>("DELETE", `/endpoints/${encodeURIComponent(endpoint)}`, {}),
  checkEndpointHealth: (endpoint: string) =>
    request<any>(
      "POST",
      `/endpoints/${encodeURIComponent(endpoint)}/health`,
      {},
    ),

  createLink: (payload: {
    source_endpoint: string;
    target_endpoint: string;
    protocol: string;
  }) =>
    request<any>("POST", "/links", payload),
  deleteLink: (source: string, target: string) =>
    request<any>(
      "DELETE",
      `/links/${encodeURIComponent(source)}/${encodeURIComponent(target)}`,
      {},
    ),
  checkLinkHealth: (source: string, target: string) =>
    request<any>(
      "POST",
      `/links/${encodeURIComponent(source)}/${encodeURIComponent(target)}/health`,
      {},
    ),
  /** link.enable：daemon 侧会补 confirm=true，这里只需空 body。 */
  enableLink: (source: string, target: string) =>
    request<any>(
      "POST",
      `/links/${encodeURIComponent(source)}/${encodeURIComponent(target)}/enable`,
      {},
    ),
  /** link.disable：同上，走 Operation 链（planned + confirm）。 */
  disableLink: (source: string, target: string) =>
    request<any>(
      "POST",
      `/links/${encodeURIComponent(source)}/${encodeURIComponent(target)}/disable`,
      {},
    ),

  dispatchAction: (action: string, fields: Record<string, string>) =>
    request<any>("POST", "/actions", { action, ...fields }),

  installRelease: (serviceId: string, fields: Record<string, string> = {}) =>
    request<any>(
      "POST",
      `/releases/${encodeURIComponent(serviceId)}/install`,
      fields,
    ),
  rollbackRelease: (
    serviceId: string,
    fields: Record<string, string> = {},
  ) =>
    request<any>(
      "POST",
      `/releases/${encodeURIComponent(serviceId)}/rollback`,
      fields,
    ),
  deleteRelease: (
    serviceId: string,
    fields: Record<string, string> = {},
  ) =>
    request<any>(
      "DELETE",
      `/releases/${encodeURIComponent(serviceId)}`,
      fields,
    ),

  operationConfirm: (id: string) =>
    request<any>("POST", `/operations/${encodeURIComponent(id)}/confirm`, {}),
  operationApply: (id: string, fields: Record<string, string> = {}) =>
    request<any>("POST", `/operations/${encodeURIComponent(id)}/apply`, fields),
  operationRollback: (id: string, fields: Record<string, string> = {}) =>
    request<any>(
      "POST",
      `/operations/${encodeURIComponent(id)}/rollback`,
      fields,
    ),

  storeStatus: () => request<StoreStatus>("GET", "/store/status"),
  storeIndex: (refresh = false) =>
    request<StoreIndexResponse>(
      "GET",
      `/store/index${refresh ? "?refresh=1" : ""}`,
    ),
  githubReleases: (repo: string) =>
    request<{ releases: GithubRelease[] }>(
      "GET",
      `/store/github/releases?repo=${encodeURIComponent(repo)}`,
    ).then((data) => data.releases ?? []),
  storeImport: (payload: { source_url: string; checksum?: string }) =>
    request<any>("POST", "/store/import", payload),
  storeInstall: (payload: Record<string, unknown>) =>
    request<any>("POST", "/store/install", payload),

  getLayout: () =>
    request<{ layout: LayoutState }>("GET", "/ui/layout").then(
      (data) => data.layout ?? {},
    ),
  putLayout: (layout: LayoutState) => request<any>("PUT", "/ui/layout", layout),
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
