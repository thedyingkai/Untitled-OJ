<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import { api } from "../api";
import type { OperationLog } from "../types";

const props = defineProps<{ operationId: string; live?: boolean }>();

const logs = ref<OperationLog[]>([]);
const error = ref("");
let timer: ReturnType<typeof setInterval> | null = null;

async function load() {
  if (!props.operationId) return;
  try {
    logs.value = await api.operationLogs(props.operationId);
    error.value = "";
  } catch (err) {
    error.value = (err as Error).message;
  }
}

function start() {
  stop();
  load();
  if (props.live) {
    timer = setInterval(load, 2000);
  }
}
function stop() {
  if (timer) {
    clearInterval(timer);
    timer = null;
  }
}

watch(() => props.operationId, start);
watch(
  () => props.live,
  (live) => (live ? start() : stop()),
);
onMounted(start);
onUnmounted(stop);
</script>

<template>
  <div class="logs mono">
    <div v-if="error" class="log-line err">{{ error }}</div>
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
</style>
