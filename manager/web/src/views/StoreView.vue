<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import PageHeader from "../components/PageHeader.vue";
import Modal from "../components/Modal.vue";
import OperationLogs from "../components/OperationLogs.vue";
import { activeNodeConfigFields, compositionServices, nodesForService, providerCandidate, secretNodeState } from "../composition-form";
import { useOrchestrator } from "../store";
import type { DeploymentRow, StoreModule } from "../types";
import { useReleaseInstall } from "../features/store/useReleaseInstall";
import { useReleaseImport, moduleKey } from "../features/store/useReleaseImport";
import { useReleaseLifecycle } from "../features/store/useReleaseLifecycle";
import { useCatalogManager } from "../features/catalog/useCatalogManager";

const store = useOrchestrator();

onMounted(async () => {
  if (!store.storeIndex) {
    // App polling may still be loading the dynamic capability matrix. Coalesce
    // with it before deciding whether catalog.search is currently published.
    await store.refreshCore();
    await store.refreshStore();
  }
});

const packageSearch = ref("");

const modules = computed<StoreModule[]>(() => {
  const items = store.storeIndex?.index?.modules ?? [];
  const query = packageSearch.value.trim().toLowerCase();
  if (!query) return items;
  return items.filter((module) =>
    [module.id, module.name, module.description, module.kind, ...module.tags]
      .join(" ")
      .toLowerCase()
      .includes(query),
  );
});

const installedCount = computed(() => store.deployments.length);

const readyNodes = computed(() =>
  store.nodes.filter((node) => node.status.toUpperCase() === "READY"),
);

function deploymentsFor(serviceId: string): DeploymentRow[] {
  return store.deployments.filter(
    (deployment) => deployment.service_id === serviceId,
  );
}

const {
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
} = useReleaseInstall(store, readyNodes);

const {
  importOpen,
  importTargetKey,
  importTargetNodeId,
  importing,
  importTarget,
  openImport,
  runImport,
} = useReleaseImport(store, modules, readyNodes);

const {
  uninstalling,
  replacing,
  deletingRelease,
  replaceRelease,
  uninstall,
  deleteImportedRelease,
} = useReleaseLifecycle(store);

const {
  catalogManagerOpen,
  catalogs,
  catalogLoading,
  catalogSaving,
  catalogRemoving,
  catalogForm,
  loadCatalogs,
  openCatalogManager,
  registerCatalog,
  removeCatalog,
} = useCatalogManager(store);

const kindLabels: Record<string, string> = {
  gateway: "网关",
  "backend-api": "后端 API",
  "backend-worker": "工作进程",
  database: "数据库",
  cache: "缓存",
  storage: "存储",
  frontend: "前端",
  external: "外部",
  agent: "代理",
};
</script>

<template>
  <PageHeader
    title="Store"
    subtitle="从受信任 Catalog v2 选择精确版本与 OCI digest"
  >
    <input
      v-model="packageSearch"
      class="input"
      data-action="catalog.search"
      :disabled="!store.supportsAction('catalog.search')"
      placeholder="搜索 Release、类型或标签"
      style="width: 220px"
    />
    <button
      class="btn sm"
      data-action="catalog.list"
      :disabled="!store.supportsAction('catalog.list')"
      @click="openCatalogManager"
    >
      管理 Catalog
    </button>
    <button
      class="btn sm"
      :disabled="!store.supportsAction('release.import')"
      @click="openImport()"
    >
      仅导入 Release
    </button>
    <button
      class="btn sm"
      :disabled="!store.supportsAction('catalog.search')"
      @click="store.refreshStore(true)"
    >
      刷新索引
    </button>
  </PageHeader>

  <div class="store-body">
    <div v-if="store.storeLoadStatus === 'loading'" class="card load-state">
      正在加载商店目录与运行状态…
    </div>
    <div
      v-else-if="store.storeLoadStatus === 'error'"
      class="card load-state error-state"
      role="alert"
    >
      <span>{{ store.storeError }}</span>
      <button class="btn sm" @click="store.refreshStore(true)">重试</button>
    </div>
    <div class="status-bar">
      <span class="chip accent">Catalog v2 · 签名验证</span>
      <span class="chip">{{ modules.length }} 个可安装版本</span>
      <span class="chip" :class="readyNodes.length ? 'ok' : 'warn'">
        {{ readyNodes.length }} 个 READY Node
      </span>
      <span class="chip">{{ installedCount }} 个 Deployment</span>
    </div>

    <!-- 模块卡片 -->
    <div class="grid" v-if="modules.length">
      <div
        v-for="module in modules"
        :key="moduleKey(module)"
        class="card module-card fade-in"
        :data-testid="`store-package-${module.id}`"
      >
        <div class="module-head">
          <div>
            <div class="module-name">{{ module.name }}</div>
            <div class="module-id mono">{{ module.id }}</div>
          </div>
          <span v-if="deploymentsFor(module.id).length" class="chip ok">
            已部署 {{ deploymentsFor(module.id).length }} 个
          </span>
        </div>
        <p class="module-desc">{{ module.description }}</p>
        <div class="module-tags">
          <span class="chip">{{ kindLabels[module.kind] ?? module.kind }}</span>
          <span v-for="tag in module.tags" :key="tag" class="chip">{{
            tag
          }}</span>
          <span class="chip mono">v{{ module.version }}</span>
          <span class="chip">{{ module.channel }}</span>
        </div>
        <div class="module-actions">
          <button
            class="btn primary sm"
            :disabled="!store.supportsAction('release.install')"
            @click="openInstall(module)"
          >
            {{ deploymentsFor(module.id).length ? "安装另一实例" : "安装" }}
          </button>
          <button
            class="btn sm"
            :disabled="!store.supportsAction('release.import')"
            @click="openImport(module)"
          >
            仅导入
          </button>
          <button
            class="btn danger sm"
            :disabled="
              !!deletingRelease || !store.supportsAction('release.delete')
            "
            @click="deleteImportedRelease(module)"
          >
            {{
              deletingRelease === `${module.id}@${module.version}`
                ? "删除中…"
                : "删除 Release"
            }}
          </button>
          <button
            v-for="deployment in deploymentsFor(module.id)"
            :key="deployment.deployment_id"
            class="btn sm"
            :disabled="!!replacing || !store.supportsAction('release.upgrade')"
            @click="replaceRelease(deployment, 'upgrade')"
          >
            {{
              replacing === `upgrade:${deployment.deployment_id}`
                ? "提交中…"
                : `升级 ${deployment.node_id}`
            }}
          </button>
          <button
            v-for="deployment in deploymentsFor(module.id)"
            :key="`rollback:${deployment.deployment_id}`"
            class="btn sm"
            :disabled="!!replacing || !store.supportsAction('release.rollback')"
            @click="replaceRelease(deployment, 'rollback')"
          >
            {{
              replacing === `rollback:${deployment.deployment_id}`
                ? "提交中…"
                : `回滚 ${deployment.node_id}`
            }}
          </button>
          <button
            v-for="deployment in deploymentsFor(module.id)"
            :key="`uninstall:${deployment.deployment_id}`"
            class="btn danger sm"
            :disabled="
              !!uninstalling || !store.supportsAction('deployment.uninstall')
            "
            @click="uninstall(deployment)"
          >
            {{
              uninstalling === deployment.deployment_id
                ? "提交中…"
                : `卸载 ${deployment.node_id}`
            }}
          </button>
          <span class="module-source mono muted">{{ module.oci_image }}</span>
        </div>
      </div>
    </div>

    <div v-else class="empty">
      <span class="icon">▤</span>
      <span>
        没有可用的受信任 Catalog v2 package。<br />
        未发布 <code class="mono">release.install</code> 时安装入口会保持禁用。
      </span>
    </div>
  </div>

  <!-- 安装抽屉 -->
  <Modal
    :open="installOpen"
    :title="installTarget ? `安装 ${installTarget.name}` : '手动安装模块'"
    width="820px"
    @close="installOpen = false"
  >
    <div v-if="installTarget" class="card package-summary">
      <div>
        <strong>{{ installTarget.id }}@{{ installTarget.version }}</strong>
      </div>
      <div class="mono muted">{{ installTarget.oci_image }}</div>
      <div class="mono muted">metadata {{ installTarget.checksum }}</div>
      <div class="muted">
        Managed 安装会默认启动，并在健康门禁通过后才提升投影。
      </div>
    </div>
    <div class="field">
      <label>目标 Node ID</label>
      <select class="select" v-model="targetNodeId">
        <option value="" disabled>选择 READY Node</option>
        <option
          v-for="node in readyNodes"
          :key="node.node_id"
          :value="node.node_id"
        >
          {{ node.node_id }} · {{ node.host_ip || "loopback" }}
        </option>
      </select>
      <span class="hint">
        Node 必须明确选择；Web 不再按 IP 猜测目标节点。
      </span>
    </div>

    <div class="field">
      <label>
        Topology / Revision（Binding 权威来源）
        <span v-if="topologyRequired" class="chip warn">必需</span>
        <span v-else class="chip">纯 Provider 可不选</span>
      </label>
      <div class="topology-selection">
        <select
          class="select"
          v-model="topologyId"
          aria-label="Install topology"
          :disabled="topologyLoading"
          @change="onTopologyChanged"
        >
          <option value="">选择已应用的 Topology revision</option>
          <option
            v-for="heads in topologyHeads"
            :key="heads.topology_id"
            :value="heads.topology_id"
            :disabled="!heads.applied_revision_id"
          >
            {{ heads.topology_id }} · applied
            {{ heads.applied_revision_id || "无" }}
          </option>
        </select>
        <input
          class="input mono"
          :value="topologyRevisionId"
          readonly
          aria-label="Topology revision ETag"
          placeholder="applied revision / ETag"
        />
      </div>
      <span class="hint">
        含 required API 的 consumer 必须显式选择；安装请求携带 topology_id 与强
        ETag， 不会按服务名静默绑定。没有 required API 的纯 Provider
        可先安装，再供后续 Topology 选择。 安装预览只显示服务端针对本次候选
        Deployment 与 Binding 计算的 prospective diff。
      </span>
    </div>

    <details class="contract-section pipeline-options">
      <summary>Release pipeline 高级选项</summary>
      <div class="field">
        <label class="check">
          <input
            v-model="installStart"
            type="checkbox"
            aria-label="Start after install"
          />
          安装完成后启动并执行健康门禁
        </label>
      </div>
      <div class="field">
        <label>Migration policy</label>
        <select
          v-model="migrationPolicy"
          class="select"
          aria-label="Migration policy"
        >
          <option value="APPLY">APPLY</option>
          <option value="DRY_RUN">DRY_RUN</option>
        </select>
      </div>
      <div class="field">
        <label>Gateway Node ID（可选）</label>
        <input
          v-model="gatewayNodeId"
          class="input mono"
          aria-label="Gateway Node ID"
          placeholder="gateway-node-a"
        />
      </div>
      <div v-if="!validationResult?.composition_plan" class="field">
        <label>Release config JSON</label>
        <textarea
          v-model="installConfigJson"
          class="input mono"
          aria-label="Release config JSON"
          rows="5"
          spellcheck="false"
        />
      </div>
      <div v-if="!validationResult?.composition_plan" class="field">
        <label>Secret references JSON</label>
        <textarea
          v-model="secretRefsJson"
          class="input mono"
          aria-label="Secret references JSON"
          rows="4"
          spellcheck="false"
        />
        <span class="hint">
          这里只填写 secret 引用，不填写明文。校验与安装会提交完全相同的
          pipeline 参数。
        </span>
      </div>
      <p v-if="validationResult?.composition_plan" class="hint">
        已切换到下方按服务分组的 Composition 动态输入；旧 root config/secret
        alias 不再参与请求。
      </p>
      <p v-if="pipelineOptionsError" class="binding-warning">
        {{ pipelineOptionsError }}
      </p>
    </details>

    <div v-if="installResult" class="install-result">
      <div class="chip" :class="installResult.ok ? 'ok' : 'err'">
        {{ installResult.ok ? "动作已提交" : "安装失败" }}
      </div>
      <template v-if="installResult.operationId">
        <p class="muted" style="margin: 10px 0 6px">
          操作 <span class="mono">{{ installResult.operationId }}</span> 日志：
        </p>
        <OperationLogs :operation-id="installResult.operationId" live />
      </template>
    </div>

    <div v-if="validationResult" class="install-result">
      <div class="chip" :class="validationResult.valid ? 'ok' : 'err'">
        {{ validationResult.valid ? "Release 校验通过" : "Release 校验失败" }}
      </div>
      <p class="muted" style="margin: 10px 0 0">
        Catalog <span class="mono">{{ validationResult.catalog_id }}</span> ·
        {{ validationResult.target_platform.os }}/{{
          validationResult.target_platform.arch
        }}
        · key
        <span class="mono">{{
          validationResult.verified_key_ids.join(", ")
        }}</span>
      </p>
      <p v-if="validationConfirmationFingerprint" class="hint mono digest-wrap">
        本次候选 / Binding / Topology 确认指纹：sha256:{{
          validationConfirmationFingerprint
        }}
      </p>

      <section
        v-if="validationResult.composition_plan"
        class="contract-section"
      >
        <h4>CompositionPlanV1</h4>
        <p class="hint mono digest-wrap">
          {{ validationResult.composition_plan.mode || "production" }} · root
          {{ validationResult.composition_plan.rootServiceId }}<br />
          plan {{ validationResult.composition_plan.planDigest }}<br />
          graph {{ validationResult.composition_plan.releaseGraphDigest }}
        </p>
        <section
          v-for="serviceId in compositionServices(
            validationResult.composition_plan,
          )"
          :key="serviceId"
          class="composition-service"
        >
          <h5>{{ serviceId }}</h5>
          <div
            v-for="node in nodesForService(
              validationResult.composition_plan,
              serviceId,
            ).filter(
              (item) =>
                item.kind !== 'package' &&
                (item.unresolvedInputs.length || item.provider),
            )"
            :key="node.nodeId"
            class="binding-choice"
            :class="{
              ambiguous:
                node.provider &&
                !node.provider.selectedProviderId &&
                node.provider.candidates.length !== 1,
            }"
          >
            <div class="binding-choice-head">
              <strong>{{ node.kind }}</strong>
              <span class="mono">{{ node.name || node.nodeId }}</span>
              <span v-if="node.resourceType" class="chip">{{
                node.resourceType
              }}</span>
              <span v-if="node.lifecycle" class="chip ok">{{
                node.lifecycle
              }}</span>
              <span v-if="node.optional" class="chip">可选</span>
              <span v-if="node.provider?.selectedProviderId" class="chip ok">
                {{
                  providerCandidate(node, node.provider.selectedProviderId)
                    ?.kind || "UNKNOWN"
                }}
                · {{ node.provider.selectedProviderId }}（已解析）
              </span>
            </div>

            <template
              v-for="declaration in node.unresolvedInputs"
              :key="`${node.nodeId}:${declaration.key}`"
            >
              <div v-if="declaration.valueType === 'provider-id'" class="field">
                <label>
                  Provider
                  <span v-if="declaration.required" class="chip warn"
                    >必需</span
                  >
                </label>
                <select
                  v-if="declaration.allowedValues.length"
                  v-model="
                    compositionInputs[serviceId][node.nodeId][declaration.key]
                  "
                  class="select mono"
                  :aria-label="`${node.serviceId} ${node.name || node.kind} provider`"
                >
                  <option value="">明确选择 Provider</option>
                  <option
                    v-for="providerId in declaration.allowedValues"
                    :key="providerId"
                    :value="providerId"
                  >
                    {{ providerId }} ·
                    {{ providerCandidate(node, providerId)?.kind || "UNKNOWN" }}
                    {{
                      providerCandidate(node, providerId)?.serviceId
                        ? `· ${providerCandidate(node, providerId)?.serviceId}`
                        : ""
                    }}
                  </option>
                </select>
                <p v-else class="binding-warning">
                  UNRESOLVED：当前没有符合
                  {{ node.provider?.capability || node.resourceType }}
                  {{
                    node.provider?.versionRequirement || node.versionRequirement
                  }}
                  的 Provider；禁止安装。
                </p>
                <div
                  v-if="node.provider?.candidates.length"
                  class="provider-candidates"
                >
                  <span
                    v-for="candidate in node.provider.candidates"
                    :key="candidate.providerId"
                    class="chip"
                    :class="
                      candidate.kind === 'MANAGED'
                        ? 'ok'
                        : candidate.kind === 'EXTERNAL'
                          ? 'warn'
                          : ''
                    "
                  >
                    {{ candidate.kind }} · {{ candidate.providerId }} ·
                    {{ candidate.version }}
                  </span>
                </div>
              </div>

              <template
                v-else-if="
                  declaration.valueType === 'json-object' && node.schema
                "
              >
                <div
                  v-for="field in activeNodeConfigFields(
                    validationResult.composition_plan,
                    node,
                    compositionInputs,
                  ).filter((item) => !item.secret)"
                  :key="`${node.nodeId}:${field.path}`"
                  class="field"
                >
                  <label>
                    {{ field.title }}
                    <span class="mono muted">{{ field.path }}</span>
                    <span v-if="field.required" class="chip warn"
                      >活动分支必填</span
                    >
                  </label>
                  <select
                    v-if="field.allowedValues.length"
                    v-model="
                      compositionInputs[serviceId][node.nodeId][field.path]
                    "
                    class="select mono"
                    :aria-label="`${serviceId} config ${field.path}`"
                  >
                    <option value="">选择值</option>
                    <option
                      v-for="choice in field.allowedValues"
                      :key="String(choice)"
                      :value="String(choice)"
                    >
                      {{ choice }}
                    </option>
                  </select>
                  <label v-else-if="field.type === 'boolean'" class="check">
                    <input
                      v-model="
                        compositionInputs[serviceId][node.nodeId][field.path]
                      "
                      type="checkbox"
                      :aria-label="`${serviceId} config ${field.path}`"
                    />
                    启用
                  </label>
                  <input
                    v-else
                    v-model="
                      compositionInputs[serviceId][node.nodeId][field.path]
                    "
                    class="input mono"
                    :type="
                      field.type === 'integer' || field.type === 'number'
                        ? 'number'
                        : 'text'
                    "
                    autocomplete="off"
                    :aria-label="`${serviceId} config ${field.path}`"
                  />
                  <span v-if="field.description" class="hint">{{
                    field.description
                  }}</span>
                </div>
              </template>

              <div
                v-else-if="
                  declaration.valueType === 'secret-ref' &&
                  secretNodeState(
                    validationResult.composition_plan,
                    node,
                    compositionInputs,
                  ).active
                "
                class="field"
              >
                <label>
                  {{ node.name || declaration.key }} secret 引用
                  <span
                    v-if="
                      secretNodeState(
                        validationResult.composition_plan,
                        node,
                        compositionInputs,
                      ).required
                    "
                    class="chip warn"
                    >活动分支必填</span
                  >
                  <span class="chip">仅引用</span>
                </label>
                <input
                  v-model="
                    compositionInputs[serviceId][node.nodeId][declaration.key]
                  "
                  class="input mono"
                  type="password"
                  autocomplete="off"
                  placeholder="file://、vault:// 等 opaque reference；禁止明文 secret"
                  :aria-label="`${node.serviceId} ${node.name || declaration.key} secret reference`"
                />
                <span class="hint"
                  >界面不会回显或记录引用值；校验与安装只提交 opaque
                  reference。</span
                >
              </div>
            </template>
          </div>
        </section>
        <p
          v-for="error in compositionErrors"
          :key="error"
          class="binding-warning"
        >
          {{ error }}
        </p>
        <p
          v-if="validationResult.composition_input_error"
          class="binding-warning"
        >
          {{ validationResult.composition_input_error }}
        </p>
      </section>

      <section v-if="validationResult.runtime" class="contract-section">
        <h4>Node 真实运行时事实</h4>
        <div class="fact-grid">
          <span>Agent</span
          ><span class="mono">{{
            validationResult.runtime.agent_version || "未知"
          }}</span>
          <span>Docker</span
          ><span class="mono">{{
            validationResult.runtime.docker.server_version || "未知"
          }}</span>
          <span>平台</span
          ><span class="mono"
            >{{ validationResult.runtime.docker.os_type }}/{{
              validationResult.runtime.docker.architecture
            }}</span
          >
          <span>cgroup</span
          ><span class="mono">{{
            validationResult.runtime.docker.cgroup_version || "未知"
          }}</span>
          <span>Policy digest</span
          ><span class="mono digest-wrap">{{
            validationResult.runtime.runtime_policy_sha256
          }}</span>
          <span>Report</span
          ><span class="mono">{{
            validationResult.runtime.report_id || "未知"
          }}</span>
          <span>Observed</span
          ><span>{{
            validationResult.runtime.observed_at_ms
              ? new Date(
                  validationResult.runtime.observed_at_ms,
                ).toLocaleString()
              : "未知"
          }}</span>
          <span>Runtime inventory</span>
          <span
            class="chip"
            :class="validationResult.runtime.inventory_complete ? 'ok' : 'warn'"
          >
            {{
              validationResult.runtime.inventory_complete
                ? "完整"
                : validationResult.runtime.inventory_error || "不完整"
            }}
          </span>
          <span>事实有效期</span
          ><span
            >{{
              Math.round(validationResult.runtime.stale_after_ms / 1000)
            }}
            秒</span
          >
          <template v-if="selectedRuntimeProfile?.id === 'judge-sandbox-v1'">
            <span>允许的 Worker OCI</span>
            <span class="mono digest-wrap">
              {{
                validationResult.runtime.judge_sandbox_allowed_images.join(
                  "\n",
                ) || "未授权任何镜像"
              }}
            </span>
          </template>
        </div>
      </section>

      <section v-if="selectedRuntimeProfile" class="contract-section">
        <h4>Runtime Profile 与权限摘要</h4>
        <div class="runtime-contract-line">
          <span class="chip warn">{{ selectedRuntimeProfile.id }}</span>
          <span class="mono digest-wrap">{{
            selectedRuntimeProfile.profile_sha256
          }}</span>
        </div>
        <ul class="permission-list">
          <li v-for="permission in profilePermissionSummary" :key="permission">
            {{ permission }}
          </li>
        </ul>
        <p class="hint">健康门禁：{{ healthGateSummary }}</p>
      </section>

      <section
        v-if="validationResult.requirements.length"
        class="contract-section"
      >
        <h4>Required API Binding（必须显式确认）</h4>
        <div
          v-for="requirement in validationResult.requirements"
          :key="requirement.name"
          class="binding-choice"
          :class="{ ambiguous: requirement.ambiguous }"
        >
          <div class="binding-choice-head">
            <strong>{{ requirement.name }}</strong>
            <span class="mono"
              >{{ requirement.api_id }} {{ requirement.version }}</span
            >
            <span v-if="requirement.optional" class="chip">可选</span>
            <span v-if="requirement.ambiguous" class="chip warn">多个候选</span>
          </div>
          <select
            class="select"
            v-model="bindingSelections[requirement.name]"
            :aria-label="`${requirement.name} provider`"
          >
            <option value="">
              {{ requirement.optional ? "不绑定" : "请选择 Provider" }}
            </option>
            <option
              v-for="candidate in requirement.candidates"
              :key="candidate.deployment_id"
              :value="candidate.deployment_id"
              :disabled="!candidate.healthy"
            >
              {{ candidate.deployment_id }} · {{ candidate.service_id }} ·
              {{ candidate.node_id }} · {{ candidate.api_version }} ·
              {{ candidate.healthy ? "HEALTHY" : "UNHEALTHY" }}
              {{
                candidate.deployment_id ===
                requirement.recommended_provider_deployment_id
                  ? "· 推荐"
                  : ""
              }}
            </option>
          </select>
          <p v-if="requirement.reason" class="hint">{{ requirement.reason }}</p>
        </div>
        <p v-if="unresolvedRequiredBindings.length" class="binding-warning">
          仍有 {{ unresolvedRequiredBindings.length }} 个必需 API
          未选择；禁止安装。
        </p>
        <p
          v-else-if="validatedFingerprint !== currentValidationFingerprint()"
          class="binding-warning"
        >
          Binding 选择已变化，请重新校验。
        </p>
      </section>

      <section v-if="validationResult.bindings.length" class="contract-section">
        <h4>服务端最终 Binding 计划</h4>
        <div
          v-for="binding in validationResult.bindings"
          :key="binding.binding_id || binding.requirement_name"
          class="binding-plan-row"
        >
          <span
            ><strong>{{ binding.requirement_name }}</strong> →
            {{ binding.provider_deployment_id || "UNBOUND" }}</span
          >
          <span
            class="chip"
            :class="binding.health === 'HEALTHY' ? 'ok' : 'warn'"
          >
            {{ binding.state }} / {{ binding.health }}
          </span>
          <span class="mono">{{ binding.virtual_endpoint }}</span>
        </div>
      </section>

      <details v-if="validationResult.topology_diff" class="contract-section">
        <summary>本次安装将产生的 Topology diff</summary>
        <pre>{{ JSON.stringify(validationResult.topology_diff, null, 2) }}</pre>
      </details>
      <p
        v-else-if="validationResult.requirements.length"
        class="binding-warning"
      >
        服务端没有返回本次安装的 prospective topology diff，禁止安装。
      </p>
    </div>

    <template #footer>
      <button class="btn" @click="installOpen = false">关闭</button>
      <button
        class="btn"
        :disabled="
          validating ||
          !store.supportsAction('release.validate') ||
          !installTarget ||
          !targetNodeId ||
          !!pipelineOptionsError ||
          (topologyRequired && (!topologyId || !topologyRevisionId))
        "
        @click="runValidate"
      >
        {{ validating ? "校验中…" : "先校验 Release" }}
      </button>
      <button
        class="btn primary"
        :disabled="
          installing ||
          !store.supportsAction('release.install') ||
          !installTarget ||
          !targetNodeId ||
          !installReady
        "
        @click="runInstall"
      >
        {{ installing ? "提交中…" : "安装、启动并验证健康" }}
      </button>
    </template>
  </Modal>

  <Modal
    :open="importOpen"
    title="仅导入 Release"
    width="560px"
    @close="importOpen = false"
  >
    <div class="field">
      <label>受信任 Catalog Release</label>
      <select class="select" v-model="importTargetKey">
        <option value="" disabled>选择已验证签名的 Release</option>
        <option
          v-for="module in modules"
          :key="moduleKey(module)"
          :value="moduleKey(module)"
        >
          {{ module.id }}@{{ module.version }} · {{ module.channel }} ·
          {{ module.source_id }}
        </option>
      </select>
    </div>
    <div class="field">
      <label>目标平台 Node</label>
      <select class="select" v-model="importTargetNodeId">
        <option value="" disabled>选择 READY Node 以确定 OS/架构</option>
        <option
          v-for="node in readyNodes"
          :key="node.node_id"
          :value="node.node_id"
        >
          {{ node.node_id }} · {{ node.host_ip || "loopback" }}
        </option>
      </select>
    </div>
    <p class="hint">
      服务端会重新验证 Catalog 签名、metadata SHA-256 与平台；仅导入不会创建
      Operation、Job、Deployment 或容器。
    </p>
    <template #footer>
      <button class="btn" @click="importOpen = false">取消</button>
      <button
        class="btn primary"
        :disabled="
          importing ||
          !store.supportsAction('release.import') ||
          !importTarget ||
          !importTargetNodeId
        "
        @click="runImport"
      >
        {{ importing ? "导入中…" : "仅导入" }}
      </button>
    </template>
  </Modal>

  <Modal
    :open="catalogManagerOpen"
    title="受信任 Catalog 来源"
    width="760px"
    @close="catalogManagerOpen = false"
  >
    <form
      v-if="store.supportsAction('catalog.register')"
      class="catalog-form"
      data-action="catalog.register"
      @submit.prevent="registerCatalog"
    >
      <div class="field">
        <label>Catalog ID</label>
        <input
          v-model="catalogForm.id"
          class="input"
          required
          placeholder="production"
        />
      </div>
      <div class="field catalog-url-field">
        <label>Catalog v2 URL</label>
        <input
          v-model="catalogForm.url"
          class="input"
          type="text"
          required
          placeholder="https://catalog.example/catalog-v2.json 或仓库内相对路径"
        />
      </div>
      <div class="field">
        <label>可信 Ed25519 key ID</label>
        <input
          v-model="catalogForm.required_key_id"
          class="input"
          required
          placeholder="release-key-2026"
        />
      </div>
      <div class="field">
        <label>认证 Secret 引用（可选）</label>
        <input
          v-model="catalogForm.auth_secret_ref"
          class="input"
          placeholder="env:OJOS_CATALOG_TOKEN"
        />
      </div>
      <div class="field catalog-public-key-field">
        <label>Ed25519 公钥（32 字节 padded base64，首次信任时必填）</label>
        <input
          v-model="catalogForm.public_key"
          class="input"
          autocomplete="off"
          spellcheck="false"
          minlength="44"
          maxlength="44"
          pattern="[A-Za-z0-9+/]{43}="
          placeholder="44 字符 padded base64 Ed25519 公钥"
        />
      </div>
      <button class="btn primary" type="submit" :disabled="catalogSaving">
        {{ catalogSaving ? "注册中…" : "注册并验证" }}
      </button>
    </form>

    <div v-if="catalogLoading" class="empty">正在加载 Catalog 来源…</div>
    <div v-else-if="catalogs.length" class="catalog-list">
      <div v-for="source in catalogs" :key="source.id" class="card catalog-row">
        <div>
          <div>
            <strong>{{ source.id }}</strong>
            <span class="chip" :class="source.enabled ? 'ok' : 'warn'">{{
              source.enabled ? "已启用" : "已停用"
            }}</span>
          </div>
          <div class="mono muted">{{ source.url }}</div>
          <div class="muted">
            签名 key：<span class="mono">{{ source.required_key_id }}</span>
          </div>
        </div>
        <button
          v-if="store.supportsAction('catalog.remove')"
          class="btn danger sm"
          data-action="catalog.remove"
          :disabled="!!catalogRemoving"
          @click="removeCatalog(source)"
        >
          {{ catalogRemoving === source.id ? "移除中…" : "移除" }}
        </button>
      </div>
    </div>
    <div v-else class="empty">尚未注册 Catalog 来源。</div>

    <template #footer>
      <button class="btn" @click="catalogManagerOpen = false">关闭</button>
      <button class="btn" :disabled="catalogLoading" @click="loadCatalogs">
        刷新
      </button>
    </template>
  </Modal>
</template>

<style scoped>
.store-body {
  flex: 1;
  overflow-y: auto;
  padding: 18px 22px;
}
.load-state {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 14px;
  color: var(--muted);
}
.error-state {
  border-color: rgba(248, 113, 113, 0.45);
  color: var(--err);
}

.catalog-form {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 12px;
  align-items: end;
  margin-bottom: 18px;
}
.catalog-form .field {
  margin: 0;
}
.catalog-url-field {
  grid-column: span 2;
}
.catalog-public-key-field {
  grid-column: span 2;
}
.catalog-form > button {
  justify-self: start;
}
.catalog-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.catalog-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 12px;
}
.catalog-row > div {
  min-width: 0;
}
.catalog-row .mono {
  overflow-wrap: anywhere;
}

.status-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 16px;
}

.uninstall-authorization {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 16px;
  padding: 10px 14px;
  border-color: rgba(245, 158, 11, 0.35);
}
.uninstall-authorization .hint {
  margin-left: auto;
  text-align: right;
}

.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: 14px;
}

.module-card {
  display: flex;
  flex-direction: column;
  gap: 10px;
  transition:
    border-color 0.15s ease,
    transform 0.15s ease;
}
.module-card:hover {
  border-color: var(--border-strong);
  transform: translateY(-1px);
}
.module-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 10px;
}
.module-name {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-strong);
}
.module-id {
  font-size: 11px;
  color: var(--faint);
}
.module-desc {
  margin: 0;
  font-size: 12.5px;
  color: var(--muted);
  min-height: 36px;
}
.module-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.module-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-top: 2px;
}
.module-source {
  font-size: 10.5px;
  flex: 1 1 160px;
  min-width: 0;
  margin-left: auto;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.row {
  display: flex;
  gap: 8px;
}
.row .input {
  flex: 1;
}
.options {
  margin-bottom: 14px;
  flex-wrap: wrap;
  gap: 14px;
}
.check {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 12.5px;
  color: var(--muted);
  cursor: pointer;
}
.check input {
  accent-color: var(--accent);
}

.divider {
  display: flex;
  align-items: center;
  gap: 12px;
  margin: 6px 0 14px;
  color: var(--faint);
  font-size: 11.5px;
}
.divider::before,
.divider::after {
  content: "";
  flex: 1;
  height: 1px;
  background: var(--border);
}

.install-result {
  margin-top: 6px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}

.topology-selection {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(240px, 0.9fr);
  gap: 8px;
}

.contract-section {
  margin-top: 14px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}
.contract-section h4 {
  margin: 0 0 9px;
  font-size: 12.5px;
  color: var(--text-strong);
}
.fact-grid {
  display: grid;
  grid-template-columns: 130px minmax(0, 1fr);
  gap: 6px 12px;
  font-size: 12px;
}
.fact-grid > span:nth-child(odd) {
  color: var(--faint);
}
.runtime-contract-line,
.binding-choice-head,
.binding-plan-row {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
}
.permission-list {
  margin: 9px 0;
  padding-left: 20px;
  color: var(--muted);
  font-size: 12px;
}
.binding-choice {
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 10px;
  margin-top: 8px;
  border: 1px solid var(--border);
  border-radius: 8px;
}
.binding-choice.ambiguous {
  border-color: rgba(245, 158, 11, 0.45);
}
.binding-choice-head .mono {
  color: var(--muted);
  font-size: 11px;
}
.composition-service {
  margin-top: 12px;
  padding: 10px;
  border: 1px solid rgba(148, 163, 184, 0.12);
  border-radius: 9px;
}
.composition-service h5 {
  margin: 0 0 8px;
  color: var(--text-strong);
  font-size: 12px;
}
.provider-candidates {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.binding-warning {
  margin: 9px 0 0;
  color: var(--warn);
  font-size: 12px;
}
.binding-plan-row {
  justify-content: space-between;
  padding: 7px 0;
  border-top: 1px solid rgba(148, 163, 184, 0.08);
  font-size: 11.5px;
}
.binding-plan-row:first-of-type {
  border-top: 0;
}
.digest-wrap {
  overflow-wrap: anywhere;
}
.contract-section pre {
  max-height: 220px;
  overflow: auto;
  padding: 9px;
  border-radius: 8px;
  background: var(--bg-soft);
  font-size: 10.5px;
}

@media (max-width: 760px) {
  .topology-selection {
    grid-template-columns: 1fr;
  }
}

code {
  background: rgba(148, 163, 184, 0.12);
  padding: 1px 6px;
  border-radius: 4px;
}
</style>
