<script setup lang="ts">
import { ref } from "vue";
import PageHeader from "../components/PageHeader.vue";
import StatusChip from "../components/StatusChip.vue";
import { api } from "../api";
import { useOrchestrator } from "../store";
import type { DeploymentRow } from "../types";

const store = useOrchestrator();
const busy = ref("");
type DeploymentAction = "start" | "stop" | "restart" | "uninstall";

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
    store.toast("err", `${capability} 失败：${(err as Error).message}`);
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
            :key="`${deployment.host_ip}:${deployment.service_id}`"
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
