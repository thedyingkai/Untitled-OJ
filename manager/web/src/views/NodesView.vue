<script setup lang="ts">
import { ref } from "vue";
import { api } from "../api";
import PageHeader from "../components/PageHeader.vue";
import StatusChip from "../components/StatusChip.vue";
import { useOrchestrator } from "../store";
import type { NodeRow } from "../types";

const store = useOrchestrator();
const busy = ref("");
const health = ref<Record<string, unknown> | null>(null);
const showEnrollment = ref(false);
const enrollment = ref({
  node_id: "",
  host_ip: "",
  role: "standalone",
  parent_node_id: "",
  ttl_seconds: 600,
});
const issuedEnrollment = ref<{
  node_id: string;
  enrollment_code: string;
  expires_at_ms: number;
} | null>(null);

async function createEnrollment() {
  if (!store.ensureAction("node.register")) return;
  busy.value = "enroll";
  issuedEnrollment.value = null;
  try {
    issuedEnrollment.value = await api.createNodeEnrollment({
      node_id: enrollment.value.node_id.trim(),
      host_ip: enrollment.value.host_ip.trim(),
      role: enrollment.value.role.trim() || "standalone",
      parent_node_id: enrollment.value.parent_node_id.trim(),
      labels: {},
      ttl_seconds: enrollment.value.ttl_seconds,
    });
    store.toast("ok", "一次性 Node 注册码已签发，请立即交给对应 Agent");
    await store.refreshCore(true);
  } catch (error) {
    store.toast("err", `Node 注册码签发失败：${(error as Error).message}`);
  } finally {
    busy.value = "";
  }
}

async function copyEnrollmentCode() {
  if (!issuedEnrollment.value) return;
  await navigator.clipboard.writeText(issuedEnrollment.value.enrollment_code);
  store.toast("ok", "注册码已复制；页面不会持久保存它");
}

async function revokeCertificates(node: NodeRow) {
  if (!store.ensureAction("node.revoke")) return;
  const reason = window.prompt(`吊销 Node ${node.node_id} 的全部有效证书，填写原因：`);
  if (!reason?.trim()) return;
  busy.value = `revoke:${node.node_id}`;
  try {
    const result = await api.revokeNodeCertificates(node.node_id, reason.trim());
    store.toast("ok", `已吊销 ${result.revoked_certificates} 张证书`);
    await store.refreshCore(true);
  } catch (error) {
    store.toast("err", `Node 证书吊销失败：${(error as Error).message}`);
  } finally {
    busy.value = "";
  }
}

async function inspectHealth(node: NodeRow) {
  if (!store.ensureAction("node.health")) return;
  busy.value = `health:${node.node_id}`;
  try {
    health.value = await api.nodeHealth(node.node_id);
  } catch (error) {
    store.toast("err", `Node 健康查询失败：${(error as Error).message}`);
  } finally {
    busy.value = "";
  }
}

async function drain(node: NodeRow) {
  if (!store.ensureAction("node.drain")) return;
  if (!window.confirm(`Drain Node ${node.node_id}？它将停止接收新任务。`)) return;
  busy.value = `drain:${node.node_id}`;
  try {
    const result = await api.nodeDrain(node.node_id);
    store.toast("ok", `Drain Operation 已提交：${result.operation_id}`);
    await store.refreshCore(true);
  } catch (error) {
    store.toast("err", `Node drain 失败：${(error as Error).message}`);
  } finally {
    busy.value = "";
  }
}

async function remove(node: NodeRow) {
  if (!store.ensureAction("node.remove")) return;
  if (!window.confirm(`移除已排空 Node ${node.node_id}？`)) return;
  busy.value = `remove:${node.node_id}`;
  try {
    const result = await api.nodeRemove(node.node_id);
    store.toast("ok", `Remove Operation 已提交：${result.operation_id}`);
    await store.refreshCore(true);
  } catch (error) {
    store.toast("err", `Node remove 失败：${(error as Error).message}`);
  } finally {
    busy.value = "";
  }
}
</script>

<template>
  <PageHeader title="Nodes" subtitle="固定任务归属、健康状态与排空生命周期">
    <button
      v-if="store.supportsAction('node.register')"
      class="btn sm"
      :disabled="!!busy"
      @click="showEnrollment = !showEnrollment"
    >
      注册 Node
    </button>
    <button class="btn sm" :disabled="!!busy" @click="store.refreshCore(true)">
      刷新
    </button>
  </PageHeader>

  <div class="nodes-body">
    <form v-if="showEnrollment" class="card enrollment-card" @submit.prevent="createEnrollment">
      <div class="form-grid">
        <label>
          <span>Node ID</span>
          <input v-model="enrollment.node_id" required maxlength="128" placeholder="edge-node-01" />
        </label>
        <label>
          <span>Agent 地址</span>
          <input v-model="enrollment.host_ip" required placeholder="10.0.0.21" />
        </label>
        <label>
          <span>角色</span>
          <input v-model="enrollment.role" required />
        </label>
        <label>
          <span>注册码有效期（秒）</span>
          <input v-model.number="enrollment.ttl_seconds" type="number" min="60" max="3600" required />
        </label>
      </div>
      <div class="form-actions">
        <button class="btn primary sm" type="submit" :disabled="!!busy">签发一次性注册码</button>
        <span class="muted">Agent 首次兑换后即失效；证书到期前由 Agent 通过 mTLS 自动续签。</span>
      </div>
      <div v-if="issuedEnrollment" class="issued-code">
        <div>
          <strong>{{ issuedEnrollment.node_id }}</strong>
          · 有效至 {{ new Date(issuedEnrollment.expires_at_ms).toLocaleString() }}
        </div>
        <code>{{ issuedEnrollment.enrollment_code }}</code>
        <button class="btn sm" type="button" @click="copyEnrollmentCode">复制</button>
      </div>
    </form>

    <div v-if="store.nodes.length" class="card table-card">
      <table class="table">
        <thead>
          <tr>
            <th>Node ID</th>
            <th>地址</th>
            <th>角色</th>
            <th>父 Node</th>
            <th>状态</th>
            <th>标签</th>
            <th style="text-align: right">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="node in store.nodes" :key="node.node_id">
            <td class="mono">{{ node.node_id }}</td>
            <td class="mono">{{ node.host_ip || "loopback" }}</td>
            <td>{{ node.role }}</td>
            <td class="mono">{{ node.parent_node_id || "—" }}</td>
            <td><StatusChip :status="node.status" /></td>
            <td class="mono labels">{{ JSON.stringify(node.labels) }}</td>
            <td>
              <div class="row-actions">
                <button
                  v-if="store.supportsAction('node.health')"
                  class="btn sm"
                  :disabled="!!busy"
                  @click="inspectHealth(node)"
                >健康</button>
                <button
                  v-if="store.supportsAction('node.drain')"
                  class="btn sm"
                  :disabled="!!busy || node.status.toUpperCase() === 'DRAINED'"
                  @click="drain(node)"
                >Drain</button>
                <button
                  v-if="store.supportsAction('node.revoke') && node.node_id !== 'desktop-local'"
                  class="btn danger sm"
                  :disabled="!!busy"
                  @click="revokeCertificates(node)"
                >吊销证书</button>
                <button
                  v-if="store.supportsAction('node.remove')"
                  class="btn danger sm"
                  :disabled="!!busy || node.status.toUpperCase() !== 'DRAINED'"
                  @click="remove(node)"
                >移除</button>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-else class="empty">没有已注册 Node。</div>

    <div v-if="health" class="card health-card">
      <div class="muted">最近一次 Node 健康响应</div>
      <pre class="mono">{{ JSON.stringify(health, null, 2) }}</pre>
    </div>
  </div>
</template>

<style scoped>
.nodes-body {
  flex: 1;
  overflow: auto;
  padding: 18px 22px;
}
.table-card {
  padding: 0;
  overflow: hidden;
}
.enrollment-card {
  margin-bottom: 14px;
}
.form-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(150px, 1fr));
  gap: 10px;
}
.form-grid label {
  display: flex;
  flex-direction: column;
  gap: 5px;
  color: var(--muted);
  font-size: 12px;
}
.form-actions,
.issued-code {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 12px;
}
.issued-code {
  padding: 10px;
  border: 1px solid var(--border);
  border-radius: 8px;
  flex-wrap: wrap;
}
.issued-code code {
  flex: 1;
  min-width: 280px;
  overflow-wrap: anywhere;
}
.labels {
  max-width: 300px;
  overflow: hidden;
  text-overflow: ellipsis;
}
.row-actions {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
}
.health-card {
  margin-top: 14px;
}
.health-card pre {
  max-height: 280px;
  overflow: auto;
  white-space: pre-wrap;
}
</style>
