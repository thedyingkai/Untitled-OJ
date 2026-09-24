import { computed, getCurrentScope, onScopeDispose, ref } from "vue";
import type {
  InstallApiBindingSelection,
  NodeRow,
  StoreMigrationPolicy,
  StoreModule,
  StorePipelineOptions,
  StoreValidationResult,
  TopologyHeads,
} from "../../types";
import type { CompositionFormState } from "./composition-form";
import {
  compositionFormErrors,
  initializeCompositionState,
  serializeCompositionInputs,
} from "./composition-form";
import { topologyApi } from "../topology/api";
import { storeApi } from "./api";
import { sha256Fingerprint } from "./confirmation";
import type { ControlPlaneContext } from "../control-plane/context";
import type { ComputedRef } from "vue";

export function useReleaseInstall(
  store: ControlPlaneContext,
  readyNodes: ComputedRef<NodeRow[]>,
) {
  let validationGeneration = 0;
  let topologyGeneration = 0;
  if (getCurrentScope()) {
    onScopeDispose(() => {
      validationGeneration += 1;
      topologyGeneration += 1;
    });
  }
  /* ---------- 安装抽屉 ---------- */

  const installOpen = ref(false);

  const installTarget = ref<StoreModule | null>(null);

  const targetNodeId = ref("");

  const installStart = ref(true);

  const migrationPolicy = ref<StoreMigrationPolicy>("APPLY");

  const gatewayNodeId = ref("");

  const installConfigJson = ref("{}");

  const secretRefsJson = ref("{}");

  const installing = ref(false);

  const validating = ref(false);

  const validationResult = ref<StoreValidationResult | null>(null);

  const compositionInputs = ref<CompositionFormState>({});

  const bindingSelections = ref<Record<string, string>>({});

  const topologyHeads = ref<TopologyHeads[]>([]);

  const topologyId = ref("");

  const topologyRevisionId = ref("");

  const topologyLoading = ref(false);

  const validatedFingerprint = ref("");

  const validationConfirmationFingerprint = ref("");

  const installResult = ref<{ operationId: string | null; ok: boolean } | null>(
    null,
  );

  function openInstall(module: StoreModule) {
    validationGeneration += 1;
    validating.value = false;
    installTarget.value = module;
    installResult.value = null;
    validationResult.value = null;
    compositionInputs.value = {};
    bindingSelections.value = {};
    topologyHeads.value = [];
    topologyId.value = "";
    topologyRevisionId.value = "";
    installStart.value = true;
    migrationPolicy.value = "APPLY";
    gatewayNodeId.value = "";
    installConfigJson.value = "{}";
    secretRefsJson.value = "{}";
    validatedFingerprint.value = "";
    validationConfirmationFingerprint.value = "";
    targetNodeId.value = readyNodes.value[0]?.node_id ?? "";
    installOpen.value = true;
    void loadTopologyOptions();
  }

  const selectedTopologyHead = computed(() =>
    topologyHeads.value.find((heads) => heads.topology_id === topologyId.value),
  );

  const selectedRuntimeProfile = computed(
    () => validationResult.value?.runtime?.selected_contract ?? null,
  );

  const profilePermissionSummary = computed(() => {
    if (selectedRuntimeProfile.value?.id === "judge-sandbox-v1") {
      return [
        "privileged=true",
        "SYS_ADMIN / NET_ADMIN / SYS_CHROOT",
        "host cgroup namespace",
        "apparmor=unconfined",
        "/sys/fs/cgroup read-write",
      ];
    }
    if (selectedRuntimeProfile.value?.id) {
      return [
        "非 privileged",
        "不接受 Release 自定义 host path/capability/security option",
      ];
    }
    return [];
  });

  const healthGateSummary = computed(() =>
    selectedRuntimeProfile.value?.id === "judge-sandbox-v1"
      ? "Docker HEALTHY，最长 120 秒；缺少 HEALTHCHECK 直接拒绝"
      : "使用签名 Release 声明的 Docker 健康门禁",
  );

  function selectedBindings(): InstallApiBindingSelection[] {
    return Object.entries(bindingSelections.value)
      .filter(([, provider]) => provider.trim())
      .map(([name, provider_deployment_id]) => ({
        name,
        provider_deployment_id,
      }))
      .sort((left, right) => left.name.localeCompare(right.name));
  }

  function selectedTopology() {
    return topologyId.value && topologyRevisionId.value
      ? {
          topology_id: topologyId.value,
          topology_etag: `"${topologyRevisionId.value}"`,
        }
      : undefined;
  }

  function parseJsonObject(
    source: string,
    label: string,
  ): Record<string, unknown> {
    const value = JSON.parse(source) as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new Error(`${label} 必须是 JSON object`);
    }
    return value as Record<string, unknown>;
  }

  function selectedPipelineOptions(): StorePipelineOptions {
    const common = {
      start: installStart.value,
      migration_policy: migrationPolicy.value,
      ...(gatewayNodeId.value.trim()
        ? { gateway_node_id: gatewayNodeId.value.trim() }
        : {}),
      inputs: selectedCompositionInputs(),
    };
    if (validationResult.value?.composition_plan) return common;

    const config = parseJsonObject(installConfigJson.value, "Release config");
    const rawSecretRefs = parseJsonObject(
      secretRefsJson.value,
      "Secret references",
    );
    const secret_refs: Record<string, string> = {};
    for (const [name, reference] of Object.entries(rawSecretRefs)) {
      if (typeof reference !== "string" || !reference.trim()) {
        throw new Error(`Secret reference ${name} 必须是非空字符串引用`);
      }
      secret_refs[name] = reference.trim();
    }
    return { ...common, config, secret_refs };
  }

  function selectedCompositionInputs(): Record<
    string,
    Record<string, unknown>
  > {
    const plan = validationResult.value?.composition_plan;
    if (!plan) return {};
    return serializeCompositionInputs(plan, compositionInputs.value);
  }

  function initializeCompositionInputs(result: StoreValidationResult) {
    if (!result.composition_plan) return;
    compositionInputs.value = initializeCompositionState(
      result.composition_plan,
      compositionInputs.value,
    );
  }

  const compositionErrors = computed(() =>
    validationResult.value?.composition_plan
      ? compositionFormErrors(
          validationResult.value.composition_plan,
          compositionInputs.value,
        )
      : [],
  );

  const pipelineOptionsError = computed(() => {
    try {
      selectedPipelineOptions();
      return "";
    } catch (error) {
      return (error as Error).message;
    }
  });

  function currentValidationFingerprint(
    compositionPlan = validationResult.value?.composition_plan,
  ): string {
    return JSON.stringify({
      service: installTarget.value?.id ?? "",
      version: installTarget.value?.version ?? "",
      catalog_source_id: installTarget.value?.source_id ?? "",
      channel: installTarget.value?.channel ?? "",
      node: targetNodeId.value,
      topology: selectedTopology(),
      bindings: selectedBindings(),
      pipeline: {
        start: installStart.value,
        migration_policy: migrationPolicy.value,
        gateway_node_id: gatewayNodeId.value.trim(),
        ...(compositionPlan
          ? {
              composition_inputs: serializeCompositionInputs(
                compositionPlan,
                compositionInputs.value,
              ),
            }
          : {
              config: installConfigJson.value.trim(),
              secret_refs: secretRefsJson.value.trim(),
            }),
      },
    });
  }

  const unresolvedRequiredBindings = computed(() =>
    (validationResult.value?.requirements ?? []).filter(
      (requirement) =>
        !requirement.optional &&
        !bindingSelections.value[requirement.name]?.trim(),
    ),
  );

  const topologyRequired = computed(
    () => (validationResult.value?.requirements.length ?? 0) > 0,
  );

  const topologySatisfied = computed(
    () =>
      !topologyRequired.value ||
      (!!topologyId.value && !!topologyRevisionId.value),
  );

  const installReady = computed(
    () =>
      !validating.value &&
      !installing.value &&
      !!validationResult.value?.valid &&
      topologySatisfied.value &&
      ((validationResult.value?.requirements.length ?? 0) === 0 ||
        !!validationResult.value?.topology_diff) &&
      unresolvedRequiredBindings.value.length === 0 &&
      compositionErrors.value.length === 0 &&
      !!validationResult.value?.composition_inputs_valid &&
      !pipelineOptionsError.value &&
      validatedFingerprint.value === currentValidationFingerprint(),
  );

  async function loadTopologyOptions() {
    const generation = ++topologyGeneration;
    if (!store.supportsAction("topology.export")) {
      topologyHeads.value = [];
      topologyId.value = "";
      topologyRevisionId.value = "";
      topologyLoading.value = false;
      return;
    }
    topologyLoading.value = true;
    try {
      const heads = await topologyApi.topologyList();
      if (generation !== topologyGeneration) return;
      topologyHeads.value = heads;
      // Binding authority is a user decision. A new consumer often does not yet
      // exist in the applied Topology, so guessing "primary" would turn a valid
      // explicit binding plan into a misleading revision conflict.
      topologyId.value = "";
      topologyRevisionId.value = "";
    } catch (err) {
      if (generation !== topologyGeneration) return;
      topologyHeads.value = [];
      topologyId.value = "";
      topologyRevisionId.value = "";
      store.toast("err", `Topology 选择加载失败：${(err as Error).message}`);
    } finally {
      if (generation === topologyGeneration) topologyLoading.value = false;
    }
  }

  async function onTopologyChanged() {
    const heads = selectedTopologyHead.value;
    topologyRevisionId.value = heads?.applied_revision_id ?? "";
  }

  async function runValidate() {
    if (!store.ensureAction("release.validate")) return;
    const module = installTarget.value;
    if (!module || !targetNodeId.value) {
      store.toast("err", "必须选择受信任 Catalog Release 和 READY Node");
      return;
    }
    const generation = ++validationGeneration;
    try {
      // Snapshot the current plan-scoped values before clearing the previous
      // response for loading. Node IDs only exist in that previous plan.
      const submittedCompositionPlan = validationResult.value?.composition_plan;
      const submittedFingerprint = currentValidationFingerprint(
        submittedCompositionPlan,
      );
      const pipelineOptions = selectedPipelineOptions();
      validating.value = true;
      validationResult.value = null;
      validatedFingerprint.value = "";
      validationConfirmationFingerprint.value = "";
      const result = await storeApi.storeValidate({
        service_id: module.id,
        version: module.version,
        catalog_source_id: module.source_id,
        channel: module.channel,
        target_node_id: targetNodeId.value,
        ...pipelineOptions,
        bindings: selectedBindings(),
        ...(selectedTopology() ?? {}),
      });
      if (generation !== validationGeneration) return;
      // Compare against the submitted plan before applying server defaults.
      // A response must never certify inputs edited while it was in flight.
      if (
        submittedFingerprint !==
        currentValidationFingerprint(submittedCompositionPlan)
      ) {
        store.toast("info", "安装参数在校验期间已变化，请重新校验");
        return;
      }
      validationResult.value = result;
      initializeCompositionInputs(result);
      for (const requirement of result.requirements) {
        if (!bindingSelections.value[requirement.name]) {
          const resolved = result.bindings.find(
            (binding) => binding.requirement_name === requirement.name,
          )?.provider_deployment_id;
          // A recommendation may be displayed for an ambiguous requirement, but
          // only an explicit user choice may resolve it.
          const recommended = requirement.ambiguous
            ? ""
            : requirement.recommended_provider_deployment_id || resolved || "";
          if (recommended)
            bindingSelections.value[requirement.name] = recommended;
        }
      }
      const compositionPlanChanged =
        !!result.composition_plan &&
        (!submittedCompositionPlan ||
          submittedCompositionPlan.planDigest !==
            result.composition_plan.planDigest ||
          submittedCompositionPlan.releaseGraphDigest !==
            result.composition_plan.releaseGraphDigest);
      if (compositionPlanChanged) {
        // The first response is plan discovery. The UI has only now learned the
        // deterministic node IDs and signed schemas. The same rule applies when
        // a later validation returns a replacement plan: inputs submitted for an
        // older digest never authorize install against the new one.
        validatedFingerprint.value = "";
        validationConfirmationFingerprint.value = "";
        store.toast(
          "info",
          "CompositionPlan 已加载；请填写按服务输入并重新校验后再安装",
        );
      } else {
        const fingerprint = currentValidationFingerprint();
        const confirmation = await sha256Fingerprint(JSON.parse(fingerprint));
        if (
          generation !== validationGeneration ||
          fingerprint !== currentValidationFingerprint()
        )
          return;
        validatedFingerprint.value = fingerprint;
        validationConfirmationFingerprint.value = confirmation;
      }
      if (compositionPlanChanged) {
        // The discovery toast above is the actionable next step.
      } else if (result.requirements.length > 0 && !selectedTopology()) {
        store.toast(
          "info",
          "该 Release 是 API consumer；请选择 applied Topology 后重新校验",
        );
      } else if (
        result.valid &&
        unresolvedRequiredBindings.value.length === 0
      ) {
        store.toast(
          "ok",
          "Release、节点事实、Runtime Profile 和 API Binding 校验通过",
        );
      } else {
        store.toast("info", "请选择所有必需 API 的 Provider，然后重新校验");
      }
    } catch (err) {
      if (generation !== validationGeneration) return;
      store.toast("err", `Release 校验失败：${(err as Error).message}`);
    } finally {
      if (generation === validationGeneration) validating.value = false;
    }
  }

  async function runInstall() {
    if (!store.ensureAction("release.install")) return;
    const module = installTarget.value;
    if (!module || !targetNodeId.value) {
      store.toast("err", "必须选择一个 READY Node");
      return;
    }
    if (!installReady.value) {
      store.toast("err", "安装参数或 Binding 已变化，请重新校验后再安装");
      return;
    }
    installing.value = true;
    installResult.value = null;
    try {
      const result = await storeApi.storeInstall({
        service_id: module.id,
        version: module.version,
        catalog_source_id: module.source_id,
        channel: module.channel,
        target_node_id: targetNodeId.value,
        mode: "MANAGED",
        ...selectedPipelineOptions(),
        ...(validationResult.value?.composition_plan
          ? {
              plan_digest: validationResult.value.composition_plan.planDigest,
              release_graph_digest:
                validationResult.value.composition_plan.releaseGraphDigest,
            }
          : {}),
        bindings: selectedBindings(),
        ...(selectedTopology() ?? {}),
      });
      installResult.value = { operationId: result.operation_id, ok: true };
      store.toast("ok", `安装操作已提交：${result.operation_id}`);
      await Promise.all([store.refreshCore(true), store.refreshStore(true)]);
    } catch (err) {
      installResult.value = { operationId: null, ok: false };
      store.toast("err", `安装失败：${(err as Error).message}`);
    } finally {
      installing.value = false;
    }
  }

  return {
    installOpen,
    installTarget,
    targetNodeId,
    installStart,
    migrationPolicy,
    gatewayNodeId,
    installConfigJson,
    secretRefsJson,
    installing,
    validating,
    validationResult,
    compositionInputs,
    bindingSelections,
    topologyHeads,
    topologyId,
    topologyRevisionId,
    topologyLoading,
    validatedFingerprint,
    validationConfirmationFingerprint,
    installResult,
    openInstall,
    selectedRuntimeProfile,
    profilePermissionSummary,
    healthGateSummary,
    compositionErrors,
    pipelineOptionsError,
    currentValidationFingerprint,
    unresolvedRequiredBindings,
    topologyRequired,
    installReady,
    onTopologyChanged,
    runValidate,
    runInstall,
  };
}
