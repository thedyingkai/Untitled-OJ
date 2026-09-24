import { computed, ref } from "vue";
import type { NodeRow, StoreModule } from "../../types";
import { storeApi } from "./api";
import type { ControlPlaneContext } from "../control-plane/context";
import type { ComputedRef } from "vue";

export function moduleKey(module: StoreModule): string {
  return `${module.source_id}\u0000${module.id}\u0000${module.version}`;
}

export function useReleaseImport(
  store: ControlPlaneContext,
  modules: ComputedRef<StoreModule[]>,
  readyNodes: ComputedRef<NodeRow[]>,
) {
  /* ---------- 仅导入 ---------- */

  const importOpen = ref(false);

  const importTargetKey = ref("");

  const importTargetNodeId = ref("");

  const importing = ref(false);

  const importTarget = computed(
    () =>
      modules.value.find(
        (module) => moduleKey(module) === importTargetKey.value,
      ) ?? null,
  );

  function openImport(module?: StoreModule) {
    const target = module ?? modules.value[0];
    importTargetKey.value = target ? moduleKey(target) : "";
    importTargetNodeId.value = readyNodes.value[0]?.node_id ?? "";
    importOpen.value = true;
  }

  async function runImport() {
    if (!store.ensureAction("release.import")) return;
    const module = importTarget.value;
    if (!module || !importTargetNodeId.value) {
      store.toast("err", "必须从受信任 Catalog 选择 Release 和目标平台 Node");
      return;
    }
    importing.value = true;
    try {
      await storeApi.storeImport({
        service_id: module.id,
        version: module.version,
        catalog_source_id: module.source_id,
        channel: module.channel,
        target_node_id: importTargetNodeId.value,
      });
      store.toast("ok", "Release 已导入；没有创建 Deployment 或运行时任务");
      importOpen.value = false;
      await store.refreshStore(true);
    } catch (err) {
      store.toast("err", `导入失败：${(err as Error).message}`);
    } finally {
      importing.value = false;
    }
  }

  return {
    importOpen,
    importTargetKey,
    importTargetNodeId,
    importing,
    importTarget,
    openImport,
    runImport,
  };
}
