import type { ApiCallOptions } from "../../shared/api/transport";
import { collectCursorItems } from "../../shared/api/pagination";
import { normalizeNode } from "./normalizers";
import { v1Request } from "../../shared/api/transport";
import type { AsyncOperationResult } from "../../types";

export const nodesApi = {
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
};
