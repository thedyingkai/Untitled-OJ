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
const planOpen = ref(false);
const planning = ref(false);
const planDocument = ref(`{
  "action": "deployment.restart",
  "fields": {
    "deployment_id": "deployment-id"
  }
}`);

// v1 plans persist their exact runtime payload before confirmation. The UI no
// longer sends a second legacy "execute driver" switch that could change it.
const runtimeDriverActions = new Set<string>();

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
  ["RUNNING", "PLANNED", "CONFIRMED", "ENQUEUING", "CANCELLING"].includes(
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
    selected.value?.status === "CONFIRMED" ||
    (selected.value?.status === "PLANNED" && !selected.value.requires_confirmation),
);
const selectedCanCancel = computed(() =>
  selected.value
    ? ["PLANNED", "CONFIRMED", "ENQUEUING", "RUNNING"].includes(
        selected.value.status,
      )
    : false,
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
  kind: "confirm" | "apply" | "cancel" | "retry" | "rollback",
  operation: OperationRow,
) {
  const capability = `operation.${kind}`;
  if (!store.ensureAction(capability)) return;
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
    if (kind === "cancel") await api.operationCancel(operation.operation_id);
    if (kind === "retry") await api.operationRetry(operation.operation_id);
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

async function createPlan() {
  if (!store.ensureAction("operation.plan")) return;
  let document: unknown;
  try {
    document = JSON.parse(planDocument.value);
  } catch (error) {
    store.toast("err", `计划 JSON 无效：${(error as Error).message}`);
    return;
  }
  if (!document || typeof document !== "object" || Array.isArray(document)) {
    store.toast("err", "计划必须是包含 action 和 fields 的 JSON 对象");
    return;
  }
  const action = (document as Record<string, unknown>).action;
  const fields = (document as Record<string, unknown>).fields;
  if (
    typeof action !== "string" ||
    !action.trim() ||
    !fields ||
    typeof fields !== "object" ||
    Array.isArray(fields)
  ) {
    store.toast("err", "计划必须包含非空 action 和对象类型 fields");
    return;
  }
  planning.value = true;
  try {
    const operation = await api.operationPlan(document as Record<string, unknown>);
    planOpen.value = false;
    await store.refreshCore(true);
    selected.value =
      store.operations.find((item) => item.operation_id === operation.operation_id) ??
      operation;
    store.toast("ok", `计划已创建：${operation.operation_id}`);
  } catch (error) {
    store.toast("err", `创建计划失败：${(error as Error).message}`);
  } finally {
    planning.value = false;
  }
}
</script>

<template>
  <PageHeader title="操作" subtitle="编排动作的计划、确认、执行与回滚记录">
    <button
      class="btn sm"
      data-action="operation.plan"
      :disabled="!store.supportsAction('operation.plan')"
      @click="planOpen = true"
    >
      新建计划
    </button>
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

  <Modal :open="planOpen" title="新建 Operation 计划" width="680px" @close="planOpen = false">
    <div class="field">
      <label>ActionRequest JSON</label>
      <textarea
        v-model="planDocument"
        class="input mono plan-document"
        spellcheck="false"
        aria-label="Operation plan JSON"
      ></textarea>
      <span class="hint">服务端负责校验 action、固定执行计划和权限；创建成功返回不可变的 PLANNED Operation。</span>
    </div>
    <template #footer>
      <button class="btn" :disabled="planning" @click="planOpen = false">取消</button>
      <button
        class="btn primary"
        data-action="operation.plan"
        :disabled="planning || !store.supportsAction('operation.plan')"
        @click="createPlan"
      >
        {{ planning ? "创建中…" : "创建计划" }}
      </button>
    </template>
  </Modal>

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
          v-if="selectedCanConfirm && store.supportsAction('operation.confirm')"
          class="btn sm"
          :disabled="busy"
          @click="operationAction('confirm', selected)"
        >
          确认
        </button>
        <button
          v-if="selectedCanApply && store.supportsAction('operation.apply')"
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
          v-if="selectedCanCancel && store.supportsAction('operation.cancel')"
          class="btn sm"
          :disabled="busy"
          @click="operationAction('cancel', selected)"
        >
          取消
        </button>
        <button
          v-if="selected.status === 'FAILED' && store.supportsAction('operation.retry')"
          class="btn sm"
          :disabled="busy"
          @click="operationAction('retry', selected)"
        >
          重试
        </button>
        <button
          v-if="
            selected.rollback_available &&
            store.supportsAction('operation.rollback') &&
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
.plan-document {
  min-height: 260px;
  resize: vertical;
  line-height: 1.5;
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
