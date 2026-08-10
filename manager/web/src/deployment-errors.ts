import { api, ApiError } from "./api";

export async function deploymentMutationMessage(
  error: unknown,
  deploymentId: string,
): Promise<string> {
  if (
    !(error instanceof ApiError) ||
    error.status !== 409 ||
    error.code !== "DEPLOYMENT_ACTIVE_BINDINGS"
  ) {
    return (error as Error)?.message ?? String(error);
  }
  try {
    const evidence = await api.deploymentBindings(deploymentId);
    const links = [...evidence.items, ...evidence.provider_items]
      .filter(
        (binding) =>
          binding.desired_state === "ACTIVE" && binding.state !== "INACTIVE",
      )
      .map(
        (binding) =>
          `${binding.topology_id}: ${binding.link_source_endpoint} → ${binding.link_target_endpoint} (${binding.requirement_name})`,
      )
      .filter((value, index, values) => values.indexOf(value) === index)
      .sort();
    const scope = links.length ? ` 受影响 Link：${links.join("；")}。` : "";
    return `Deployment 仍被已应用的 ApiBinding 使用，不能卸载。请先在对应 Topology draft 中解除 Link/requirement 并 Apply，确认 Binding 失效后再卸载。${scope}`;
  } catch {
    return `Deployment 仍被已应用的 ApiBinding 使用，不能卸载。请先在对应 Topology draft 中解除 Link/requirement 并 Apply 后重试。服务端详情：${error.message}`;
  }
}
