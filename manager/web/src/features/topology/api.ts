import type { ApiCallOptions } from "../../shared/api/transport";
import { ApiError, v1Request } from "../../shared/api/transport";
import type {
  AsyncOperationResult,
  TopologyDetail,
  TopologyDiff,
  TopologyHeads,
  TopologyRevision,
  TopologySpec,
  TopologyStatus,
} from "../../types";
import { collectCursorItems } from "../../shared/api/pagination";

export const topologyApi = {
  topology: async (topologyId: string, options?: ApiCallOptions) => {
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
  topologyRevisions: (topologyId: string, options?: ApiCallOptions) =>
    collectCursorItems<TopologyRevision>(
      `/api/v1/topologies/${encodeURIComponent(topologyId)}/revisions`,
      (data) => data.items,
      options,
    ).then(({ items }) => items),
  topologyStatus: (topologyId: string, options?: ApiCallOptions) =>
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
};
