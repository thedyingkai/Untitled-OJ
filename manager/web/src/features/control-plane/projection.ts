import type {
  DeploymentRow,
  EndpointRow,
  LinkRow,
  NodeRow,
  ServiceRow,
  TopologyDetail,
} from "../../types";

export interface ControlPlaneProjection {
  services: ServiceRow[];
  deployments: DeploymentRow[];
  endpoints: EndpointRow[];
  links: LinkRow[];
}

/** Join current server facts into display rows without reading or mutating UI state. */
export function projectControlPlane(
  nodes: NodeRow[],
  deployments: DeploymentRow[],
  topology: TopologyDetail | null,
): ControlPlaneProjection {
  const endpointStatuses = new Map(
    (topology?.status?.endpoints ?? []).map((status) => [
      status.endpoint,
      status,
    ]),
  );
  const linkStatuses = new Map(
    (topology?.status?.links ?? []).map((status) => [
      `${status.source_endpoint}\0${status.target_endpoint}`,
      status,
    ]),
  );
  const endpointRows: EndpointRow[] =
    topology?.draft.spec.endpoints.map((endpoint) => {
      const status = endpointStatuses.get(endpoint.endpoint);
      return {
        endpoint: endpoint.endpoint,
        service_id: endpoint.service_id,
        protocol: endpoint.protocol,
        expose:
          endpoint.endpoint === topology.draft.spec.root_endpoint
            ? topology.draft.spec.authority.exposure_policy
            : "",
        source: "topology-draft",
        health_path: endpoint.health_path,
        health: status?.health ?? "UNKNOWN",
        reachable: status?.reachable ?? false,
        display_name: endpoint.display_name,
        note: endpoint.note,
        config: endpoint.config,
      };
    }) ?? [];
  const linkRows: LinkRow[] =
    topology?.draft.spec.links.map((link) => ({
      from: link.source_endpoint,
      to: link.target_endpoint,
      protocol: link.protocol,
      auth_mode: link.auth_mode,
      scope: link.scope,
      enabled: link.enabled ? "enabled" : "disabled",
      source: "topology-draft",
      health:
        linkStatuses.get(`${link.source_endpoint}\0${link.target_endpoint}`)
          ?.health ?? "UNKNOWN",
    })) ?? [];
  const nodeById = new Map(nodes.map((node) => [node.node_id, node]));
  const enrichedDeployments = deployments.map((deployment) => {
    const matchingEndpoints = endpointRows.filter(
      (endpoint) =>
        deployment.endpoint === endpoint.endpoint ||
        deployment.endpoints.includes(endpoint.endpoint) ||
        endpoint.config?.deployment_id === deployment.deployment_id,
    );
    const primaryEndpoint = matchingEndpoints[0];
    return {
      ...deployment,
      host_ip: nodeById.get(deployment.node_id)?.host_ip || deployment.node_id,
      endpoint: primaryEndpoint?.endpoint ?? deployment.endpoint,
      protocol: primaryEndpoint?.protocol ?? deployment.protocol,
      health_path: primaryEndpoint?.health_path ?? deployment.health_path,
      endpoint_health: primaryEndpoint?.health ?? deployment.endpoint_health,
      reachable: primaryEndpoint?.reachable ?? deployment.reachable,
      endpoint_count: matchingEndpoints.length || deployment.endpoint_count,
      endpoints: matchingEndpoints.length
        ? matchingEndpoints.map((endpoint) => endpoint.endpoint)
        : deployment.endpoints,
    };
  });
  const serviceRows: ServiceRow[] = enrichedDeployments
    .filter((deployment) => !!deployment.deployment_id)
    .map((deployment) => ({
      id: deployment.deployment_id,
      deployment_id: deployment.deployment_id,
      node_id: deployment.node_id,
      service_id: deployment.service_id,
      name: deployment.service_id,
      version: deployment.version,
      kind: deployment.kind,
      endpoint: deployment.endpoint,
      runtime: deployment.runtime,
      ui: "",
      health: deployment.endpoint_health,
    }));
  return {
    services: serviceRows,
    deployments: enrichedDeployments,
    endpoints: endpointRows,
    links: linkRows,
  };
}
