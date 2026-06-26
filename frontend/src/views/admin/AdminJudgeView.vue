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
  { title: 'Host', key: 'hostname', minWidth: 150 },
  { title: 'Version', key: 'version', width: 110 },
  {
    title: 'Status',
    key: 'status',
    width: 120,
    render: (row) => h(OjosWorkerStatusTag, { status: row.status }),
  },
  { title: 'Slots', key: 'slots', width: 100, render: (row) => `${row.running_count}/${row.max_concurrency}` },
  {
    title: 'Languages',
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
  { title: 'Last Seen', key: 'last_seen', width: 180, render: (row) => formatDateTime(row.last_seen) },
  {
    title: 'Action',
    key: 'action',
    width: 110,
    render: (row) =>
      row.status === 'DRAINING'
        ? h(NTag, { size: 'small', type: 'warning' }, { default: () => 'Draining' })
        : hButton('Drain', () => handleDrain(row.worker_id)),
  },
])

const taskColumns = computed<DataTableColumns<JudgeTaskItem>>(() => [
  { title: 'Task', key: 'task_id', minWidth: 140 },
  {
    title: 'Submission',
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
    title: 'Status',
    key: 'status',
    width: 120,
    render: (row) => h(OjosStatusTag, { status: row.status, domain: 'task' }),
  },
  { title: 'Attempt', key: 'attempt', width: 90 },
  { title: 'Heartbeat', key: 'heartbeat_at', width: 180, render: (row) => formatDateTime(row.heartbeat_at) },
  {
    title: 'Lease Expires',
    key: 'lease_expires_at',
    width: 180,
    render: (row) => formatDateTime(row.lease_expires_at),
  },
  {
    title: 'Action',
    key: 'action',
    width: 110,
    render: (row) => hButton('Requeue', () => handleRequeue(row.submission_id)),
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
    message.success('Worker set to draining')
    await load(true)
  } catch (err) {
    message.error(toApiClientError(err).message)
  }
}

async function handleRequeue(submissionId: number): Promise<void> {
  try {
    await requeueSubmission(submissionId)
    message.success('Submission requeued')
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
      title="Judge Cluster"
      description="Operational view of queue signals, PostgreSQL task leases, workers, and requeue controls."
      eyebrow="Admin"
    >
      <template #actions>
        <NSpace align="center">
          <NText depth="3">Auto refresh</NText>
          <NSwitch v-model:value="autoRefresh" />
          <NButton :loading="refreshing" secondary @click="load(true)">Refresh</NButton>
        </NSpace>
      </template>
    </OjosPageHeader>

    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />

      <template v-else>
        <div class="judge-summary-grid">
          <OjosStatCard label="Stream Length" :value="queue?.stream_length ?? 0" tone="primary" />
          <OjosStatCard label="Redis Pending" :value="queue?.pending_count ?? 0" />
          <OjosStatCard label="Scheduled" :value="queue?.scheduled ?? 0" />
          <OjosStatCard label="Judging" :value="queue?.judging ?? 0" tone="success" />
          <OjosStatCard label="Running Tasks" :value="runningTasks" />
          <OjosStatCard label="Lease Rows" :value="staleHint" />
        </div>

        <OjosSection
          title="Queue"
          description="Redis Streams are signal history; PostgreSQL judge_tasks is the task ownership source."
        >
          <div class="queue-detail-grid">
            <span>Consumer group</span>
            <strong>{{ queue?.consumer_group || '-' }}</strong>
            <span>Last stream id</span>
            <strong>{{ queue?.last_id || '-' }}</strong>
            <span>Trim strategy</span>
            <strong>{{ queue?.trim_strategy || '-' }}</strong>
            <span>Oldest pending idle</span>
            <strong>{{ queue?.pending_oldest_idle_ms ? formatDuration(queue.pending_oldest_idle_ms) : '-' }}</strong>
          </div>
        </OjosSection>

        <OjosSection title="Workers" description="Registered workers, heartbeats, languages, and concurrency slots.">
          <EmptyView v-if="workers.length === 0" description="No workers registered" />
          <NDataTable
            v-else
            :columns="workerColumns"
            :data="workers"
            :pagination="{ pageSize: 8 }"
            :bordered="false"
          />
        </OjosSection>

        <OjosSection title="Tasks" description="Current task rows with worker leases and requeue controls.">
          <EmptyView v-if="tasks.length === 0" description="No judge tasks" />
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
