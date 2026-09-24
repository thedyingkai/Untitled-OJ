import type { NodeRow } from "../../types";
import { textOr } from "../../shared/api/values";

export function normalizeNode(value: unknown): NodeRow {
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
