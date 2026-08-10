<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import PageHeader from "../components/PageHeader.vue";
import Modal from "../components/Modal.vue";
import StatusChip from "../components/StatusChip.vue";
import FlowCanvas from "../components/FlowCanvas.vue";
import type { FlowEdge, FlowNode } from "../flow-types";
import EndpointNode from "../components/EndpointNode.vue";
import { api } from "../api";
import { parseEndpointId } from "../endpoint";
import { useOrchestrator } from "../store";
import type {
  ApiBinding,
  EndpointRow,
  LinkRow,
  ServiceRow,
  TopologyApiBindingSpec,
  TopologyDiff,
  TopologyRevision,
  TopologySpec,
} from "../types";

const store = useOrchestrator();
const canvas = ref<InstanceType<typeof FlowCanvas> | null>(null);
const newTopologyId = ref("");
const topologyId = computed(
  () => store.activeTopologyId || newTopologyId.value.trim(),
);
const history = ref<TopologyRevision[]>([]);
const rollbackRevisionId = ref("");
const diffResult = ref<TopologyDiff | null>(null);
const validationHash = ref("");
const topologyOperationId = ref("");

const currentRevision = computed(() => store.topology?.draft ?? null);
const topologyStatus = computed(() => store.topology?.status ?? null);
const editCapability = computed(() =>
  store.topology ? "topology.revision" : "topology.draft",
);

async function refreshHistory() {
  if (!store.topology || !store.supportsAction("topology.export")) {
    history.value = [];
    return;
  }
  history.value = await api.topologyRevisions(topologyId.value);
  if (!rollbackRevisionId.value) {
    rollbackRevisionId.value =
      store.topology.heads.applied_revision_id ?? history.value[0]?.revision_id ?? "";
  }
}

watch(
  () => store.topology?.draft.revision_id,
  () => void refreshHistory(),
  { immediate: true },
);

function cloneSpec(): TopologySpec | null {
  const spec = store.topology?.draft.spec;
  return spec ? JSON.parse(JSON.stringify(spec)) as TopologySpec : null;
}

async function saveSpec(spec: TopologySpec, message: string) {
  if (!store.ensureAction(editCapability.value)) return;
  if (store.topology) {
    await api.topologyCreateRevision(
      topologyId.value,
      spec,
      store.topology.draft.revision_id,
      { changeMessage: message },
    );
  } else {
    await api.topologyCreate(spec, { changeMessage: message });
  }
  await store.refreshCore(true);
  await refreshHistory();
}

/* ---------- 画布数据 ---------- */

const selectedNode = ref<string | null>(null);
const selectedEdge = ref<string | null>(null);

async function changeTopology(event: Event) {
  const selected = (event.target as HTMLSelectElement).value;
  selectedNode.value = null;
  selectedEdge.value = null;
  rollbackRevisionId.value = "";
  diffResult.value = null;
  validationHash.value = "";
  topologyOperationId.value = "";
  await store.selectTopology(selected);
}

/** 画布调色板只暴露已有部署投影对应的服务，避免把“已登记 manifest”伪装成可部署实例。 */
const paletteServices = computed<ServiceRow[]>(() => {
  return store.services.filter(
    (service) =>
      store.deployments.find(
        (deployment) => deployment.deployment_id === service.deployment_id,
      )?.status.toUpperCase() !== "FAILED",
  );
});

/** 拖动中的临时坐标（避免每帧写 pinia + PUT） */
const livePositions = ref<Record<string, { x: number; y: number }>>({});

function autoPosition(index: number) {
  const columns = 4;
  return {
    x: 60 + (index % columns) * 250,
    y: 60 + Math.floor(index / columns) * 160,
  };
}

const nodes = computed<FlowNode[]>(() =>
  store.endpoints.map((endpoint: EndpointRow, index) => {
    const parsed = parseEndpointId(endpoint.endpoint) ?? {
      host: "",
      port: "",
      service: endpoint.service_id,
    };
    const deployment = store.deployments.find(
      (candidate) =>
        candidate.endpoint === endpoint.endpoint ||
        candidate.endpoints?.includes(endpoint.endpoint) ||
        endpoint.config?.deployment_id === candidate.deployment_id,
    );
    const service = deployment
      ? store.serviceByDeploymentId(deployment.deployment_id)
      : undefined;
    const position =
      livePositions.value[endpoint.endpoint] ??
      store.layout.positions?.[endpoint.endpoint] ??
      autoPosition(index);
    return {
      id: endpoint.endpoint,
      x: position.x,
      y: position.y,
      data: {
        serviceId: endpoint.service_id,
        kind: service?.kind ?? "backend-api",
        protocol: endpoint.protocol,
        // 健康必须来自部署/Endpoint 的 observed 投影，不从 Service manifest 猜测。
        health: deployment?.endpoint_health || "unknown",
        host: parsed.host,
        port: parsed.port,
      },
    };
  }),
);

/**
 * core 的 LinkViewRow.enabled 由 link_enabled_label(bool) 生成，取值只有
 * "enabled" / "disabled"（services/orchestrator/core/src/view.rs）。
 * 字段缺失/为空时按“启用”处理，兼容旧 daemon 返回。
 */
function isLinkEnabled(link: LinkRow): boolean {
  const value = (link.enabled ?? "").trim().toLowerCase();
  return value !== "disabled" && value !== "false";
}

const edges = computed<FlowEdge[]>(() =>
  store.links.map((link) => ({
    id: `${link.from}|${link.to}`,
    source: link.from,
    target: link.to,
    label: link.protocol || undefined,
    disabled: !isLinkEnabled(link),
  })),
);

function onNodeMove(id: string, position: { x: number; y: number }) {
  livePositions.value[id] = position;
  store.setNodePosition(id, position);
}

function onNodeClick(id: string) {
  selectedNode.value = id;
  selectedEdge.value = null;
}

function onEdgeClick(id: string) {
  selectedEdge.value = id;
  selectedNode.value = null;
}

function clearSelection() {
  selectedNode.value = null;
  selectedEdge.value = null;
}

/* ---------- 连线 → 创建 Link ---------- */

const linkProtocols = ["http", "https", "tcp", "postgres", "redis"] as const;
const pendingConnection = ref<{
  source: string;
  target: string;
  protocol: (typeof linkProtocols)[number];
} | null>(null);
const creatingLink = ref(false);

function defaultLinkProtocol(target: string): (typeof linkProtocols)[number] {
  const protocol = store.endpoints
    .find((endpoint) => endpoint.endpoint === target)
    ?.protocol.trim()
    .toLowerCase();
  return linkProtocols.find((candidate) => candidate === protocol) ?? "http";
}

function onConnect(source: string, target: string) {
  if (!store.ensureAction(editCapability.value)) return;
  const exists = store.links.some(
    (link) => link.from === source && link.to === target,
  );
  if (exists) {
    store.toast("info", "该 Link 已存在");
    return;
  }
  pendingConnection.value = {
    source,
    target,
    protocol: defaultLinkProtocol(target),
  };
}

async function confirmCreateLink() {
  if (!store.ensureAction(editCapability.value)) return;
  const connection = pendingConnection.value;
  if (!connection) return;
  const spec = cloneSpec();
  if (!spec) {
    store.toast("err", "请先创建拓扑中的第一个 Endpoint");
    return;
  }
  creatingLink.value = true;
  try {
    spec.links.push({
      source_endpoint: connection.source,
      target_endpoint: connection.target,
      protocol: connection.protocol,
      auth_mode: "none",
      scope: "",
      enabled: true,
      config_ref: "",
      secret_ref: "",
      policy: {},
    });
    await saveSpec(spec, `add link ${connection.source} -> ${connection.target}`);
    store.toast("ok", "已创建包含该 Link 的新 draft revision");
    pendingConnection.value = null;
  } catch (err) {
    store.toast("err", `创建 Link 失败：${(err as Error).message}`);
  } finally {
    creatingLink.value = false;
  }
}

/* ---------- 拖拽服务 → 创建 Endpoint ---------- */

const paletteOpen = ref(true);
const endpointModal = ref(false);
const creatingEndpoint = ref(false);
const endpointForm = ref({
  deployment_id: "",
  service_id: "",
  host_ip: "127.0.0.1",
  port: "8080",
  protocol: "http",
  health_path: "/health",
  x: 0,
  y: 0,
});

function defaultPortFor(service: ServiceRow | undefined): string {
  const match = service?.endpoint?.match(/(\d{2,5})/);
  return match ? match[1] : "8080";
}

function onPaletteDragStart(event: DragEvent, service: ServiceRow) {
  event.dataTransfer?.setData("application/ojos-service", service.id);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = "copy";
}

function onDropAt(position: { x: number; y: number }, event: DragEvent) {
  const deploymentId = event.dataTransfer?.getData("application/ojos-service");
  if (!deploymentId) return;
  const service = store.serviceByDeploymentId(deploymentId);
  if (!service) return;
  const parsed = parseEndpointId(service.endpoint);
  endpointForm.value = {
    deployment_id: deploymentId,
    service_id: service.service_id,
    host_ip: parsed?.host || store.deployments.find(
      (deployment) => deployment.deployment_id === deploymentId,
    )?.host_ip || "127.0.0.1",
    port: defaultPortFor(service),
    protocol: "http",
    health_path: "/health",
    x: Math.round(position.x),
    y: Math.round(position.y),
  };
  endpointModal.value = true;
}

async function confirmCreateEndpoint() {
  if (!store.ensureAction(editCapability.value)) return;
  const form = endpointForm.value;
  const endpointId = `${form.host_ip.trim()}:${form.port.trim()}:${form.service_id}`;
  creatingEndpoint.value = true;
  try {
    const endpoint = {
      endpoint: endpointId,
      service_id: form.service_id,
      protocol: form.protocol,
      health_path: form.health_path.trim(),
      display_name: form.service_id,
      note: "",
      config: { deployment_id: form.deployment_id },
    };
    const spec = cloneSpec() ?? {
      api_version: "v1" as const,
      topology_id: topologyId.value,
      root_endpoint: endpointId,
      authority: {
        root_endpoint: endpointId,
        exposure_policy: "internal",
      },
      endpoints: [],
      links: [],
    };
    spec.endpoints.push(endpoint);
    await saveSpec(spec, `add endpoint ${endpointId}`);
    store.setNodePosition(endpointId, { x: form.x, y: form.y });
    store.toast("ok", `已创建 Endpoint ${endpointId} 的新 draft revision`);
    endpointModal.value = false;
  } catch (err) {
    store.toast("err", `创建端点失败：${(err as Error).message}`);
  } finally {
    creatingEndpoint.value = false;
  }
}

/* ---------- 检查 / 删除 ---------- */

const busy = ref(false);

const selectedEndpointRow = computed(() =>
  store.endpoints.find((endpoint) => endpoint.endpoint === selectedNode.value),
);

const selectedEdgeParts = computed(() => {
  if (!selectedEdge.value) return null;
  const [source, target] = selectedEdge.value.split("|");
  return source && target ? { source, target } : null;
});

const selectedLinkRow = computed(() =>
  store.links.find(
    (link) =>
      link.from === selectedEdgeParts.value?.source &&
      link.to === selectedEdgeParts.value?.target,
  ),
);

const selectedSpecLink = computed(() =>
  store.topology?.draft.spec.links.find(
    (link) =>
      link.source_endpoint === selectedEdgeParts.value?.source &&
      link.target_endpoint === selectedEdgeParts.value?.target,
  ),
);

const selectedApiBindings = ref<ApiBinding[]>([]);
const bindingEvidenceLoading = ref(false);
const bindingEvidenceError = ref("");

function sourceDeploymentId(): string {
  const source = selectedEdgeParts.value?.source;
  if (!source) return "";
  const endpoint = store.topology?.draft.spec.endpoints.find(
    (item) => item.endpoint === source,
  );
  const configuredDeployment = endpoint?.config?.deployment_id;
  if (typeof configuredDeployment === "string" && configuredDeployment) {
    return configuredDeployment;
  }
  return store.deployments.find(
    (deployment) =>
      deployment.endpoint === source || deployment.endpoints.includes(source),
  )?.deployment_id ?? "";
}

async function refreshSelectedBindingEvidence() {
  selectedApiBindings.value = [];
  bindingEvidenceError.value = "";
  const deploymentId = sourceDeploymentId();
  if (!deploymentId || !store.supportsAction("deployment.get")) return;
  bindingEvidenceLoading.value = true;
  try {
    const result = await api.deploymentBindings(deploymentId);
    const requirementNames = new Set(
      (selectedSpecLink.value?.api_bindings ?? []).map(
        (binding) => binding.requirement,
      ),
    );
    selectedApiBindings.value = result.items.filter(
      (binding) =>
        (binding.link_source_endpoint === selectedEdgeParts.value?.source &&
          binding.link_target_endpoint === selectedEdgeParts.value?.target) ||
        requirementNames.has(binding.requirement_name),
    );
  } catch (err) {
    bindingEvidenceError.value = (err as Error).message;
  } finally {
    bindingEvidenceLoading.value = false;
  }
}

watch(
  () => `${selectedEdge.value ?? ""}\0${store.topology?.status?.updated_at ?? ""}`,
  () => void refreshSelectedBindingEvidence(),
);

const bindingModal = ref(false);
const bindingSaving = ref(false);
const originalBindingRequirement = ref("");
const bindingForm = ref<TopologyApiBindingSpec>({
  requirement: "",
  api_id: "",
  version: ">=1.0.0 <2.0.0",
  optional: false,
  provider_deployment_id: "",
  selection: "explicit",
});

const providerDeployments = computed(() =>
  [...store.deployments]
    .filter(
      (deployment) =>
        deployment.deployment_id &&
        !["FAILED", "STOPPED"].includes(deployment.observed_state.toUpperCase()),
    )
    .sort((left, right) =>
      left.deployment_id.localeCompare(right.deployment_id),
    ),
);

function editApiBinding(binding?: TopologyApiBindingSpec) {
  const value = binding ?? {
    requirement: "",
    api_id: "",
    version: ">=1.0.0 <2.0.0",
    optional: false,
    provider_deployment_id: "",
    selection: "explicit",
  };
  originalBindingRequirement.value = binding?.requirement ?? "";
  bindingForm.value = JSON.parse(JSON.stringify(value)) as TopologyApiBindingSpec;
  bindingModal.value = true;
}

async function saveApiBinding() {
  const link = selectedSpecLink.value;
  const requirement = bindingForm.value.requirement.trim();
  const apiId = bindingForm.value.api_id.trim();
  const provider = bindingForm.value.provider_deployment_id.trim();
  if (!link || !requirement || !apiId || !provider) {
    store.toast("err", "Binding 需要 requirement、API ID 和明确的 Provider Deployment");
    return;
  }
  const spec = cloneSpec();
  const draftLink = spec?.links.find(
    (candidate) =>
      candidate.source_endpoint === link.source_endpoint &&
      candidate.target_endpoint === link.target_endpoint,
  );
  if (!spec || !draftLink) return;
  const duplicate = (draftLink.api_bindings ?? []).some(
    (binding) =>
      binding.requirement === requirement &&
      binding.requirement !== originalBindingRequirement.value,
  );
  if (duplicate) {
    store.toast("err", `Requirement ${requirement} 已经绑定`);
    return;
  }
  const next: TopologyApiBindingSpec = {
    requirement,
    api_id: apiId,
    version: bindingForm.value.version.trim(),
    optional: bindingForm.value.optional,
    provider_deployment_id: provider,
    selection: "explicit",
  };
  const bindings = [...(draftLink.api_bindings ?? [])];
  const index = bindings.findIndex(
    (binding) => binding.requirement === originalBindingRequirement.value,
  );
  if (index >= 0) bindings[index] = next;
  else bindings.push(next);
  bindings.sort((left, right) =>
    left.requirement.localeCompare(right.requirement),
  );
  draftLink.api_bindings = bindings;
  bindingSaving.value = true;
  try {
    await saveSpec(
      spec,
      `${index >= 0 ? "rebind" : "bind"} ${requirement} on ${link.source_endpoint} -> ${link.target_endpoint}`,
    );
    bindingModal.value = false;
    store.toast("ok", `已创建包含 ${requirement} Binding 的新 draft revision`);
  } catch (err) {
    store.toast("err", `保存 ApiBinding 失败：${(err as Error).message}`);
  } finally {
    bindingSaving.value = false;
  }
}

async function removeApiBinding(requirement: string) {
  const link = selectedSpecLink.value;
  const spec = cloneSpec();
  const draftLink = spec?.links.find(
    (candidate) =>
      candidate.source_endpoint === link?.source_endpoint &&
      candidate.target_endpoint === link?.target_endpoint,
  );
  if (!spec || !draftLink) return;
  if (!window.confirm(`从 draft Link 中解除 ${requirement}？apply 后旧凭据会立即失效。`)) {
    return;
  }
  draftLink.api_bindings = (draftLink.api_bindings ?? []).filter(
    (binding) => binding.requirement !== requirement,
  );
  bindingSaving.value = true;
  try {
    await saveSpec(spec, `unbind ${requirement} from ${draftLink.source_endpoint}`);
    store.toast("ok", `已创建解除 ${requirement} 的新 draft revision`);
  } catch (err) {
    store.toast("err", `解除 ApiBinding 失败：${(err as Error).message}`);
  } finally {
    bindingSaving.value = false;
  }
}

async function deleteSelectedNode() {
  if (!store.ensureAction(editCapability.value)) return;
  if (!selectedNode.value) return;
  const spec = cloneSpec();
  if (!spec) return;
  if (spec.endpoints.length === 1) {
    store.toast("err", "TopologySpec 必须保留 root Endpoint，不能删除最后一个 Endpoint");
    return;
  }
  if (!window.confirm(`删除端点 ${selectedNode.value} 及其相关 Link？`)) return;
  busy.value = true;
  try {
    const removed = selectedNode.value;
    spec.endpoints = spec.endpoints.filter((item) => item.endpoint !== removed);
    spec.links = spec.links.filter(
      (link) =>
        link.source_endpoint !== removed && link.target_endpoint !== removed,
    );
    if (spec.root_endpoint === removed) {
      spec.root_endpoint = spec.endpoints[0]!.endpoint;
      spec.authority.root_endpoint = spec.root_endpoint;
    }
    await saveSpec(spec, `remove endpoint ${removed}`);
    store.toast("ok", "已创建移除 Endpoint 的新 draft revision");
    clearSelection();
  } catch (err) {
    store.toast("err", `删除失败：${(err as Error).message}`);
  } finally {
    busy.value = false;
  }
}

async function deleteSelectedEdge() {
  if (!store.ensureAction(editCapability.value)) return;
  const parts = selectedEdgeParts.value;
  if (!parts) return;
  const spec = cloneSpec();
  if (!spec) return;
  if (!window.confirm(`删除 Link ${parts.source} → ${parts.target}？`)) return;
  busy.value = true;
  try {
    spec.links = spec.links.filter(
      (link) =>
        link.source_endpoint !== parts.source ||
        link.target_endpoint !== parts.target,
    );
    await saveSpec(spec, `remove link ${parts.source} -> ${parts.target}`);
    store.toast("ok", "已创建移除 Link 的新 draft revision");
    clearSelection();
  } catch (err) {
    store.toast("err", `删除失败：${(err as Error).message}`);
  } finally {
    busy.value = false;
  }
}

/** 当前选中 Link 是否启用；未在 links 里找到时按启用显示。 */
const selectedLinkEnabled = computed(() =>
  selectedLinkRow.value ? isLinkEnabled(selectedLinkRow.value) : true,
);

/** 停用/启用当前 Link：daemon 的 enable/disable 路由会补 confirm=true。 */
async function toggleSelectedEdge() {
  const parts = selectedEdgeParts.value;
  if (!parts) return;
  const enabled = selectedLinkEnabled.value;
  if (!store.ensureAction(editCapability.value)) return;
  const spec = cloneSpec();
  if (!spec) return;
  busy.value = true;
  try {
    const link = spec.links.find(
      (item) =>
        item.source_endpoint === parts.source &&
        item.target_endpoint === parts.target,
    );
    if (!link) return;
    link.enabled = !enabled;
    await saveSpec(
      spec,
      `${enabled ? "disable" : "enable"} link ${parts.source} -> ${parts.target}`,
    );
    store.toast("ok", enabled ? "已在新 revision 中停用 Link" : "已在新 revision 中启用 Link");
  } catch (err) {
    store.toast(
      "err",
      `${enabled ? "停用" : "启用"} Link 失败：${(err as Error).message}`,
    );
  } finally {
    busy.value = false;
  }
}

async function validateDraft() {
  const spec = cloneSpec();
  if (!spec || !store.ensureAction("topology.validate")) return;
  busy.value = true;
  try {
    const result = await api.topologyValidate(topologyId.value, spec);
    validationHash.value = result.content_sha256;
    store.toast("ok", `Spec 校验通过：${result.content_sha256}`);
  } catch (err) {
    store.toast("err", `校验失败：${(err as Error).message}`);
  } finally {
    busy.value = false;
  }
}

async function diffDraft() {
  if (!store.topology || !store.ensureAction("topology.diff")) return;
  busy.value = true;
  try {
    diffResult.value = await api.topologyDiff(topologyId.value, {
      from_revision_id: store.topology.heads.applied_revision_id ?? undefined,
      to_revision_id: store.topology.draft.revision_id,
    });
  } catch (err) {
    store.toast("err", `Diff 失败：${(err as Error).message}`);
  } finally {
    busy.value = false;
  }
}

async function applyDraft() {
  if (!store.topology || !store.ensureAction("topology.apply")) return;
  busy.value = true;
  try {
    const result = await api.topologyApply(
      topologyId.value,
      store.topology.draft.revision_id,
    );
    topologyOperationId.value = result.operation_id;
    store.toast("ok", `Apply Operation 已提交：${result.operation_id}`);
    await store.refreshCore(true);
  } catch (err) {
    store.toast("err", `Apply 失败：${(err as Error).message}`);
  } finally {
    busy.value = false;
  }
}

async function rollbackTopology() {
  if (
    !store.topology ||
    !rollbackRevisionId.value ||
    !store.ensureAction("topology.rollback")
  ) return;
  busy.value = true;
  try {
    const result = await api.topologyRollback(
      topologyId.value,
      store.topology.draft.revision_id,
      rollbackRevisionId.value,
    );
    topologyOperationId.value = result.operation_id;
    store.toast("ok", `Rollback Operation 已提交：${result.operation_id}`);
    await store.refreshCore(true);
    await refreshHistory();
  } catch (err) {
    store.toast("err", `Rollback 失败：${(err as Error).message}`);
  } finally {
    busy.value = false;
  }
}

/* ---------- 布局 ---------- */

function resetLayout() {
  livePositions.value = {};
  store.layout.positions = {};
  store.endpoints.forEach((endpoint, index) => {
    store.setNodePosition(endpoint.endpoint, autoPosition(index));
  });
  if (resetFitTimer) clearTimeout(resetFitTimer);
  resetFitTimer = setTimeout(() => {
    resetFitTimer = null;
    canvas.value?.fitView();
  }, 60);
}

let resetFitTimer: ReturnType<typeof setTimeout> | null = null;

onBeforeUnmount(() => {
  if (resetFitTimer) clearTimeout(resetFitTimer);
  resetFitTimer = null;
});
</script>

<template>
  <PageHeader
    title="拓扑"
    subtitle="拖拽服务到画布创建端点，从右侧端口拖出连线建立 Link"
  >
    <select
      v-if="store.topologyHeads.length"
      class="select topology-picker"
      aria-label="Current topology"
      :value="store.activeTopologyId"
      @change="changeTopology"
    >
      <option
        v-for="heads in store.topologyHeads"
        :key="heads.topology_id"
        :value="heads.topology_id"
      >
        {{ heads.topology_id }} · {{ heads.draft_revision_id }}
      </option>
    </select>
    <input
      v-else
      v-model="newTopologyId"
      class="input topology-picker"
      aria-label="New topology ID"
      placeholder="new topology id"
    />
    <span
      v-if="store.layoutStatus === 'saving' || store.layoutStatus === 'error'"
      class="chip"
      data-testid="layout-persistence-status"
      :class="store.layoutStatus === 'error' ? 'err' : ''"
      :title="store.layoutError"
    >
      {{ store.layoutStatus === "saving" ? "布局保存中…" : "布局未保存" }}
    </span>
    <button class="btn sm" @click="paletteOpen = !paletteOpen">
      {{ paletteOpen ? "隐藏服务面板" : "显示服务面板" }}
    </button>
    <span v-if="currentRevision" class="chip mono">
      draft r{{ currentRevision.revision_number }} · {{ currentRevision.revision_id }}
    </span>
    <span v-if="topologyStatus" class="chip" :class="topologyStatus.state === 'IN_SYNC' ? 'ok' : 'warn'">
      {{ topologyStatus.state }} · drift {{ topologyStatus.drift.length }}
    </span>
    <button
      class="btn sm"
      :disabled="!!busy || !currentRevision || !store.supportsAction('topology.validate')"
      @click="validateDraft"
    >校验</button>
    <button
      class="btn sm"
      :disabled="!!busy || !currentRevision || !store.supportsAction('topology.diff')"
      @click="diffDraft"
    >Diff</button>
    <button
      class="btn primary sm"
      :disabled="!!busy || !currentRevision || !store.supportsAction('topology.apply')"
      @click="applyDraft"
    >Apply</button>
    <button class="btn sm" @click="resetLayout">自动布局</button>
    <button class="btn sm" @click="store.refreshCore(true)">刷新</button>
  </PageHeader>

  <div v-if="currentRevision" class="topology-statebar">
    <span v-if="validationHash" class="mono">validated {{ validationHash }}</span>
    <span v-if="diffResult">diff {{ diffResult.changes?.length ?? 0 }} changes</span>
    <span v-if="topologyOperationId" class="mono">operation {{ topologyOperationId }}</span>
    <label v-if="history.length" class="rollback-control">
      回滚到
      <select class="select" v-model="rollbackRevisionId">
        <option v-for="revision in history" :key="revision.revision_id" :value="revision.revision_id">
          r{{ revision.revision_number }} · {{ revision.message || revision.revision_id }}
        </option>
      </select>
      <button
        class="btn sm"
        :disabled="!!busy || !rollbackRevisionId || !store.supportsAction('topology.rollback')"
        @click="rollbackTopology"
      >Rollback</button>
    </label>
  </div>

  <div class="canvas-wrap">
    <!-- 服务拖拽面板 -->
    <aside v-if="paletteOpen" class="palette fade-in">
      <div class="palette-title">已安装服务</div>
      <p class="palette-hint">拖拽到画布创建运行端点</p>
      <div class="palette-list">
        <div
          v-for="service in paletteServices"
          :key="service.id"
          class="palette-item"
          :draggable="store.supportsAction(editCapability)"
          :class="{ unavailable: !store.supportsAction(editCapability) }"
          :data-service-id="service.id"
          @dragstart="onPaletteDragStart($event, service)"
        >
          <div class="palette-item-name">{{ service.name || service.id }}</div>
          <div class="palette-item-meta">
            <span class="chip">{{ service.kind }}</span>
            <span v-if="service.version" class="mono muted">v{{ service.version }}</span>
            <span class="mono muted">{{ service.deployment_id }} @ {{ service.node_id }}</span>
          </div>
        </div>
        <div v-if="!paletteServices.length" class="empty">
          <span class="icon">▤</span>
          <span>尚无已安装服务<br />先去商店安装模块</span>
        </div>
      </div>
    </aside>

    <!-- 画布 -->
    <div class="canvas" data-testid="topology-canvas">
      <FlowCanvas
        ref="canvas"
        :nodes="nodes"
        :edges="edges"
        :selected-node="selectedNode"
        :selected-edge="selectedEdge"
        @node-move="onNodeMove"
        @node-click="onNodeClick"
        @edge-click="onEdgeClick"
        @pane-click="clearSelection"
        @connect="onConnect"
        @drop-at="onDropAt"
      >
        <template #node="{ node, selected }">
          <EndpointNode :data="node.data as any" :selected="selected" />
        </template>
      </FlowCanvas>

      <div v-if="!store.endpoints.length" class="canvas-empty">
        <div class="canvas-empty-card">
          <div class="icon">◈</div>
          <h3>画布为空</h3>
          <p class="muted">
            从左侧把已安装的服务拖进来创建端点；<br />
            没有服务时，先到 <b>商店</b> 安装模块。
          </p>
        </div>
      </div>
    </div>

    <!-- 详情侧栏 -->
    <aside v-if="selectedNode || selectedEdgeParts" class="inspector fade-in">
      <template v-if="selectedNode && selectedEndpointRow">
        <div class="inspector-head">
          <h3>端点详情</h3>
          <button class="btn ghost sm" @click="clearSelection">✕</button>
        </div>
        <div class="kv">
          <span>端点</span
          ><span class="mono">{{ selectedEndpointRow.endpoint }}</span>
        </div>
        <div class="kv">
          <span>服务</span><span>{{ selectedEndpointRow.service_id }}</span>
        </div>
        <div class="kv">
          <span>协议</span><span>{{ selectedEndpointRow.protocol }}</span>
        </div>
        <div class="kv">
          <span>暴露</span><span>{{ selectedEndpointRow.expose }}</span>
        </div>
        <div class="kv">
          <span>来源</span><span>{{ selectedEndpointRow.source }}</span>
        </div>
        <div class="kv">
          <span>实时健康</span><span><StatusChip :status="selectedEndpointRow.health" /></span>
        </div>
        <div class="inspector-actions">
          <button
            class="btn danger sm"
            :disabled="busy || !store.supportsAction(editCapability)"
            @click="deleteSelectedNode"
          >
            删除端点
          </button>
        </div>
      </template>

      <template v-else-if="selectedEdgeParts">
        <div class="inspector-head">
          <h3>Link 详情</h3>
          <button class="btn ghost sm" @click="clearSelection">✕</button>
        </div>
        <div class="kv">
          <span>源</span><span class="mono">{{ selectedEdgeParts.source }}</span>
        </div>
        <div class="kv">
          <span>目标</span
          ><span class="mono">{{ selectedEdgeParts.target }}</span>
        </div>
        <div class="kv" v-if="selectedLinkRow">
          <span>协议</span><span>{{ selectedLinkRow.protocol || "—" }}</span>
        </div>
        <div class="kv" v-if="selectedLinkRow">
          <span>鉴权</span><span>{{ selectedLinkRow.auth_mode || "—" }}</span>
        </div>
        <div class="kv" v-if="selectedLinkRow">
          <span>范围</span><span>{{ selectedLinkRow.scope || "—" }}</span>
        </div>
        <div class="kv" v-if="selectedLinkRow">
          <span>状态</span>
          <span>
            <span class="chip" :class="selectedLinkEnabled ? 'ok' : 'err'">
              {{ selectedLinkEnabled ? "已启用" : "已停用" }}
            </span>
          </span>
        </div>
        <div class="kv" v-if="selectedLinkRow">
          <span>实时健康</span><span><StatusChip :status="selectedLinkRow.health" /></span>
        </div>
        <section
          class="binding-inspector"
        >
          <div class="binding-inspector-head">
            <h4>ApiBinding</h4>
            <button
              class="btn sm"
              :disabled="bindingSaving || !store.supportsAction(editCapability)"
              @click="editApiBinding()"
            >新增 requirement</button>
          </div>
          <article
            v-for="specBinding in selectedSpecLink?.api_bindings ?? []"
            :key="specBinding.requirement"
            class="binding-inspector-card"
          >
            <div class="binding-inspector-head">
              <strong>{{ specBinding.requirement }}</strong>
              <span class="mono">{{ specBinding.api_id }}</span>
            </div>
            <div class="binding-inspector-line">
              <span>期望 Provider</span>
              <span class="mono">{{ specBinding.provider_deployment_id || specBinding.selection }}</span>
            </div>
            <template
              v-for="observed in selectedApiBindings.filter((binding) => binding.requirement_name === specBinding.requirement)"
              :key="observed.binding_id"
            >
              <div class="binding-inspector-line">
                <span>实际 Provider</span>
                <span class="mono">{{ observed.provider_deployment_id || "UNBOUND" }}</span>
              </div>
              <div class="binding-inspector-line">
                <span>状态</span>
                <span><StatusChip :status="observed.health" /> {{ observed.state }}</span>
              </div>
              <div class="binding-inspector-line">
                <span>代次</span>
                <span class="mono">ctx {{ observed.context_generation || "?" }} / cred {{ observed.credential_generation || "?" }}</span>
              </div>
              <div v-if="observed.drift.length" class="binding-inspector-drift">
                Drift：{{ observed.drift.join("；") }}
              </div>
            </template>
            <div class="binding-inspector-actions">
              <button class="btn sm" @click="editApiBinding(specBinding)">编辑 / Rebind</button>
              <button
                class="btn danger sm"
                :disabled="bindingSaving"
                @click="removeApiBinding(specBinding.requirement)"
              >解除</button>
            </div>
          </article>
          <p v-if="bindingEvidenceLoading" class="hint">正在读取实际 Binding…</p>
          <p v-else-if="bindingEvidenceError" class="binding-inspector-drift">
            Binding 证据加载失败：{{ bindingEvidenceError }}
          </p>
          <p
            v-else-if="!selectedApiBindings.length"
            class="hint"
          >
            当前 Link 只有 draft 期望，尚无已应用的实际 Binding。
          </p>
        </section>
        <div class="inspector-actions">
          <button
            class="btn sm"
            :class="{ primary: !selectedLinkEnabled }"
            :disabled="
              busy ||
              !store.supportsAction(editCapability)
            "
            @click="toggleSelectedEdge"
          >
            {{ selectedLinkEnabled ? "停用" : "启用" }}
          </button>
        </div>
        <div class="inspector-actions">
          <button
            class="btn danger sm"
            :disabled="busy || !store.supportsAction(editCapability)"
            @click="deleteSelectedEdge"
          >
            删除 Link
          </button>
        </div>
      </template>
    </aside>
  </div>

  <!-- 创建 Link 确认 -->
  <Modal
    :open="!!pendingConnection"
    title="创建 Link"
    @close="pendingConnection = null"
  >
    <p class="muted" style="margin-top: 0">
      将授权以下通信关系（source → target）：
    </p>
    <div class="link-preview mono">
      <div>{{ pendingConnection?.source }}</div>
      <div class="arrow">↓</div>
      <div>{{ pendingConnection?.target }}</div>
    </div>
    <div class="field">
      <label for="link-protocol">协议</label>
      <select
        id="link-protocol"
        v-if="pendingConnection"
        v-model="pendingConnection.protocol"
        class="input"
      >
        <option v-for="protocol in linkProtocols" :key="protocol" :value="protocol">
          {{ protocol }}
        </option>
      </select>
    </div>
    <template #footer>
      <button class="btn" @click="pendingConnection = null">取消</button>
      <button
        class="btn primary"
        :disabled="creatingLink || !store.supportsAction(editCapability)"
        @click="confirmCreateLink"
      >
        {{ creatingLink ? "创建中…" : "创建 Link" }}
      </button>
    </template>
  </Modal>

  <Modal
    :open="bindingModal"
    :title="originalBindingRequirement ? `编辑 ${originalBindingRequirement}` : '新增 ApiBinding requirement'"
    width="620px"
    @close="bindingModal = false"
  >
    <div class="field">
      <label>Requirement 名称</label>
      <input class="input mono" v-model="bindingForm.requirement" aria-label="Binding requirement" />
    </div>
    <div class="field">
      <label>API ID</label>
      <input class="input mono" v-model="bindingForm.api_id" aria-label="Binding API ID" placeholder="storage.object.get" />
    </div>
    <div class="field">
      <label>SemVer 范围</label>
      <input class="input mono" v-model="bindingForm.version" aria-label="Binding API version" />
    </div>
    <div class="field">
      <label>Provider Deployment（精确身份）</label>
      <select
        class="select mono"
        v-model="bindingForm.provider_deployment_id"
        aria-label="Binding provider deployment"
      >
        <option value="">请选择 Deployment</option>
        <option
          v-for="deployment in providerDeployments"
          :key="deployment.deployment_id"
          :value="deployment.deployment_id"
        >
          {{ deployment.deployment_id }} · {{ deployment.service_id }} · {{ deployment.node_id }} · {{ deployment.observed_state }}
        </option>
      </select>
    </div>
    <label class="check-line">
      <input type="checkbox" v-model="bindingForm.optional" /> 可选 requirement
    </label>
    <p class="hint">
      保存只创建新的 immutable draft revision；Apply 成功后才切换实际 Binding 与凭据代次。
    </p>
    <template #footer>
      <button class="btn" @click="bindingModal = false">取消</button>
      <button class="btn primary" :disabled="bindingSaving" @click="saveApiBinding">
        {{ bindingSaving ? "保存中…" : "保存到 draft" }}
      </button>
    </template>
  </Modal>

  <!-- 创建端点 -->
  <Modal :open="endpointModal" title="创建端点" @close="endpointModal = false">
    <div class="field">
      <label>服务</label>
      <input class="input" :value="endpointForm.service_id" disabled />
    </div>
    <div class="field">
      <label>主机 IP</label>
      <input
        class="input"
        v-model="endpointForm.host_ip"
        placeholder="127.0.0.1"
      />
    </div>
    <div class="field">
      <label>端口</label>
      <input class="input" v-model="endpointForm.port" placeholder="8080" />
    </div>
    <div class="field">
      <label>协议</label>
      <select class="select" v-model="endpointForm.protocol">
        <option>http</option>
        <option>https</option>
        <option>tcp</option>
        <option>postgres</option>
        <option>redis</option>
      </select>
    </div>
    <div class="field">
      <label>健康检查路径</label>
      <input
        class="input"
        v-model="endpointForm.health_path"
        placeholder="/health"
      />
    </div>
    <p class="hint mono">
      端点 ID：{{ endpointForm.host_ip }}:{{ endpointForm.port }}:{{
        endpointForm.service_id
      }}
    </p>
    <template #footer>
      <button class="btn" @click="endpointModal = false">取消</button>
      <button
        class="btn primary"
        :disabled="creatingEndpoint || !store.supportsAction(editCapability)"
        @click="confirmCreateEndpoint"
      >
        {{ creatingEndpoint ? "创建中…" : "创建端点" }}
      </button>
    </template>
  </Modal>
</template>

<style scoped>
.topology-statebar {
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 38px;
  padding: 6px 14px;
  border-bottom: 1px solid var(--border);
  background: var(--bg-soft);
  color: var(--muted);
  font-size: 11px;
}
.topology-picker {
  width: min(300px, 34vw);
}
.rollback-control {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
}
.rollback-control .select {
  width: 260px;
  padding: 4px 8px;
}
.canvas-wrap {
  flex: 1;
  display: flex;
  min-height: 0;
  position: relative;
}

.palette {
  width: 218px;
  flex-shrink: 0;
  border-right: 1px solid var(--border);
  background: var(--bg-soft);
  display: flex;
  flex-direction: column;
  padding: 14px;
  overflow-y: auto;
}
.palette-item.unavailable {
  cursor: not-allowed;
  opacity: 0.55;
}
.palette-title {
  font-size: 12px;
  font-weight: 600;
  color: var(--text-strong);
}
.palette-hint {
  font-size: 11px;
  color: var(--faint);
  margin: 3px 0 12px;
}
.palette-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.palette-item {
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--panel);
  cursor: grab;
  transition: all 0.14s ease;
}
.palette-item:hover {
  border-color: var(--accent);
  transform: translateY(-1px);
}
.palette-item:active {
  cursor: grabbing;
}
.palette-item-name {
  font-size: 12.5px;
  font-weight: 600;
  margin-bottom: 5px;
}
.palette-item-meta {
  display: flex;
  align-items: center;
  gap: 7px;
}

.canvas {
  flex: 1;
  min-width: 0;
  position: relative;
  background: var(--bg);
}

.canvas-empty {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}
.canvas-empty-card {
  text-align: center;
  padding: 34px 44px;
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-lg);
  background: rgba(13, 18, 32, 0.75);
}
.canvas-empty-card .icon {
  font-size: 26px;
  color: var(--accent-2);
  margin-bottom: 8px;
}
.canvas-empty-card h3 {
  font-size: 14px;
  margin-bottom: 6px;
}
.canvas-empty-card p {
  font-size: 12.5px;
  margin: 0;
}

.inspector {
  width: 262px;
  flex-shrink: 0;
  border-left: 1px solid var(--border);
  background: var(--bg-soft);
  padding: 16px;
  overflow-y: auto;
}
.inspector-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;
}
.inspector-head h3 {
  font-size: 13.5px;
}
.kv {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  padding: 7px 0;
  border-bottom: 1px solid rgba(148, 163, 184, 0.07);
  font-size: 12.5px;
}
.kv > span:first-child {
  color: var(--faint);
  flex-shrink: 0;
}
.kv > span:last-child {
  text-align: right;
  word-break: break-all;
}
.inspector-actions {
  display: flex;
  gap: 8px;
  margin-top: 14px;
}
.inspector-actions + .inspector-actions {
  margin-top: 8px;
}

.binding-inspector {
  margin-top: 14px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}
.binding-inspector h4 {
  margin: 0 0 8px;
  font-size: 12px;
}
.binding-inspector-card {
  padding: 8px;
  margin-top: 7px;
  border: 1px solid var(--border);
  border-radius: 8px;
}
.binding-inspector-head,
.binding-inspector-line {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.binding-inspector-head {
  flex-wrap: wrap;
  margin-bottom: 6px;
  font-size: 11.5px;
}
.binding-inspector-head h4 {
  margin: 0;
}
.binding-inspector-head .mono {
  color: var(--muted);
  overflow-wrap: anywhere;
}
.binding-inspector-line {
  padding: 3px 0;
  font-size: 10.5px;
}
.binding-inspector-line > span:first-child {
  color: var(--faint);
}
.binding-inspector-line > span:last-child {
  text-align: right;
  overflow-wrap: anywhere;
}
.binding-inspector-drift {
  margin: 6px 0 0;
  color: var(--warn);
  font-size: 10.5px;
}
.binding-inspector-actions {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
  margin-top: 8px;
}
.check-line {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--muted);
}

.link-preview {
  background: var(--bg-soft);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 12px 14px;
  font-size: 12px;
  text-align: center;
  word-break: break-all;
}
.link-preview .arrow {
  color: var(--accent-2);
  margin: 4px 0;
}
</style>
