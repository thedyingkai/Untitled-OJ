import type { ApiBinding, ApiProviderCandidate } from "../../types";
import {
  booleanOr,
  numberOr,
  objectOrEmpty,
  stringsOrEmpty,
  textOr,
} from "../../shared/api/values";

export function normalizeApiProviderCandidate(
  value: unknown,
): ApiProviderCandidate {
  const row = objectOrEmpty(value);
  return {
    deployment_id: textOr(
      row.deployment_id,
      textOr(row.provider_deployment_id),
    ),
    service_id: textOr(row.service_id, textOr(row.provider_service_id)),
    node_id: textOr(row.node_id, textOr(row.provider_node_id)),
    endpoint: textOr(row.endpoint, textOr(row.provider_endpoint)),
    path: textOr(row.path, textOr(row.provider_path)),
    api_id: textOr(row.api_id),
    api_version: textOr(row.api_version, textOr(row.version)),
    protocol: textOr(row.protocol),
    methods: stringsOrEmpty(row.methods),
    auth_mode: textOr(row.auth_mode),
    permission: textOr(row.permission),
    healthy: booleanOr(
      row.healthy,
      textOr(row.health).toUpperCase() === "HEALTHY",
    ),
    recommended: booleanOr(row.recommended),
    reason: textOr(row.reason),
  };
}

export function normalizeApiBinding(value: unknown): ApiBinding {
  const row = objectOrEmpty(value);
  return {
    binding_id: textOr(row.binding_id),
    requirement_name: textOr(row.requirement_name, textOr(row.name)),
    api_id: textOr(row.api_id),
    api_version: textOr(row.api_version, textOr(row.version)),
    consumer_deployment_id: textOr(row.consumer_deployment_id),
    consumer_service_id: textOr(row.consumer_service_id),
    consumer_node_id: textOr(row.consumer_node_id),
    consumer_endpoint: textOr(row.consumer_endpoint),
    provider_deployment_id: textOr(row.provider_deployment_id),
    provider_service_id: textOr(row.provider_service_id),
    provider_node_id: textOr(row.provider_node_id),
    provider_endpoint: textOr(row.provider_endpoint),
    provider_path: textOr(row.provider_path),
    virtual_endpoint: textOr(row.virtual_endpoint, textOr(row.gateway_path)),
    protocol: textOr(row.protocol),
    methods: stringsOrEmpty(row.methods),
    auth_mode: textOr(row.auth_mode),
    provider_auth_mode: textOr(row.provider_auth_mode),
    permission: textOr(row.permission),
    timeout_ms:
      typeof row.timeout_ms === "number" && Number.isFinite(row.timeout_ms)
        ? row.timeout_ms
        : null,
    topology_id: textOr(row.topology_id),
    topology_revision_id: textOr(
      row.topology_revision_id,
      textOr(row.revision_id),
    ),
    link_source_endpoint: textOr(row.link_source_endpoint),
    link_target_endpoint: textOr(row.link_target_endpoint),
    credential_generation: numberOr(row.credential_generation),
    context_generation: numberOr(row.context_generation),
    desired_state: textOr(row.desired_state),
    observed_state: textOr(row.observed_state),
    health: textOr(row.health, "UNKNOWN"),
    drift: stringsOrEmpty(row.drift),
    last_operation_id: textOr(row.last_operation_id),
    state: textOr(row.state, textOr(row.observed_state, "UNKNOWN")),
    optional: booleanOr(row.optional),
    reason: textOr(row.reason),
    updated_at: textOr(row.updated_at),
  };
}
