import type { ApiCallOptions } from "../../shared/api/transport";
import { v1Request } from "../../shared/api/transport";
import type { CapabilityRow, HealthInfo } from "../../types";
import { normalizeHealth } from "./normalizers";
import { arrayOrEmpty } from "../../shared/api/values";

export const controlPlaneApi = {
  health: (options?: ApiCallOptions) =>
    v1Request<HealthInfo>(
      "GET",
      "/api/v1/healthz/ready",
      undefined,
      options,
    ).then(normalizeHealth),
  capabilities: (options?: ApiCallOptions) =>
    v1Request<{ actions?: CapabilityRow[] }>(
      "GET",
      "/api/v1/capabilities",
      undefined,
      options,
    ).then((data) =>
      arrayOrEmpty<CapabilityRow>(data.actions).filter(
        (capability) =>
          typeof capability?.action === "string" &&
          capability.capability_status?.toUpperCase() !== "UNSUPPORTED",
      ),
    ),
};
