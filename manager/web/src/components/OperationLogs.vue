<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import {
  api,
  isRequestCancelled,
  MAX_OPERATION_LOGS,
  normalizeOperationLog,
} from "../api";
import type { OperationLog } from "../types";

const props = defineProps<{ operationId: string; live?: boolean }>();

const logs = ref<OperationLog[]>([]);
const error = ref("");
const loading = ref(false);
let timer: ReturnType<typeof setTimeout> | null = null;
let controller: AbortController | null = null;
let generation = 0;
let lastEventId = "";
let retryMs = 1000;
const seenEvents = new Set<string>();

async function load(currentGeneration: number) {
  if (!props.operationId || currentGeneration !== generation) return;
  controller?.abort("superseded");
  const requestController = new AbortController();
  controller = requestController;
  loading.value = true;
  try {
    if (props.live) {
      const batch = await api.operationEvents(props.operationId, lastEventId, {
        signal: requestController.signal,
      });
      if (currentGeneration !== generation) return;
      lastEventId = batch.lastEventId;
      retryMs = batch.retryMs;
      const next = batch.events
        .filter((event) => event.event === "job")
        .map((event) => event.data.event)
        .filter((event): event is Record<string, unknown> => {
          if (!event || typeof event !== "object" || Array.isArray(event)) return false;
          const record = event as Record<string, unknown>;
          const key = `${String(record.job_id ?? "")}:${String(record.sequence ?? "")}`;
          if (seenEvents.has(key)) return false;
          seenEvents.add(key);
          return true;
        })
        .map(normalizeOperationLog);
      logs.value = [...logs.value, ...next].slice(-MAX_OPERATION_LOGS);
      while (seenEvents.size > MAX_OPERATION_LOGS * 2) {
        const oldest = seenEvents.values().next().value;
        if (oldest === undefined) break;
        seenEvents.delete(oldest);
      }
    } else {
      const next = await api.operationLogs(props.operationId, {
        signal: requestController.signal,
      });
      if (currentGeneration !== generation) return;
      logs.value = next.slice(-MAX_OPERATION_LOGS);
    }
    error.value = "";
  } catch (err) {
    if (currentGeneration !== generation || isRequestCancelled(err)) return;
    error.value = (err as Error).message;
  } finally {
    if (controller === requestController) controller = null;
    if (currentGeneration !== generation) return;
    loading.value = false;
    // 递归 timeout 保证上一轮结束后才开始下一轮，不会在 daemon 变慢时堆积请求。
    if (props.live) {
      timer = setTimeout(() => void load(currentGeneration), retryMs);
    }
  }
}

function start() {
  stop();
  logs.value = [];
  error.value = "";
  lastEventId = "";
  retryMs = 1000;
  seenEvents.clear();
  const currentGeneration = generation;
  void load(currentGeneration);
}
function stop() {
  generation += 1;
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  controller?.abort("operation event stream stopped");
  controller = null;
  loading.value = false;
}

watch(() => [props.operationId, props.live], start);
onMounted(start);
onUnmounted(stop);
</script>

<template>
  <div class="logs mono">
    <div v-if="error" class="log-line err">{{ error }}</div>
    <div v-else-if="loading && !logs.length" class="log-line muted">日志加载中…</div>
    <div v-else-if="!logs.length" class="log-line muted">暂无日志</div>
    <div
      v-for="(log, index) in logs"
      :key="index"
      class="log-line"
      :class="log.level"
    >
      <span class="step">[{{ log.step_id }}]</span>
      <span>{{ log.message }}</span>
    </div>
    <div v-if="logs.length >= MAX_OPERATION_LOGS" class="log-line muted buffer-note">
      仅保留最新 {{ MAX_OPERATION_LOGS }} 条日志
    </div>
  </div>
</template>

<style scoped>
.logs {
  background: #070b12;
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 10px 12px;
  max-height: 300px;
  overflow-y: auto;
  font-size: 11.5px;
  line-height: 1.7;
}
.log-line {
  white-space: pre-wrap;
  word-break: break-all;
  color: #a8b3c5;
}
.log-line .step {
  color: var(--accent-2);
  margin-right: 6px;
}
.log-line.error {
  color: var(--err);
}
.log-line.warn {
  color: var(--warn);
}
.log-line.err {
  color: var(--err);
}
.buffer-note {
  margin-top: 4px;
  border-top: 1px solid var(--border);
  padding-top: 4px;
}
</style>
