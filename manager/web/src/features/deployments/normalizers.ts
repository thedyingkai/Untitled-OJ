import type { DeploymentRow } from "../../types";
import {
  booleanOr,
  numberOr,
  stringsOrEmpty,
  textOr,
} from "../../shared/api/values";

export function normalizeDeployment(value: unknown): DeploymentRow {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  const instance =
    row.instance &&
    typeof row.instance === "object" &&
    !Array.isArray(row.instance)
      ? (row.instance as Record<string, unknown>)
      : row;
  const observedState = textOr(
    instance.observed_state,
    textOr(row.status, "UNKNOWN"),
  );
  const health = textOr(
    instance.health,
    textOr(row.endpoint_health, "UNKNOWN"),
  );
  const runtimeContract =
    instance.runtime_contract &&
    typeof instance.runtime_contract === "object" &&
    !Array.isArray(instance.runtime_contract)
      ? (instance.runtime_contract as Record<string, unknown>)
      : {};
  return {
    deployment_id: textOr(instance.deployment_id, textOr(row.deployment_id)),
    node_id: textOr(row.node_id),
    service_id: textOr(instance.service_id, textOr(row.service_id)),
    name: textOr(row.name),
    version: textOr(row.version),
    kind: textOr(row.kind, "container"),
    runtime: textOr(row.runtime, "docker"),
    host_ip: textOr(row.host_ip, textOr(row.node_id)),
    status: observedState,
    endpoint: textOr(row.endpoint),
    protocol: textOr(row.protocol),
    health_path: textOr(row.health_path),
    endpoint_health: health,
    reachable: booleanOr(row.reachable, health.toUpperCase() === "HEALTHY"),
    endpoint_count: numberOr(row.endpoint_count),
    endpoints: stringsOrEmpty(row.endpoints),
    container_id: textOr(instance.container_id),
    artifact_digest: textOr(instance.artifact_digest),
    release_version: textOr(instance.release_version, textOr(row.version)),
    runtime_profile: textOr(runtimeContract.id, textOr(row.runtime_profile)),
    runtime_profile_sha256: textOr(
      runtimeContract.profile_sha256,
      textOr(row.runtime_profile_sha256),
    ),
    runtime_policy_sha256: textOr(
      instance.runtime_policy_sha256,
      textOr(row.runtime_policy_sha256),
    ),
    effective_host_config_sha256: textOr(
      instance.effective_runtime_sha256,
      textOr(row.effective_host_config_sha256, textOr(row.host_config_digest)),
    ),
    runtime_attested: booleanOr(
      instance.runtime_attested,
      booleanOr(row.runtime_attested),
    ),
    last_observed_at_ms: numberOr(row.last_observed_at_ms),
    drift_reason: textOr(row.drift_reason),
    credential_expires_at_ms: numberOr(row.credential_expires_at_ms),
    credential_last_success_at_ms: numberOr(row.credential_last_success_at_ms),
    credential_last_error: textOr(row.credential_last_error),
    desired_state: textOr(instance.desired_state, "UNKNOWN"),
    observed_state: observedState,
    updated_at: textOr(row.updated_at),
  };
}
