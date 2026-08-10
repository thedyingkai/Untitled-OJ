<script setup lang="ts">
import { ref } from "vue";
import PageHeader from "../components/PageHeader.vue";
import StatusChip from "../components/StatusChip.vue";
import Modal from "../components/Modal.vue";
import { api } from "../api";
import { deploymentMutationMessage } from "../deployment-errors";
import { useOrchestrator } from "../store";
import type { DeploymentBindings, DeploymentRow } from "../types";

const store = useOrchestrator();
const busy = ref("");
type DeploymentAction = "start" | "stop" | "restart" | "uninstall";

const detailOpen = ref(false);
const detailLoading = ref(false);
const detailDeployment = ref<DeploymentRow | null>(null);
const detailBindings = ref<DeploymentBindings | null>(null);
const healthEvidence = ref<Record<string, unknown> | null>(null);

function formatUnixMs(value: number): string {
  return value > 0 ? new Date(value).toLocaleString() : "未报告";
}

async function showDetails(deployment: DeploymentRow) {
  if (!store.ensureAction("deployment.get")) return;
  detailOpen.value = true;
  detailLoading.value = true;
  detailDeployment.value = deployment;
  detailBindings.value = null;
  healthEvidence.value = null;
  try {
    const [detail, bindings, health] = await Promise.allSettled([
      api.deployment(deployment.deployment_id),
      api.deploymentBindings(deployment.deployment_id),
      api.deploymentHealth(deployment.deployment_id),
    ]);
    if (detail.status === "fulfilled") detailDeployment.value = detail.value;
    if (bindings.status === "fulfilled") detailBindings.value = bindings.value;
    if (health.status === "fulfilled") healthEvidence.value = health.value;
    const errors = [detail, bindings, health]
      .filter((result): result is PromiseRejectedResult => result.status === "rejected")
      .map((result) => String(result.reason?.message ?? result.reason));
    if (errors.length) {
      store.toast("err", `部分 Deployment 证据加载失败：${errors.join("；")}`);
    }
  } finally {
    detailLoading.value = false;
  }
}

async function lifecycle(action: DeploymentAction, deployment: DeploymentRow) {
  const capability = `deployment.${action}`;
  if (!store.ensureAction(capability)) return;
  if (
    (action === "stop" || action === "uninstall") &&
    !window.confirm(
      `${action === "stop" ? "停止" : "卸载"}部署 ${deployment.deployment_id}？`,
    )
  ) return;
  busy.value = `${action}:${deployment.deployment_id}`;
  try {
    const result = await api.deploymentAction(deployment.deployment_id, action);
    store.toast("ok", `操作已提交：${result.operation_id}`);
    await store.refreshCore(true);
  } catch (err) {
    const message =
      action === "uninstall"
        ? await deploymentMutationMessage(err, deployment.deployment_id)
        : (err as Error).message;
    store.toast("err", `${capability} 失败：${message}`);
  } finally {
    busy.value = "";
  }
}
</script>

<template>
  <PageHeader title="服务" subtitle="按主机列出已部署实例，并精确执行生命周期动作">
    <button class="btn sm" @click="store.refreshCore()">刷新</button>
  </PageHeader>

  <div class="services-body">
    <div
      class="card"
      v-if="store.deployments.length"
      style="padding: 0; overflow: hidden"
    >
      <table class="table">
        <thead>
          <tr>
            <th>Deployment</th>
            <th>服务</th>
            <th>Node</th>
            <th>运行时</th>
            <th>期望 / 实际</th>
            <th>制品 Digest</th>
            <th>Endpoint</th>
            <th>健康</th>
            <th style="text-align: right">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="deployment in store.deployments"
            :key="deployment.deployment_id"
          >
            <td class="mono">{{ deployment.deployment_id }}</td>
            <td>
              <div style="font-weight: 600">
                {{ deployment.name || deployment.service_id }}
              </div>
              <div class="mono muted" style="font-size: 11px">
                {{ deployment.service_id }}
              </div>
            </td>
            <td>
              <div class="mono">{{ deployment.node_id }}</div>
              <div class="muted" style="font-size: 11px">{{ deployment.host_ip }}</div>
            </td>
            <td class="muted">{{ deployment.runtime }}</td>
            <td>
              <div class="mono">{{ deployment.desired_state }}</div>
              <StatusChip :status="deployment.observed_state || 'unknown'" />
            </td>
            <td class="mono digest-cell">{{ deployment.artifact_digest }}</td>
            <td>
              <div class="mono endpoint-cell">
                {{ deployment.endpoint || "未登记" }}
              </div>
              <div
                v-if="deployment.endpoint_count > 1"
                class="muted endpoint-extra"
              >
                共 {{ deployment.endpoint_count }} 个，生命周期动作使用上列 Endpoint
              </div>
            </td>
            <td>
              <StatusChip
                :status="deployment.endpoint_health || 'unknown'"
                :title="
                  deployment.reachable
                    ? 'Endpoint 最近一次检查可达'
                    : 'Endpoint 尚未确认可达或最近一次检查失败'
                "
              />
            </td>
            <td>
              <div class="row-actions">
                <button
                  class="btn sm"
                  :disabled="!!busy || !store.supportsAction('deployment.get')"
                  @click="showDetails(deployment)"
                >
                  详情
                </button>
                <button
                  class="btn sm"
                  :disabled="
                    !!busy ||
                    !store.supportsAction('deployment.start')
                  "
                  @click="lifecycle('start', deployment)"
                >
                  启动
                </button>
                <button
                  class="btn sm"
                  :disabled="
                    !!busy ||
                    !store.supportsAction('deployment.stop')
                  "
                  @click="lifecycle('stop', deployment)"
                >
                  停止
                </button>
                <button
                  class="btn sm"
                  :disabled="
                    !!busy ||
                    !store.supportsAction('deployment.restart')
                  "
                  @click="lifecycle('restart', deployment)"
                >
                  重启
                </button>
                <button
                  class="btn danger sm"
                  :disabled="
                    !!busy || !store.supportsAction('deployment.uninstall')
                  "
                  @click="lifecycle('uninstall', deployment)"
                >
                  卸载
                </button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-else class="empty">
      <span class="icon">❖</span>
      <span>尚无部署记录，先到商店安装模块。</span>
    </div>
  </div>

  <Modal
    :open="detailOpen"
    :title="detailDeployment ? `Deployment ${detailDeployment.deployment_id}` : 'Deployment 详情'"
    width="820px"
    @close="detailOpen = false"
  >
    <div v-if="detailLoading" class="empty">正在读取运行态、Binding 与健康证据…</div>
    <template v-else-if="detailDeployment">
      <section class="detail-section">
        <h4>运行时证明</h4>
        <div class="detail-grid">
          <span>Service / Node</span><span class="mono">{{ detailDeployment.service_id }} / {{ detailDeployment.node_id }}</span>
          <span>Release</span><span class="mono">{{ detailDeployment.release_version || detailDeployment.version || "未知" }}</span>
          <span>Runtime Profile</span><span class="mono">{{ detailDeployment.runtime_profile || "未知" }}</span>
          <span>Profile digest</span><span class="mono digest-wrap">{{ detailDeployment.runtime_profile_sha256 || "未报告" }}</span>
          <span>Policy digest</span><span class="mono digest-wrap">{{ detailDeployment.runtime_policy_sha256 || "未报告" }}</span>
          <span>Effective HostConfig</span><span class="mono digest-wrap">{{ detailDeployment.effective_host_config_sha256 || "未报告" }}</span>
          <span>Agent attestation</span>
          <span class="chip" :class="detailDeployment.runtime_attested ? 'ok' : 'warn'">
            {{ detailDeployment.runtime_attested ? "已验证" : "未验证" }}
          </span>
          <span>Runtime observed</span><span>{{ formatUnixMs(detailDeployment.last_observed_at_ms) }}</span>
          <span>Runtime drift</span>
          <span class="chip" :class="detailDeployment.drift_reason ? 'warn' : 'ok'">
            {{ detailDeployment.drift_reason || "无" }}
          </span>
          <span>Desired / Observed</span><span>{{ detailDeployment.desired_state }} / {{ detailDeployment.observed_state }}</span>
        </div>
      </section>

      <section class="detail-section">
        <h4>API Binding 与凭据代次</h4>
        <div class="detail-grid credential-evidence">
          <span>Credential expires</span><span>{{ formatUnixMs(detailDeployment.credential_expires_at_ms) }}</span>
          <span>Last refresh</span><span>{{ formatUnixMs(detailDeployment.credential_last_success_at_ms) }}</span>
          <span>Refresh error</span>
          <span class="chip" :class="detailDeployment.credential_last_error ? 'warn' : 'ok'">
            {{ detailDeployment.credential_last_error || "无" }}
          </span>
        </div>
        <div v-if="detailBindings?.items.length" class="binding-list">
          <article
            v-for="binding in detailBindings.items"
            :key="binding.binding_id"
            class="binding-card"
          >
            <div class="binding-card-head">
              <strong>{{ binding.requirement_name }}</strong>
              <span class="mono">{{ binding.api_id }}@{{ binding.api_version }}</span>
              <StatusChip :status="binding.health" />
            </div>
            <div class="detail-grid compact">
              <span>Provider</span><span class="mono">{{ binding.provider_deployment_id || "UNBOUND" }} / {{ binding.provider_node_id || "-" }}</span>
              <span>Gateway path</span><span class="mono">{{ binding.virtual_endpoint }}</span>
              <span>State</span><span>{{ binding.desired_state || "-" }} / {{ binding.observed_state || binding.state }}</span>
              <span>Generation</span><span class="mono">context {{ binding.context_generation || "未报告" }} · credential {{ binding.credential_generation || "未报告" }}</span>
              <span>Topology</span><span class="mono">{{ binding.topology_id || "-" }} / {{ binding.topology_revision_id || "-" }}</span>
            </div>
            <div v-if="binding.drift.length" class="binding-drift">
              Drift：{{ binding.drift.join("；") }}
            </div>
            <div v-if="binding.reason" class="muted">{{ binding.reason }}</div>
          </article>
        </div>
        <div v-else class="empty compact-empty">该 Deployment 没有 API Binding。</div>
      </section>

      <details v-if="healthEvidence" class="detail-section">
        <summary>健康证据</summary>
        <pre>{{ JSON.stringify(healthEvidence, null, 2) }}</pre>
      </details>
    </template>
    <template #footer>
      <button class="btn" @click="detailOpen = false">关闭</button>
      <button
        class="btn"
        :disabled="detailLoading || !detailDeployment"
        @click="detailDeployment && showDetails(detailDeployment)"
      >刷新证据</button>
    </template>
  </Modal>
</template>

<style scoped>
.services-body {
  flex: 1;
  overflow-y: auto;
  padding: 18px 22px;
}
.row-actions {
  display: flex;
  gap: 6px;
  justify-content: flex-end;
}
.detail-section + .detail-section {
  margin-top: 18px;
  padding-top: 16px;
  border-top: 1px solid var(--border);
}
.detail-section h4 {
  margin: 0 0 10px;
  font-size: 13px;
}
.detail-grid {
  display: grid;
  grid-template-columns: 160px minmax(0, 1fr);
  gap: 7px 12px;
  font-size: 12px;
}
.detail-grid.compact {
  grid-template-columns: 130px minmax(0, 1fr);
}
.credential-evidence {
  margin-bottom: 12px;
}
.detail-grid > span:nth-child(odd) {
  color: var(--faint);
}
.binding-list {
  display: flex;
  flex-direction: column;
  gap: 9px;
}
.binding-card {
  padding: 11px;
  border: 1px solid var(--border);
  border-radius: 9px;
}
.binding-card-head {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 9px;
}
.binding-card-head .mono {
  color: var(--muted);
}
.binding-drift {
  margin-top: 8px;
  color: var(--warn);
  font-size: 11.5px;
}
.compact-empty {
  min-height: 70px;
}
.digest-wrap {
  overflow-wrap: anywhere;
}
.detail-section pre {
  max-height: 260px;
  overflow: auto;
  padding: 10px;
  border-radius: 8px;
  background: var(--bg-soft);
  font-size: 10.5px;
}
.endpoint-cell {
  max-width: 260px;
  overflow: hidden;
  text-overflow: ellipsis;
}
.endpoint-extra {
  margin-top: 3px;
  max-width: 260px;
  font-size: 10.5px;
}
.check-config {
  white-space: nowrap;
}
.check-config .mono {
  display: block;
  margin-top: 2px;
  font-size: 10.5px;
}

.driver-authorization {
  margin-bottom: 16px;
  padding: 12px 16px;
  border-color: rgba(245, 158, 11, 0.35);
}
.driver-check {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  cursor: pointer;
}
.driver-check input {
  margin-top: 3px;
  accent-color: var(--accent);
}
.driver-check span {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.driver-check strong {
  color: var(--text-strong);
  font-size: 12.5px;
}
.driver-check small {
  color: var(--muted);
  font-size: 11.5px;
  line-height: 1.5;
}

.host-card {
  margin-bottom: 16px;
  padding: 14px 16px;
}
.host-head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  flex-wrap: wrap;
  margin-bottom: 10px;
}
.host-head h3 {
  font-size: 13.5px;
}
.host-hint {
  font-size: 11.5px;
}
.host-list {
  display: flex;
  flex-direction: column;
}
.host-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 9px 0;
  border-top: 1px solid rgba(148, 163, 184, 0.07);
}
.host-row:first-child {
  border-top: none;
}
.host-info {
  display: flex;
  align-items: center;
  gap: 9px;
  min-width: 0;
  flex-wrap: wrap;
}
.host-ip {
  font-size: 12.5px;
  color: var(--text-strong);
  word-break: break-all;
}
.host-sub {
  font-size: 11.5px;
}
.host-empty {
  font-size: 12.5px;
}
</style>
