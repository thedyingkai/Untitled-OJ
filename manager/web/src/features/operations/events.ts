import type { ApiCallOptions } from "../../shared/api/transport";
import {
  ApiError,
  AuthRequiredError,
  DEFAULT_READ_TIMEOUT_MS,
  RequestCancelledError,
  RequestTimeoutError,
  responseMessage,
  waitForPromise,
} from "../../shared/api/transport";
import { markAuthRequired } from "../../auth";

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
    if (
      !response.headers.get("content-type")?.startsWith("text/event-stream")
    ) {
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
