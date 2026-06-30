<script setup lang="ts">
import { computed, h, onBeforeUnmount, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import {
  NButton,
  NDataTable,
  NSpace,
  NSwitch,
  NTag,
  NText,
  useMessage,
  type DataTableColumns,
} from 'naive-ui'

import {
  drainWorker,
  getAdminQueue,
  getAdminTasks,
  getAdminWorkers,
  requeueSubmission,
} from '../../api/judge'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosLanguageTag from '../../components/oj/OjosLanguageTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import OjosStatusTag from '../../components/oj/OjosStatusTag.vue'
import OjosWorkerStatusTag from '../../components/oj/OjosWorkerStatusTag.vue'
import type { JudgeTaskItem, QueueStatus, WorkerItem } from '../../types/worker'
import { formatDateTime, formatDuration, formatList } from '../../utils/format'

const message = useMessage()
const queue = ref<QueueStatus | null>(null)
const workers = ref<WorkerItem[]>([])
const tasks = ref<JudgeTaskItem[]>([])
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const autoRefresh = ref(true)
let timer: number | undefined

const runningTasks = computed(() => tasks.value.filter((task) => task.status === 'RUNNING').length)
const staleHint = computed(() =>
  tasks.value.filter((task) => task.status === 'RUNNING' && task.lease_expires_at).length,
)

const workerColumns = computed<DataTableColumns<WorkerItem>>(() => [
  {
    title: 'Worker',
    key: 'worker_name',
    minWidth: 220,
    render: (row) =>
      h('div', { class: 'worker-cell' }, [
        h('strong', row.worker_name || row.worker_id),
        h('span', row.worker_id),
      ]),
  },
  { title: '主机', key: 'hostname', minWidth: 150 },
  { title: '版本', key: 'version', width: 110 },
  {
    title: '状态',
    key: 'status',
    width: 120,
    render: (row) => h(OjosWorkerStatusTag, { status: row.status }),
  },
  { title: '槽位', key: 'slots', width: 100, render: (row) => `${row.running_count}/${row.max_concurrency}` },
  {
    title: '语言',
    key: 'supported_languages',
    minWidth: 220,
    render: (row) =>
      h(
        NSpace,
        { size: 6 },
        {
          default: () =>
            row.supported_languages.length
              ? row.supported_languages.map((language) =>
                  h(OjosLanguageTag, { key: language, language }),
                )
              : formatList(row.supported_languages),
        },
      ),
  },
  { title: '最近心跳', key: 'last_seen', width: 180, render: (row) => formatDateTime(row.last_seen) },
  {
    title: '操作',
    key: 'action',
    width: 110,
    render: (row) =>
      row.status === 'DRAINING'
        ? h(NTag, { size: 'small', type: 'warning' }, { default: () => '排空中' })
        : hButton('排空', () => handleDrain(row.worker_id)),
  },
])

const taskColumns = computed<DataTableColumns<JudgeTaskItem>>(() => [
  { title: '任务', key: 'task_id', minWidth: 140 },
  {
    title: '提交',
    key: 'submission_id',
    width: 120,
    render: (row) =>
      h(
        RouterLink,
        { to: `/submissions/${row.submission_id}`, class: 'table-link' },
        { default: () => row.submission_id },
      ),
  },
  { title: 'Worker', key: 'worker_id', minWidth: 160, render: (row) => row.worker_id || '-' },
  {
    title: '状态',
    key: 'status',
    width: 120,
    render: (row) => h(OjosStatusTag, { status: row.status, domain: 'task' }),
  },
  { title: '尝试', key: 'attempt', width: 90 },
  { title: '心跳', key: 'heartbeat_at', width: 180, render: (row) => formatDateTime(row.heartbeat_at) },
  {
    title: '租约到期',
    key: 'lease_expires_at',
    width: 180,
    render: (row) => formatDateTime(row.lease_expires_at),
  },
  {
    title: '操作',
    key: 'action',
    width: 110,
    render: (row) => hButton('重排队', () => handleRequeue(row.submission_id)),
  },
])

async function load(silent = false): Promise<void> {
  if (silent) {
    refreshing.value = true
  } else {
    loading.value = true
  }
  error.value = null
  try {
    const [queueResp, workersResp, tasksResp] = await Promise.all([
      getAdminQueue(),
      getAdminWorkers(),
      getAdminTasks(),
    ])
    queue.value = queueResp
    workers.value = workersResp.workers
    tasks.value = tasksResp.tasks
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
    refreshing.value = false
  }
}

async function handleDrain(workerId: string): Promise<void> {
  try {
    await drainWorker(workerId)
    message.success('Worker 已进入排空状态')
    await load(true)
  } catch (err) {
    message.error(toApiClientError(err).message)
  }
}

async function handleRequeue(submissionId: number): Promise<void> {
  try {
    await requeueSubmission(submissionId)
    message.success('提交已重新入队')
    await load(true)
  } catch (err) {
    message.error(toApiClientError(err).message)
  }
}

function startTimer(): void {
  stopTimer()
  timer = window.setInterval(() => {
    if (autoRefresh.value) {
      void load(true)
    }
  }, 3000)
}

function stopTimer(): void {
  if (timer) {
    window.clearInterval(timer)
    timer = undefined
  }
}

function hButton(label: string, onClick: () => void) {
  return h(NButton, { size: 'small', secondary: true, onClick }, { default: () => label })
}

onMounted(() => {
  void load()
  startTimer()
})

onBeforeUnmount(stopTimer)
</script>

<template>
  <div class="admin-judge-page">
    <OjosPageHeader
      title="评测集群"
      description="队列信号、PostgreSQL 任务租约、Worker 和重排队控制的运维视图。"
      eyebrow="管理"
    >
      <template #actions>
        <NSpace align="center">
          <NText depth="3">自动刷新</NText>
          <NSwitch v-model:value="autoRefresh" />
          <NButton :loading="refreshing" secondary @click="load(true)">刷新</NButton>
        </NSpace>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />

      <template v-else>
        <div class="judge-summary-grid">
          <OjosStatCard label="Stream 长度" :value="queue?.stream_length ?? 0" tone="primary" />
          <OjosStatCard label="Redis 等待" :value="queue?.pending_count ?? 0" />
          <OjosStatCard label="已调度" :value="queue?.scheduled ?? 0" />
          <OjosStatCard label="评测中" :value="queue?.judging ?? 0" tone="success" />
          <OjosStatCard label="运行任务" :value="runningTasks" />
          <OjosStatCard label="租约行" :value="staleHint" />
        </div>

        <OjosSection
          title="队列"
          description="Redis Streams 是信号历史；PostgreSQL judge_tasks 是任务所有权事实源。"
        >
          <div class="queue-detail-grid">
            <span>Consumer group</span>
            <strong>{{ queue?.consumer_group || '-' }}</strong>
            <span>最后 stream id</span>
            <strong>{{ queue?.last_id || '-' }}</strong>
            <span>裁剪策略</span>
            <strong>{{ queue?.trim_strategy || '-' }}</strong>
            <span>最久等待空闲</span>
            <strong>{{ queue?.pending_oldest_idle_ms ? formatDuration(queue.pending_oldest_idle_ms) : '-' }}</strong>
          </div>
        </OjosSection>

        <OjosSection title="Worker" description="已注册 Worker、心跳、语言和并发槽位。">
          <EmptyView v-if="workers.length === 0" description="暂无注册 Worker" />
          <NDataTable
            v-else
            :columns="workerColumns"
            :data="workers"
            :pagination="{ pageSize: 8 }"
            :bordered="false"
          />
        </OjosSection>

        <OjosSection title="任务" description="当前任务行、Worker 租约和重排队控制。">
          <EmptyView v-if="tasks.length === 0" description="暂无评测任务" />
          <NDataTable
            v-else
            :columns="taskColumns"
            :data="tasks"
            :pagination="{ pageSize: 10 }"
            :bordered="false"
          />
        </OjosSection>
      </template>
    </template>
  </div>
</template>

<style scoped>
.admin-judge-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.judge-summary-grid {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  gap: 12px;
}

.queue-detail-grid {
  display: grid;
  grid-template-columns: 170px minmax(0, 1fr);
  gap: 8px 14px;
}

.queue-detail-grid span {
  color: var(--muted);
}

.queue-detail-grid strong {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}

:deep(.worker-cell) {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

:deep(.worker-cell strong) {
  color: var(--text-strong);
}

:deep(.worker-cell span) {
  color: var(--muted);
  font-size: 12px;
}

@media (max-width: 1200px) {
  .judge-summary-grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
}

@media (max-width: 760px) {
  .judge-summary-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
