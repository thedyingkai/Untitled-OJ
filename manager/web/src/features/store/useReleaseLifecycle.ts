import { ref } from "vue";
import type {
  DeploymentRow,
  InstallApiBindingSelection,
  StoreModule,
} from "../../types";
import { deploymentsApi } from "../deployments/api";
import { topologyApi } from "../topology/api";
import { sha256Fingerprint } from "./confirmation";
import { storeApi } from "./api";
import { deploymentMutationMessage } from "../deployments/errors";
import type { ControlPlaneContext } from "../control-plane/context";

export function useReleaseLifecycle(store: ControlPlaneContext) {
  /* ---------- 卸载 ---------- */

  const uninstalling = ref("");

  const replacing = ref("");

  const deletingRelease = ref("");

  async function replaceRelease(
    deployment: DeploymentRow,
    action: "upgrade" | "rollback",
  ) {
    const capability =
      action === "upgrade" ? "release.upgrade" : "release.rollback";
    if (!store.ensureAction(capability)) return;
    const label =
      action === "upgrade" ? "升级到最新兼容版本" : "回滚到最近一次已证明版本";
    replacing.value = `${action}:${deployment.deployment_id}`;
    try {
      const bindingRoles = await deploymentsApi.deploymentBindings(
        deployment.deployment_id,
      );
      const affectedTopologyIds = Array.from(
        new Set(
          [...bindingRoles.items, ...bindingRoles.provider_items]
            .filter(
              (binding) =>
                binding.desired_state === "ACTIVE" &&
                binding.state === "ACTIVE",
            )
            .map((binding) => binding.topology_id)
            .filter(Boolean),
        ),
      ).sort();
      const replacementPayload: {
        deployment_id: string;
        bindings?: InstallApiBindingSelection[];
        topology_id?: string;
        topology_etag?: string;
        topologies?: Array<{ topology_id: string; topology_etag: string }>;
      } = {
        deployment_id: deployment.deployment_id,
        bindings: bindingRoles.items
          .filter(
            (binding) =>
              binding.desired_state === "ACTIVE" &&
              binding.provider_deployment_id,
          )
          .map((binding) => ({
            name: binding.requirement_name,
            provider_deployment_id: binding.provider_deployment_id,
          }))
          .sort((left, right) => left.name.localeCompare(right.name)),
      };
      if (affectedTopologyIds.length > 0) {
        const heads = await topologyApi.topologyList();
        const cas = affectedTopologyIds.map((topology_id) => {
          const applied = heads.find(
            (item) => item.topology_id === topology_id,
          )?.applied_revision_id;
          if (!applied) {
            throw new Error(
              `Topology ${topology_id} 没有 applied head，无法安全替换`,
            );
          }
          return { topology_id, topology_etag: `"${applied}"` };
        });
        if (cas.length === 1) {
          replacementPayload.topology_id = cas[0].topology_id;
          replacementPayload.topology_etag = cas[0].topology_etag;
        } else {
          replacementPayload.topologies = cas;
        }
      }
      const fingerprint = await sha256Fingerprint(replacementPayload);
      const bindingSummary = replacementPayload.bindings?.length
        ? replacementPayload.bindings
            .map(
              (binding) => `${binding.name}=${binding.provider_deployment_id}`,
            )
            .join(", ")
        : "无 consumer Binding";
      const topologySummary = replacementPayload.topologies
        ? replacementPayload.topologies
            .map(
              (topology) => `${topology.topology_id}@${topology.topology_etag}`,
            )
            .join(", ")
        : replacementPayload.topology_id
          ? `${replacementPayload.topology_id}@${replacementPayload.topology_etag}`
          : "无受影响 Topology";
      if (
        !window.confirm(
          `${label}：${deployment.deployment_id}\nBindings: ${bindingSummary}\nTopology CAS: ${topologySummary}\n确认指纹 sha256:${fingerprint}`,
        )
      ) {
        return;
      }
      const result =
        action === "upgrade"
          ? await storeApi.storeUpgrade(replacementPayload)
          : await storeApi.storeRollback(replacementPayload);
      store.toast("ok", `${label}操作已提交：${result.operation_id}`);
      await Promise.all([store.refreshCore(true), store.refreshStore(true)]);
    } catch (err) {
      store.toast("err", `${label}失败：${(err as Error).message}`);
    } finally {
      replacing.value = "";
    }
  }

  async function uninstall(deployment: DeploymentRow) {
    if (!store.ensureAction("deployment.uninstall")) return;
    if (
      !window.confirm(
        `卸载 ${deployment.deployment_id}？Release 元数据会保留。`,
      )
    )
      return;
    uninstalling.value = deployment.deployment_id;
    try {
      const result = await deploymentsApi.deploymentAction(
        deployment.deployment_id,
        "uninstall",
      );
      store.toast("ok", `卸载操作已提交：${result.operation_id}`);
      await Promise.all([store.refreshCore(true), store.refreshStore(true)]);
    } catch (err) {
      store.toast(
        "err",
        `卸载失败：${await deploymentMutationMessage(err, deployment.deployment_id)}`,
      );
    } finally {
      uninstalling.value = "";
    }
  }

  async function deleteImportedRelease(module: StoreModule) {
    if (!store.ensureAction("release.delete")) return;
    if (
      !window.confirm(
        `删除未被 Deployment 使用的 Release ${module.id}@${module.version}？`,
      )
    ) {
      return;
    }
    deletingRelease.value = `${module.id}@${module.version}`;
    try {
      await storeApi.deleteRelease(module.id, module.version);
      store.toast("ok", `已删除 Release ${module.id}@${module.version}`);
      await store.refreshStore(true);
    } catch (err) {
      store.toast("err", `删除 Release 失败：${(err as Error).message}`);
    } finally {
      deletingRelease.value = "";
    }
  }

  return {
    uninstalling,
    replacing,
    deletingRelease,
    replaceRelease,
    uninstall,
    deleteImportedRelease,
  };
}
