import type { OperationLog, OperationRow } from "../../types";
import { arrayOrEmpty, booleanOr, numberOr, textOr } from "../../shared/api/values";

export const MAX_OPERATION_LOGS = 500;

export function normalizeOperation(value: unknown): OperationRow {
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
    summary: textOr(
      row.summary,
      `${textOr(row.action, "operation")} ${target}`.trim(),
    ),
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
    step_id: textOr(
      row.step_id,
      textOr(row.job_id, textOr(row.event_type, "runtime")),
    ),
    level: textOr(row.level, "info").toLowerCase(),
    message: textOr(row.message),
    created_at: textOr(
      row.created_at,
      createdAtMs > 0 ? new Date(createdAtMs).toISOString() : "",
    ),
  };
}

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
