import { markAuthRequired } from "../../auth";

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
  if (
    typeof crypto !== "undefined" &&
    typeof crypto.randomUUID === "function"
  ) {
    return crypto.randomUUID();
  }
  idempotencySequence += 1;
  return `web-${Date.now().toString(36)}-${idempotencySequence.toString(36)}`;
}

function isMutation(method: string): boolean {
  return !["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase());
}

export function waitForPromise<T>(
  promise: Promise<T>,
  signal: AbortSignal,
): Promise<T> {
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

export function responseMessage(data: any, status: number): string {
  for (const value of [data?.detail, data?.message, data?.title, data?.error]) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return `HTTP ${status}`;
}

export async function request<T>(
  method: string,
  path: string,
  body?: unknown,
  options: ApiCallOptions = {},
): Promise<T> {
  const timeoutMs =
    options.timeoutMs ??
    (isMutation(method)
      ? DEFAULT_MUTATION_TIMEOUT_MS
      : DEFAULT_READ_TIMEOUT_MS);
  const controller = new AbortController();
  let timedOut = false;
  const timeout = window.setTimeout(
    () => {
      timedOut = true;
      controller.abort("timeout");
    },
    Math.max(1, timeoutMs),
  );
  const abortFromCaller = () => controller.abort(options.signal?.reason);
  if (options.signal?.aborted) abortFromCaller();
  else
    options.signal?.addEventListener("abort", abortFromCaller, { once: true });

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
      response.headers.get("x-request-id") ||
      data?.meta?.request_id ||
      data?.request_id ||
      "";
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
