<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import Modal from "../components/Modal.vue";
import PageHeader from "../components/PageHeader.vue";
import StatusChip from "../components/StatusChip.vue";
import { api } from "../api";
import { useOrchestrator } from "../store";

type DiagnosticRow = Record<string, unknown>;

const store = useOrchestrator();
const diagnostics = ref<DiagnosticRow[]>([]);
const loading = ref(false);
const creating = ref(false);
const opening = ref("");
const exporting = ref("");
const selected = ref<DiagnosticRow | null>(null);

const hasListCapability = computed(() => store.supportsAction("diagnostic.list"));

function text(row: DiagnosticRow, field: string, fallback = ""): string {
  const value = row[field];
  return typeof value === "string" ? value : fallback;
}

function reportId(row: DiagnosticRow): string {
  return text(row, "report_id", text(row, "diagnostic_id"));
}

function unwrapReport(value: DiagnosticRow): DiagnosticRow {
  const nested = value.diagnostic_report;
  return nested && typeof nested === "object" && !Array.isArray(nested)
    ? (nested as DiagnosticRow)
    : value;
}

async function loadDiagnostics() {
  if (!store.ensureAction("diagnostic.list")) return;
  loading.value = true;
  try {
    diagnostics.value = (await api.diagnostics()).map(unwrapReport);
  } catch (error) {
    store.toast("err", `诊断列表加载失败：${(error as Error).message}`);
  } finally {
    loading.value = false;
  }
}

async function createDiagnostic() {
  if (!store.ensureAction("diagnostic.create")) return;
  creating.value = true;
  try {
    await api.createDiagnostic();
    await loadDiagnostics();
    store.toast("ok", "已创建当前 Topology 的诊断报告");
  } catch (error) {
    store.toast("err", `创建诊断失败：${(error as Error).message}`);
  } finally {
    creating.value = false;
  }
}

async function openDiagnostic(row: DiagnosticRow) {
  if (!store.ensureAction("diagnostic.get")) return;
  const id = reportId(row);
  if (!id) return;
  opening.value = id;
  try {
    selected.value = unwrapReport(await api.diagnostic(id));
  } catch (error) {
    store.toast("err", `读取诊断失败：${(error as Error).message}`);
  } finally {
    opening.value = "";
  }
}

async function exportDiagnostic(row: DiagnosticRow, format: "json" | "md") {
  if (!store.ensureAction("diagnostic.export")) return;
  const id = reportId(row);
  if (!id) return;
  exporting.value = `${id}:${format}`;
  try {
    const result = await api.exportDiagnostic(id, format);
    const rawContent = result.content;
    const content =
      typeof rawContent === "string"
        ? rawContent
        : JSON.stringify(rawContent ?? result, null, 2);
    const blob = new Blob([content], {
      type: format === "json" ? "application/json" : "text/markdown",
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = `${id}.${format}`;
    link.click();
    URL.revokeObjectURL(url);
    store.toast("ok", `诊断 ${id}.${format} 已导出`);
  } catch (error) {
    store.toast("err", `导出诊断失败：${(error as Error).message}`);
  } finally {
    exporting.value = "";
  }
}

onMounted(loadDiagnostics);
</script>

<template>
  <PageHeader title="诊断" subtitle="创建、查看并导出不可变的编排器诊断报告">
    <button
      class="btn sm"
      data-action="diagnostic.list"
      :disabled="loading || !hasListCapability"
      @click="loadDiagnostics"
    >
      {{ loading ? "刷新中…" : "刷新" }}
    </button>
  </PageHeader>

  <div class="diagnostics-body">
    <form
      v-if="store.supportsAction('diagnostic.create')"
      class="card create-form"
      data-action="diagnostic.create"
      @submit.prevent="createDiagnostic"
    >
      <p>采集当前已应用 Topology、Endpoint/Link 健康、失败 Operation 和受限日志摘要。</p>
      <button class="btn primary" type="submit" :disabled="creating">
        {{ creating ? "创建中…" : "创建诊断" }}
      </button>
    </form>

    <div v-if="loading && !diagnostics.length" class="empty">正在加载诊断报告…</div>
    <div v-else-if="diagnostics.length" class="card table-card">
      <table class="table">
        <thead>
          <tr>
            <th>报告 ID</th>
            <th>Operation</th>
            <th>状态</th>
            <th>摘要</th>
            <th>创建时间</th>
            <th>操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in diagnostics" :key="reportId(row)">
            <td class="mono">{{ reportId(row) }}</td>
            <td class="mono">{{ text(row, "operation_id", "—") }}</td>
            <td><StatusChip :status="text(row, 'status', 'UNKNOWN')" /></td>
            <td>{{ text(row, "summary", "—") }}</td>
            <td class="muted">{{ text(row, "created_at", "—") }}</td>
            <td class="row-actions">
              <button
                v-if="store.supportsAction('diagnostic.get')"
                class="btn sm"
                data-action="diagnostic.get"
                :disabled="opening === reportId(row)"
                @click="openDiagnostic(row)"
              >
                {{ opening === reportId(row) ? "读取中…" : "查看" }}
              </button>
              <button
                v-if="store.supportsAction('diagnostic.export')"
                class="btn sm"
                data-action="diagnostic.export"
                :disabled="!!exporting"
                @click="exportDiagnostic(row, 'json')"
              >
                导出 JSON
              </button>
              <button
                v-if="store.supportsAction('diagnostic.export')"
                class="btn sm"
                :disabled="!!exporting"
                @click="exportDiagnostic(row, 'md')"
              >
                导出 Markdown
              </button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-else class="empty">暂无诊断报告。</div>
  </div>

  <Modal
    :open="!!selected"
    :title="`诊断 ${selected ? reportId(selected) : ''}`"
    width="760px"
    @close="selected = null"
  >
    <pre v-if="selected" class="diagnostic-json">{{ JSON.stringify(selected, null, 2) }}</pre>
    <template #footer>
      <button class="btn" @click="selected = null">关闭</button>
      <button
        v-if="selected && store.supportsAction('diagnostic.export')"
        class="btn"
        @click="exportDiagnostic(selected, 'json')"
      >
        导出 JSON
      </button>
      <button
        v-if="selected && store.supportsAction('diagnostic.export')"
        class="btn"
        @click="exportDiagnostic(selected, 'md')"
      >
        导出 Markdown
      </button>
    </template>
  </Modal>
</template>

<style scoped>
.diagnostics-body {
  flex: 1;
  overflow: auto;
  padding: 18px 22px;
}
.create-form {
  display: flex;
  align-items: end;
  gap: 12px;
  margin-bottom: 16px;
}
.create-form .field {
  flex: 1;
  margin: 0;
}
.table-card {
  padding: 0;
  overflow: auto;
}
.row-actions {
  display: flex;
  gap: 6px;
  white-space: nowrap;
}
.diagnostic-json {
  max-height: 520px;
  overflow: auto;
  padding: 12px;
  border: 1px solid var(--border);
  border-radius: 7px;
  background: var(--bg);
  color: var(--text);
  font-size: 12px;
  line-height: 1.5;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
</style>
