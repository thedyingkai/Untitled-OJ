// Compatibility entry for existing clients. Implementations belong to feature modules.
import { controlPlaneApi } from "./features/control-plane/api";
import { catalogApi } from "./features/catalog/api";
import { nodesApi } from "./features/nodes/api";
import { deploymentsApi } from "./features/deployments/api";
import { operationsApi } from "./features/operations/api";
import { topologyApi } from "./features/topology/api";
import { storeApi } from "./features/store/api";
import { diagnosticsApi } from "./features/diagnostics/api";
import { layoutApi } from "./features/topology/layout-api";

export type { ApiCallOptions } from "./shared/api/transport";
export { DEFAULT_READ_TIMEOUT_MS } from "./shared/api/transport";
export { DEFAULT_MUTATION_TIMEOUT_MS } from "./shared/api/transport";
export { MAX_OPERATION_LOGS } from "./features/operations/model";
export { ApiError } from "./shared/api/transport";
export { RequestTimeoutError } from "./shared/api/transport";
export { RequestCancelledError } from "./shared/api/transport";
export { isRequestCancelled } from "./shared/api/transport";
export { AuthRequiredError } from "./shared/api/transport";
export { isAuthRequiredError } from "./shared/api/transport";
export { normalizeApiBinding } from "./features/topology/bindings";
export { normalizeStoreValidation } from "./features/store/normalizers";
export { normalizeOperationLog } from "./features/operations/model";
export { request } from "./shared/api/transport";
export { v1Request } from "./shared/api/transport";
export type { OperationStreamEvent } from "./features/operations/events";
export type { OperationEventBatch } from "./features/operations/events";
export { operationEventBatch } from "./features/operations/events";
export { parseOperationEventStream } from "./features/operations/events";
export { findOperationId } from "./features/operations/model";

export const api = {
  ...controlPlaneApi,
  ...catalogApi,
  ...nodesApi,
  ...deploymentsApi,
  ...operationsApi,
  ...topologyApi,
  ...storeApi,
  ...diagnosticsApi,
  ...layoutApi,
};
