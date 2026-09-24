import type {
  ApiBinding,
  ApiBindingRequirementPlan,
  NodeRuntimeValidation,
  StoreIndexResponse,
  StoreModule,
  StoreValidationResult,
  TopologyDiff,
} from "../../types";
import {
  arrayOrEmpty,
  booleanOr,
  numberOr,
  objectOrEmpty,
  stringsOrEmpty,
  textOr,
} from "../../shared/api/values";
import {
  normalizeApiBinding,
  normalizeApiProviderCandidate,
} from "../topology/bindings";

function normalizeBindingRequirement(
  value: unknown,
  resolvedBindings: ApiBinding[],
): ApiBindingRequirementPlan {
  const row = objectOrEmpty(value);
  const name = textOr(row.name, textOr(row.requirement_name));
  const rawCandidates = arrayOrEmpty<unknown>(
    row.candidates ?? row.provider_candidates ?? row.compatible_providers,
  );
  const candidates = rawCandidates.map(normalizeApiProviderCandidate);
  const resolved = resolvedBindings.find(
    (binding) => binding.requirement_name === name,
  );
  if (resolved?.provider_deployment_id && candidates.length === 0) {
    candidates.push(
      normalizeApiProviderCandidate({
        provider_deployment_id: resolved.provider_deployment_id,
        provider_service_id: resolved.provider_service_id,
        provider_node_id: resolved.provider_node_id,
        provider_endpoint: resolved.provider_endpoint,
        provider_path: resolved.provider_path,
        api_id: resolved.api_id,
        api_version: resolved.api_version,
        protocol: resolved.protocol,
        methods: resolved.methods,
        auth_mode: resolved.provider_auth_mode || resolved.auth_mode,
        permission: resolved.permission,
        health: resolved.health,
      }),
    );
  }
  const explicitRecommendation = textOr(
    row.recommended_provider_deployment_id,
    textOr(row.recommended_deployment_id),
  );
  const markedRecommendation = candidates.find(
    (candidate) => candidate.recommended,
  )?.deployment_id;
  const recommended =
    explicitRecommendation ||
    markedRecommendation ||
    (candidates.length === 1 && candidates[0]?.healthy
      ? candidates[0].deployment_id
      : "");
  return {
    name,
    api_id: textOr(row.api_id, resolved?.api_id ?? ""),
    version: textOr(
      row.version,
      textOr(row.version_requirement, resolved?.api_version ?? ""),
    ),
    optional: booleanOr(row.optional, resolved?.optional ?? false),
    selection: textOr(row.selection, "explicit"),
    candidates,
    recommended_provider_deployment_id: recommended,
    ambiguous: booleanOr(
      row.ambiguous,
      candidates.filter((candidate) => candidate.healthy).length > 1 &&
        !recommended,
    ),
    reason: textOr(row.reason),
  };
}

function normalizeRuntimeValidation(
  value: unknown,
): NodeRuntimeValidation | null {
  const row = objectOrEmpty(value);
  if (!Object.keys(row).length) return null;
  const facts = Object.keys(objectOrEmpty(row.facts)).length
    ? objectOrEmpty(row.facts)
    : row;
  const docker = objectOrEmpty(facts.docker ?? row.docker);
  const contracts = arrayOrEmpty<unknown>(
    facts.allowed_contracts ?? row.allowed_contracts,
  ).map((contract) => {
    const item = objectOrEmpty(contract);
    return {
      id: textOr(item.id),
      profile_sha256: textOr(item.profile_sha256, textOr(item.sha256)),
    };
  });
  const selectedRow = objectOrEmpty(row.selected_contract ?? row.contract);
  return {
    node_id: textOr(row.node_id),
    report_id: textOr(facts.report_id, textOr(row.report_id)),
    observed_at_ms: numberOr(
      row.observed_at_ms,
      numberOr(facts.observed_at_ms),
    ),
    received_at_ms: numberOr(row.received_at_ms),
    stale_after_ms: numberOr(row.stale_after_ms, 60_000),
    agent_version: textOr(facts.agent_version, textOr(row.agent_version)),
    runtime_policy_sha256: textOr(
      facts.runtime_policy_sha256,
      textOr(row.runtime_policy_sha256),
    ),
    allowed_contracts: contracts,
    judge_sandbox_allowed_images: stringsOrEmpty(
      facts.judge_sandbox_allowed_images ?? row.judge_sandbox_allowed_images,
    ),
    inventory_complete: booleanOr(
      facts.inventory_complete,
      booleanOr(row.inventory_complete),
    ),
    inventory_error: textOr(facts.inventory_error, textOr(row.inventory_error)),
    selected_contract: Object.keys(selectedRow).length
      ? {
          id: textOr(selectedRow.id),
          profile_sha256: textOr(
            selectedRow.profile_sha256,
            textOr(selectedRow.sha256),
          ),
        }
      : null,
    docker: {
      engine: textOr(docker.engine),
      server_version: textOr(docker.server_version),
      operating_system: textOr(docker.operating_system),
      os_type: textOr(docker.os_type),
      architecture: textOr(docker.architecture),
      cgroup_version: textOr(docker.cgroup_version),
      memory_limit: booleanOr(docker.memory_limit),
      pids_limit: booleanOr(docker.pids_limit),
      rootless: booleanOr(docker.rootless),
      apparmor: booleanOr(docker.apparmor),
      seccomp: booleanOr(docker.seccomp),
      security_options: stringsOrEmpty(docker.security_options),
    },
  };
}

export function normalizeStoreValidation(
  value: unknown,
): StoreValidationResult {
  const row = objectOrEmpty(value);
  const bindings = arrayOrEmpty<unknown>(row.bindings).map(normalizeApiBinding);
  const plan = objectOrEmpty(row.plan);
  const requirementValues = arrayOrEmpty<unknown>(
    row.requirements ??
      row.binding_requirements ??
      plan.requirements ??
      plan.binding_requirements,
  );
  const requirements = requirementValues.map((requirement) =>
    normalizeBindingRequirement(requirement, bindings),
  );
  for (const binding of bindings) {
    if (
      binding.requirement_name &&
      !requirements.some(
        (requirement) => requirement.name === binding.requirement_name,
      )
    ) {
      requirements.push(
        normalizeBindingRequirement(
          {
            name: binding.requirement_name,
            api_id: binding.api_id,
            version: binding.api_version,
            optional: binding.optional,
          },
          bindings,
        ),
      );
    }
  }
  const topology = objectOrEmpty(row.topology);
  const rawDiff = row.topology_diff ?? row.diff;
  const sideEffects = objectOrEmpty(row.side_effects);
  const targetPlatform = objectOrEmpty(row.target_platform);
  const composition = objectOrEmpty(row.composition_plan);
  const compositionNodes = arrayOrEmpty<unknown>(composition.nodes).map(
    (value) => {
      const node = objectOrEmpty(value);
      const provider = objectOrEmpty(node.provider);
      const providerCandidates = arrayOrEmpty<unknown>(provider.candidates).map(
        (value) => {
          const candidate = objectOrEmpty(value);
          return {
            providerId: textOr(
              candidate.providerId,
              textOr(candidate.provider_id),
            ),
            version: textOr(candidate.version),
            kind: textOr(candidate.kind).toUpperCase(),
            ...(textOr(candidate.serviceId, textOr(candidate.service_id))
              ? {
                  serviceId: textOr(
                    candidate.serviceId,
                    textOr(candidate.service_id),
                  ),
                }
              : {}),
          };
        },
      );
      return {
        nodeId: textOr(node.nodeId, textOr(node.node_id)),
        serviceId: textOr(node.serviceId, textOr(node.service_id)),
        kind: textOr(node.kind),
        ...(textOr(node.name) ? { name: textOr(node.name) } : {}),
        ...(textOr(node.resourceType, textOr(node.resource_type))
          ? {
              resourceType: textOr(
                node.resourceType,
                textOr(node.resource_type),
              ),
            }
          : {}),
        ...(textOr(node.versionRequirement, textOr(node.version_requirement))
          ? {
              versionRequirement: textOr(
                node.versionRequirement,
                textOr(node.version_requirement),
              ),
            }
          : {}),
        ...(typeof node.optional === "boolean"
          ? { optional: node.optional }
          : {}),
        ...(textOr(node.lifecycle)
          ? { lifecycle: textOr(node.lifecycle) }
          : {}),
        ...(typeof node.required === "boolean"
          ? { required: node.required }
          : {}),
        ...(Object.keys(objectOrEmpty(node.schema)).length
          ? { schema: objectOrEmpty(node.schema) }
          : {}),
        ...(Object.keys(provider).length
          ? {
              provider: {
                capability: textOr(provider.capability),
                versionRequirement: textOr(
                  provider.versionRequirement,
                  textOr(provider.version_requirement),
                ),
                policy: textOr(provider.policy),
                candidates: providerCandidates,
                ...(textOr(
                  provider.selectedProviderId,
                  textOr(provider.selected_provider_id),
                )
                  ? {
                      selectedProviderId: textOr(
                        provider.selectedProviderId,
                        textOr(provider.selected_provider_id),
                      ),
                    }
                  : {}),
              },
            }
          : {}),
        unresolvedInputs: arrayOrEmpty<unknown>(
          node.unresolvedInputs ?? node.unresolved_inputs,
        ).map((value) => {
          const declaration = objectOrEmpty(value);
          return {
            key: textOr(declaration.key),
            valueType: textOr(
              declaration.valueType,
              textOr(declaration.value_type),
            ),
            required: booleanOr(declaration.required),
            sensitive: booleanOr(declaration.sensitive),
            allowedValues: stringsOrEmpty(
              declaration.allowedValues ?? declaration.allowed_values,
            ),
          };
        }),
      };
    },
  );
  return {
    valid: booleanOr(row.valid),
    catalog_source_id: textOr(row.catalog_source_id),
    catalog_id: textOr(row.catalog_id),
    verified_key_ids: stringsOrEmpty(row.verified_key_ids),
    target_platform: {
      os: textOr(targetPlatform.os),
      arch: textOr(targetPlatform.arch),
    },
    plan: row.plan,
    metadata: arrayOrEmpty<Record<string, unknown>>(row.metadata),
    bindings,
    requirements,
    composition_plan: Object.keys(composition).length
      ? {
          schemaVersion: textOr(
            composition.schemaVersion,
            textOr(composition.schema_version),
          ),
          mode: textOr(composition.mode),
          rootServiceId: textOr(
            composition.rootServiceId,
            textOr(composition.root_service_id),
          ),
          planDigest: textOr(
            composition.planDigest,
            textOr(composition.plan_digest),
          ),
          releaseGraphDigest: textOr(
            composition.releaseGraphDigest,
            textOr(composition.release_graph_digest),
          ),
          nodes: compositionNodes,
          edges: arrayOrEmpty<Record<string, unknown>>(composition.edges),
        }
      : null,
    composition_inputs_valid: booleanOr(
      row.composition_inputs_valid,
      !textOr(row.composition_input_error),
    ),
    composition_input_error: textOr(row.composition_input_error),
    topology_confirmation_required: booleanOr(
      row.topology_confirmation_required,
    ),
    runtime: normalizeRuntimeValidation(row.runtime ?? row.runtime_facts),
    topology: Object.keys(topology).length
      ? {
          topology_id: textOr(topology.topology_id),
          revision_id: textOr(topology.revision_id),
        }
      : null,
    topology_diff:
      rawDiff && typeof rawDiff === "object" ? (rawDiff as TopologyDiff) : null,
    side_effects: {
      release_imports: numberOr(sideEffects.release_imports),
      operations: numberOr(sideEffects.operations),
      jobs: numberOr(sideEffects.jobs),
      runtime_calls: numberOr(sideEffects.runtime_calls),
    },
  };
}

export function normalizeStoreIndex(value: unknown): StoreIndexResponse {
  const row = (value && typeof value === "object" ? value : {}) as Record<
    string,
    unknown
  >;
  const modules = arrayOrEmpty<unknown>(row.items).map((value) => {
    const module =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)
        : {};
    return {
      id: textOr(module.module_id, textOr(module.id)),
      name: textOr(module.name, textOr(module.module_id, textOr(module.id))),
      description: textOr(module.description),
      kind: textOr(module.kind, "unknown"),
      tags: stringsOrEmpty(module.tags),
      repo: "",
      source_url: "",
      checksum: textOr(module.metadata_sha256, textOr(module.checksum)),
      version: textOr(module.version),
      channel: textOr(module.channel, "stable"),
      platforms: arrayOrEmpty<unknown>(module.platforms)
        .map((platform) => {
          const item =
            platform && typeof platform === "object" && !Array.isArray(platform)
              ? (platform as Record<string, unknown>)
              : {};
          return { os: textOr(item.os), arch: textOr(item.arch) };
        })
        .filter((platform) => platform.os && platform.arch),
      min_orchestrator_version: textOr(module.min_orchestrator_version),
      oci_image: textOr(module.oci_image),
      source_id: textOr(module.source_id),
      catalog_id: textOr(module.catalog_id),
    } satisfies StoreModule;
  });
  const installed: StoreIndexResponse["installed"] = {};
  const rawInstalled =
    row.installed && typeof row.installed === "object"
      ? (row.installed as Record<string, unknown>)
      : {};
  for (const [id, value] of Object.entries(rawInstalled)) {
    const item =
      value && typeof value === "object"
        ? (value as Record<string, unknown>)
        : {};
    installed[id] = {
      version: textOr(item.version),
      versions: stringsOrEmpty(item.versions),
      kind: textOr(item.kind, "unknown"),
      deployments: arrayOrEmpty<unknown>(item.deployments).map((value) => {
        const deployment =
          value && typeof value === "object"
            ? (value as Record<string, unknown>)
            : {};
        return {
          deployment_id: textOr(deployment.deployment_id),
          node_id: textOr(deployment.node_id),
          version: textOr(deployment.version),
          host_ip: textOr(deployment.host_ip),
          status: textOr(deployment.status, "unknown"),
        };
      }),
    };
  }
  return {
    index_url: textOr(row.index_url, "trusted-catalog-v2"),
    cached: booleanOr(row.cached),
    index: {
      schema_version: 2,
      name: "Trusted Catalog v2",
      description: "Signed and digest-pinned release catalog",
      updated_at: "",
      modules,
    },
    installed,
  };
}
