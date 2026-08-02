<script setup lang="ts">
import { computed, ref } from "vue";
import PageHeader from "../components/PageHeader.vue";
import Modal from "../components/Modal.vue";
import StatusChip from "../components/StatusChip.vue";
import OperationLogs from "../components/OperationLogs.vue";
import { api } from "../api";
import { useOrchestrator } from "../store";
import type { OperationRow } from "../types";

const store = useOrchestrator();
const selected = ref<OperationRow | null>(null);
const busy = ref(false);
const filter = ref("");
const executeDriver = ref(false);

const runtimeDriverActions = new Set([
  "release.install",
  "release.rollback",
  "host.start",
  "host.stop",
  "service.start",
  "service.stop",
  "service.restart",
  "service.delete",
  "service.enable",
  "service.disable",
]);

const filtered = computed(() => {
  const keyword = filter.value.trim().toLowerCase();
  const operations = [...store.operations];
  if (!keyword) return operations;
  return operations.filter(
    (operation) =>
      operation.operation_id.toLowerCase().includes(keyword) ||
      operation.action.toLowerCase().includes(keyword) ||
      operation.target.toLowerCase().includes(keyword) ||
      operation.status.toLowerCase().includes(keyword),
  );
});

const selectedLive = computed(() =>
  ["RUNNING", "PLANNED", "AWAITING_CONFIRMATION"].includes(
    selected.value?.status ?? "",
  ),
);
const selectedCanUseDriver = computed(() =>
  runtimeDriverActions.has(selected.value?.action ?? ""),
);
const selectedCanConfirm = computed(
  () =>
    selected.value?.status === "PLANNED" &&
    selected.value.requires_confirmation,
);
const selectedCanApply = computed(
  () =>
    selected.value?.status === "AWAITING_CONFIRMATION" ||
    (selected.value?.status === "PLANNED" &&
      !selected.value.requires_confirmation),
);

function selectOperation(operation: OperationRow) {
  selected.value = operation;
  executeDriver.value = false;
}

function closeSelected() {
  selected.value = null;
  executeDriver.value = false;
}

function driverRequired(kind: "apply" | "rollback") {
  const operation = selected.value;
  if (!operation || !runtimeDriverActions.has(operation.action)) return false;
  if (operation.action === "release.install") {
    return operation.driver_authorized;
  }
  return true;
}

async function operationAction(
  kind: "confirm" | "apply" | "rollback",
  operation: OperationRow,
) {
  if (
    (kind === "apply" || kind === "rollback") &&
    driverRequired(kind) &&
    !executeDriver.value
  ) {
    store.toast("err", "请先授权执行运行时驱动");
    return;
  }
  busy.value = true;
  try {
    if (kind === "confirm") await api.operationConfirm(operation.operation_id);
    if (kind === "apply") {
      await api.operationApply(
        operation.operation_id,
        executeDriver.value ? { execute_service_driver: "true" } : {},
      );
    }
    if (kind === "rollback") {
      await api.operationRollback(
        operation.operation_id,
        executeDriver.value ? { execute_service_driver: "true" } : {},
      );
    }
    store.toast("ok", `${kind} 已执行`);
    await store.refreshCore();
    const updated = store.operations.find(
      (item) => item.operation_id === operation.operation_id,
    );
    if (updated) selected.value = updated;
    if (kind === "apply" || kind === "rollback") executeDriver.value = false;
  } catch (err) {
    store.toast("err", `${kind} 失败：${(err as Error).message}`);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <PageHeader title="操作" subtitle="编排动作的计划、确认、执行与回滚记录">
    <input
      class="input"
      style="width: 220px"
      v-model="filter"
      placeholder="筛选：动作 / 目标 / 状态…"
    />
    <button class="btn sm" @click="store.refreshCore()">刷新</button>
  </PageHeader>

  <div class="operations-body">
    <div class="card" v-if="filtered.length" style="padding: 0; overflow: hidden">
      <table class="table">
        <thead>
          <tr>
            <th>操作 ID</th>
            <th>动作</th>
            <th>目标</th>
            <th>状态</th>
            <th>风险</th>
            <th>日志</th>
            <th>更新时间</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="operation in filtered"
            :key="operation.operation_id"
            class="clickable"
            @click="selectOperation(operation)"
          >
            <td class="mono" style="max-width: 240px; overflow: hidden; text-overflow: ellipsis">
              {{ operation.operation_id }}
            </td>
            <td class="mono">{{ operation.action }}</td>
            <td style="max-width: 220px; overflow: hidden; text-overflow: ellipsis">
              {{ operation.target }}
            </td>
            <td><StatusChip :status="operation.status" /></td>
            <td>
              <span
                class="chip"
                :class="operation.risk === 'HIGH' ? 'err' : operation.risk === 'MEDIUM' ? 'warn' : ''"
              >
                {{ operation.risk || "—" }}
              </span>
            </td>
            <td>{{ operation.log_count }}</td>
            <td class="muted" style="font-size: 12px">{{ operation.updated_at }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-else class="empty">
      <span class="icon">≡</span>
      <span>暂无操作记录。</span>
    </div>
  </div>

  <Modal
    :open="!!selected"
    :title="`操作 ${selected?.operation_id ?? ''}`"
    width="640px"
    @close="closeSelected"
  >
    <template v-if="selected">
      <div class="op-meta">
        <div class="kv"><span>动作</span><span class="mono">{{ selected.action }}</span></div>
        <div class="kv"><span>目标</span><span>{{ selected.target }}</span></div>
        <div class="kv">
          <span>状态</span><span><StatusChip :status="selected.status" /></span>
        </div>
        <div class="kv"><span>摘要</span><span>{{ selected.summary || "—" }}</span></div>
        <div class="kv" v-if="selected.error">
          <span>错误</span><span class="err-text">{{ selected.error }}</span>
        </div>
      </div>

      <label
        v-if="
          selectedCanUseDriver &&
          ['PLANNED', 'AWAITING_CONFIRMATION', 'SUCCEEDED', 'FAILED'].includes(
            selected.status,
          )
        "
        class="driver-authorization"
      >
        <input v-model="executeDriver" type="checkbox" :disabled="busy" />
        <span>
          <strong>授权执行运行时驱动</strong>
          <small>
            release.install 可不勾选，作为登记或延后启动；其余运行时动作以及
            已用驱动执行过的安装，在执行或回滚前都要单独授权。
          </small>
        </span>
      </label>

      <div class="op-actions">
        <button
          v-if="selectedCanConfirm"
          class="btn sm"
          :disabled="busy"
          @click="operationAction('confirm', selected)"
        >
          确认
        </button>
        <button
          v-if="selectedCanApply"
          class="btn primary sm"
          :disabled="busy || (driverRequired('apply') && !executeDriver)"
          :title="
            driverRequired('apply') && !executeDriver
              ? '请先授权执行运行时驱动'
              : ''
          "
          @click="operationAction('apply', selected)"
        >
          执行
        </button>
        <button
          v-if="
            selected.rollback_available &&
            ['SUCCEEDED', 'FAILED'].includes(selected.status)
          "
          class="btn danger sm"
          :disabled="busy || (driverRequired('rollback') && !executeDriver)"
          :title="
            driverRequired('rollback') && !executeDriver
              ? '请先授权执行运行时驱动'
              : ''
          "
          @click="operationAction('rollback', selected)"
        >
          回滚
        </button>
      </div>

      <p class="muted" style="margin: 12px 0 6px">日志</p>
      <OperationLogs :operation-id="selected.operation_id" :live="selectedLive" />
    </template>
  </Modal>
</template>

<style scoped>
.operations-body {
  flex: 1;
  overflow-y: auto;
  padding: 18px 22px;
}
.op-meta .kv {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  padding: 6px 0;
  border-bottom: 1px solid rgba(148, 163, 184, 0.07);
  font-size: 12.5px;
}
.op-meta .kv > span:first-child {
  color: var(--faint);
  flex-shrink: 0;
}
.op-meta .kv > span:last-child {
  text-align: right;
  word-break: break-all;
}
.err-text {
  color: var(--err);
}
.driver-authorization {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  margin-top: 12px;
  padding: 10px 12px;
  border: 1px solid rgba(245, 158, 11, 0.35);
  border-radius: 7px;
  cursor: pointer;
}
.driver-authorization input {
  margin-top: 3px;
  accent-color: var(--accent);
}
.driver-authorization span {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.driver-authorization strong {
  color: var(--text-strong);
  font-size: 12.5px;
}
.driver-authorization small {
  color: var(--muted);
  font-size: 11.5px;
  line-height: 1.5;
}
.op-actions {
  display: flex;
  gap: 8px;
  margin-top: 12px;
}
</style>
