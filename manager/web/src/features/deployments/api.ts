import type { ApiCallOptions } from "../../shared/api/transport";
import { collectCursorItems } from "../../shared/api/pagination";
import { normalizeDeployment } from "./normalizers";
import { v1Request } from "../../shared/api/transport";
import type { AsyncOperationResult, DeploymentBindings } from "../../types";
import { arrayOrEmpty, textOr } from "../../shared/api/values";
import { normalizeApiBinding } from "../topology/bindings";

export const deploymentsApi = {
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
deploymentBindings: (deploymentId: string, options?: ApiCallOptions) =>
    v1Request<Record<string, unknown>>(
      "GET",
      `/api/v1/deployments/${encodeURIComponent(deploymentId)}/bindings`,
      undefined,
      options,
    ).then((data): DeploymentBindings => ({
      deployment_id: textOr(data.deployment_id, deploymentId),
      service_id: textOr(data.service_id),
      items: arrayOrEmpty<unknown>(data.items ?? data.bindings).map(
        normalizeApiBinding,
      ),
      provider_items: arrayOrEmpty<unknown>(data.provider_items).map(
        normalizeApiBinding,
      ),
    })),
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
resourcePurge: (
    claimId: string,
    input: {
      node_id: string;
      claim_digest: string;
      generation: number;
      confirmation: string;
      reason: string;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      `/api/v1/resources/${encodeURIComponent(claimId)}:purge`,
      input,
      options,
    ),
};
