import type { ApiCallOptions } from "../../shared/api/transport";
import { collectCursorItems } from "../../shared/api/pagination";
import { normalizeStoreIndex, normalizeStoreValidation } from "./normalizers";
import { v1Request } from "../../shared/api/transport";
import type { AsyncOperationResult, InstallApiBindingSelection, InstallTopologySelection, ReplacementTopologyCas, StorePipelineOptions } from "../../types";

export const storeApi = {
storeIndex: (_refresh = false, options?: ApiCallOptions) =>
    collectCursorItems<unknown>(
      "/api/v1/store/packages",
      (data) => data.items,
      options,
    ).then(({ items, pages }) => {
      const installed: Record<string, unknown> = {};
      for (const page of pages) {
        if (page.installed && typeof page.installed === "object") {
          Object.assign(installed, page.installed);
        }
      }
      return normalizeStoreIndex({ items, installed });
    }),
storeImport: (
    payload: {
      service_id: string;
      target_node_id: string;
      version?: string;
      catalog_source_id?: string;
      channel?: string;
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/store/releases:import",
      payload,
      options,
    ),
storeValidate: (
    payload: {
      service_id: string;
      target_node_id: string;
      version?: string;
      catalog_source_id?: string;
      channel?: string;
      endpoint?: string;
      bindings?: InstallApiBindingSelection[];
      topology_id?: string;
      topology_etag?: string;
      /** 0.2 compatibility only. */
      topology?: InstallTopologySelection;
    } & StorePipelineOptions,
    options?: ApiCallOptions,
  ) =>
    v1Request<unknown>(
      "POST",
      "/api/v1/store/releases:validate",
      {
        start: true,
        migration_policy: "APPLY",
        config: {},
        secret_refs: {},
        ...payload,
      },
      options,
    ).then(normalizeStoreValidation),
storeInstall: (
    payload: {
      service_id: string;
      version?: string;
      catalog_source_id?: string;
      channel?: string;
      target_node_id: string;
      mode?: "MANAGED" | "EXTERNAL";
      endpoint?: string;
      bindings?: InstallApiBindingSelection[];
      topology_id?: string;
      topology_etag?: string;
      /** 0.2 compatibility only. */
      topology?: InstallTopologySelection;
    } & StorePipelineOptions,
    options?: ApiCallOptions,
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      "/api/v1/store/releases:install",
      {
        mode: "MANAGED",
        start: true,
        migration_policy: "APPLY",
        config: {},
        secret_refs: {},
        ...payload,
      },
      options,
    ),
deleteRelease: (
    serviceId: string,
    version: string,
    options?: ApiCallOptions,
  ) =>
    v1Request<Record<string, unknown>>(
      "POST",
      "/api/v1/store/releases:delete",
      { service_id: serviceId, version },
      options,
    ),
storeUpgrade: (
    payload: {
      deployment_id: string;
      version?: string;
      catalog_source_id?: string;
      bindings?: InstallApiBindingSelection[];
      topology_id?: string;
      topology_etag?: string;
      topologies?: ReplacementTopologyCas[];
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      "/api/v1/store/releases:upgrade",
      payload,
      options,
    ),
storeRollback: (
    payload: {
      deployment_id: string;
      version?: string;
      catalog_source_id?: string;
      bindings?: InstallApiBindingSelection[];
      topology_id?: string;
      topology_etag?: string;
      topologies?: ReplacementTopologyCas[];
    },
    options?: ApiCallOptions,
  ) =>
    v1Request<AsyncOperationResult>(
      "POST",
      "/api/v1/store/releases:rollback",
      payload,
      options,
    ),
};
