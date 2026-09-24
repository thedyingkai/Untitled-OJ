import type { ApiCallOptions } from "../../shared/api/transport";
import { collectCursorItems } from "../../shared/api/pagination";
import type { OperationRow } from "../../types";
import { MAX_OPERATION_LOGS, normalizeOperation, normalizeOperationLog } from "./model";
import { v1Request } from "../../shared/api/transport";
import { operationEventBatch } from "./events";

export const operationsApi = {
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
operationPlan: (plan: Record<string, unknown>, options?: ApiCallOptions) =>
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
};
