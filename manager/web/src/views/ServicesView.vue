<script setup lang="ts">
import { computed, ref } from "vue";
import PageHeader from "../components/PageHeader.vue";
import StatusChip from "../components/StatusChip.vue";
import { api } from "../api";
import { useOrchestrator } from "../store";
import type { DeploymentRow } from "../types";

const store = useOrchestrator();
const busy = ref("");
const executeDriver = ref(false);

interface HostRow {
  ip: string;
  services: number;
  endpoints: number;
}

/**
 * 主机清单来自明确的部署行，不再从端点字符串猜主机，也因此兼容 IPv6。
 */
const hosts = computed<HostRow[]>(() => {
  const grouped = new Map<string, { services: Set<string>; endpoints: number }>();
  for (const deployment of store.deployments) {
    if (!deployment.host_ip) continue;
    let entry = grouped.get(deployment.host_ip);
    if (!entry) {
      entry = { services: new Set<string>(), endpoints: 0 };
      grouped.set(deployment.host_ip, entry);
    }
    entry.endpoints += deployment.endpoint_count;
    if (deployment.service_id) entry.services.add(deployment.service_id);
  }
  return [...grouped.entries()]
    .map(([ip, entry]) => ({
      ip,
      services: entry.services.size,
      endpoints: entry.endpoints,
    }))
    .sort((left, right) => left.ip.localeCompare(right.ip));
});

/**
 * 主机整机启停：core 的 host.start / host.stop 取 host_ip + confirm=true，
 * 走通用 POST /actions 派发。
 */
async function hostLifecycle(action: "host.start" | "host.stop", ip: string) {
  if (
    action === "host.stop" &&
    !window.confirm(`停止主机 ${ip} 上的全部服务？该操作会中断其上所有端点。`)
  ) {
    return;
  }
  busy.value = `${action}:${ip}`;
  try {
    await api.dispatchAction(action, {
      host_ip: ip,
      confirm: "true",
      execute_service_driver: executeDriver.value ? "true" : "false",
    });
    store.toast(
      "ok",
      `${action === "host.start" ? "启动" : "停止"}主机 ${ip} 的动作已下发`,
    );
    await store.refreshCore();
  } catch (err) {
    store.toast("err", `${action} 失败：${(err as Error).message}`);
  } finally {
    busy.value = "";
  }
}

async function lifecycle(action: string, deployment: DeploymentRow) {
  if (!deployment.endpoint) {
    store.toast(
      "err",
      `${deployment.service_id}@${deployment.host_ip} 没有登记 Endpoint，无法精确执行生命周期动作`,
    );
    return;
  }
  busy.value = `${action}:${deployment.service_id}@${deployment.host_ip}`;
  try {
    await api.dispatchAction(action, {
      service_id: deployment.service_id,
      host_ip: deployment.host_ip,
      endpoint: deployment.endpoint,
      version: deployment.version,
      confirm: "true",
      execute_service_driver: executeDriver.value ? "true" : "false",
    });
    store.toast("ok", `${action} 已执行`);
    await store.refreshCore();
  } catch (err) {
    store.toast("err", `${action} 失败：${(err as Error).message}`);
  } finally {
    busy.value = "";
  }
}

function actionTitle(deployment: DeploymentRow): string {
  if (!deployment.endpoint) return "该部署没有登记 Endpoint，无法精确选择运行实例";
  return executeDriver.value ? "" : "请先授权执行运行时驱动";
}
</script>

<template>
  <PageHeader title="服务" subtitle="按主机列出已部署实例，并精确执行生命周期动作">
    <button class="btn sm" @click="store.refreshCore()">刷新</button>
  </PageHeader>

  <div class="services-body">
    <section class="card driver-authorization">
      <label class="driver-check">
        <input v-model="executeDriver" type="checkbox" :disabled="!!busy" />
        <span>
          <strong>授权执行运行时驱动</strong>
          <small>
            勾选后，启动、停止和重启会实际执行容器或本地进程命令；未勾选时这些按钮不可用。
          </small>
        </span>
      </label>
    </section>

    <!-- 主机整机启停 -->
    <section class="card host-card">
      <div class="host-head">
        <h3>主机</h3>
        <span class="muted host-hint">
          按部署记录聚合，可整机启停其上全部服务
        </span>
      </div>
      <div v-if="hosts.length" class="host-list">
        <div v-for="host in hosts" :key="host.ip" class="host-row">
          <div class="host-info">
            <span class="mono host-ip">{{ host.ip }}</span>
            <span class="chip">{{ host.services }} 个服务</span>
            <span class="muted host-sub">{{ host.endpoints }} 个端点</span>
          </div>
          <div class="row-actions">
            <button
              class="btn sm"
              :disabled="!!busy || !executeDriver"
              :title="executeDriver ? '' : '请先授权执行运行时驱动'"
              @click="hostLifecycle('host.start', host.ip)"
            >
              启动全部
            </button>
            <button
              class="btn danger sm"
              :disabled="!!busy || !executeDriver"
              :title="executeDriver ? '' : '请先授权执行运行时驱动'"
              @click="hostLifecycle('host.stop', host.ip)"
            >
              停止全部
            </button>
          </div>
        </div>
      </div>
      <div v-else class="muted host-empty">
        尚无部署记录，先到商店安装模块。
      </div>
    </section>

    <div
      class="card"
      v-if="store.deployments.length"
      style="padding: 0; overflow: hidden"
    >
      <table class="table">
        <thead>
          <tr>
            <th>服务</th>
            <th>主机</th>
            <th>版本</th>
            <th>类型</th>
            <th>运行时</th>
            <th>部署状态</th>
            <th>Endpoint</th>
            <th>最近检查</th>
            <th>检查配置</th>
            <th style="text-align: right">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="deployment in store.deployments"
            :key="`${deployment.host_ip}:${deployment.service_id}`"
          >
            <td>
              <div style="font-weight: 600">
                {{ deployment.name || deployment.service_id }}
              </div>
              <div class="mono muted" style="font-size: 11px">
                {{ deployment.service_id }}
              </div>
            </td>
            <td class="mono">{{ deployment.host_ip }}</td>
            <td class="mono">{{ deployment.version }}</td>
            <td><span class="chip">{{ deployment.kind }}</span></td>
            <td class="muted">{{ deployment.runtime }}</td>
            <td><StatusChip :status="deployment.status || 'unknown'" /></td>
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
            <td class="muted check-config">
              {{ deployment.protocol || "—" }}
              <span class="mono">{{ deployment.health_path || "未配置路径" }}</span>
            </td>
            <td>
              <div class="row-actions">
                <button
                  class="btn sm"
                  :disabled="!!busy || !executeDriver || !deployment.endpoint"
                  :title="actionTitle(deployment)"
                  @click="lifecycle('service.start', deployment)"
                >
                  启动
                </button>
                <button
                  class="btn sm"
                  :disabled="!!busy || !executeDriver || !deployment.endpoint"
                  :title="actionTitle(deployment)"
                  @click="lifecycle('service.stop', deployment)"
                >
                  停止
                </button>
                <button
                  class="btn sm"
                  :disabled="!!busy || !executeDriver || !deployment.endpoint"
                  :title="actionTitle(deployment)"
                  @click="lifecycle('service.restart', deployment)"
                >
                  重启
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
