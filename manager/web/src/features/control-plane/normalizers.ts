import type { HealthInfo } from "../../types";
import { stringsOrEmpty, textOr } from "../../shared/api/values";

export function normalizeHealth(value: unknown): HealthInfo {
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
