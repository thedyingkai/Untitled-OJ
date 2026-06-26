<script setup lang="ts">
import { computed, h, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  NButton,
  NDataTable,
  NGrid,
  NGridItem,
  NSpace,
  NSwitch,
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
import PageCard from '../../components/common/PageCard.vue'
import StatusTag from '../../components/common/StatusTag.vue'
import TimeText from '../../components/common/TimeText.vue'
import type { JudgeTaskItem, QueueStatus, WorkerItem } from '../../types/worker'

const message = useMessage()
const queue = ref<QueueStatus | null>(null)
const workers = ref<WorkerItem[]>([])
const tasks = ref<JudgeTaskItem[]>([])
const loading = ref(true)
const refreshing = ref(false)
const error = ref<ApiClientError | null>(null)
const autoRefresh = ref(true)
let timer: number | undefined

const workerColumns = computed<DataTableColumns<WorkerItem>>(() => [
  { title: 'Worker', key: 'worker_name', render: (row) => row.worker_name || row.worker_id },
  { title: 'Host', key: 'hostname' },
  { title: 'Version', key: 'version' },
  { title: 'Status', key: 'status', render: (row) => hStatus(row.status) },
  { title: 'Slots', key: 'slots', render: (row) => `${row.running_count}/${row.max_concurrency}` },
  {
    title: 'Languages',
    key: 'supported_languages',
    render: (row) => row.supported_languages.join(', '),
  },
  { title: 'Last Seen', key: 'last_seen', render: (row) => hTime(row.last_seen) },
  {
    title: 'Action',
    key: 'action',
    render: (row) =>
      row.status === 'DRAINING'
        ? 'Draining'
        : hButton('Drain', () => handleDrain(row.worker_id)),
  },
])

const taskColumns = computed<DataTableColumns<JudgeTaskItem>>(() => [
  { title: 'Task', key: 'task_id' },
  { title: 'Submission', key: 'submission_id' },
  { title: 'Worker', key: 'worker_id' },
  { title: 'Status', key: 'status', render: (row) => hStatus(row.status) },
  { title: 'Attempt', key: 'attempt' },
  { title: 'Heartbeat', key: 'heartbeat_at', render: (row) => hTime(row.heartbeat_at) },
  { title: 'Lease Expires', key: 'lease_expires_at', render: (row) => hTime(row.lease_expires_at) },
  {
    title: 'Action',
    key: 'action',
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

function hStatus(status: string) {
  return h(StatusTag, { status })
}

function hTime(value?: string) {
  return value ? h(TimeText, { value }) : '-'
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
  <PageCard title="Judge Cluster">
    <LoadingView v-if="loading" />
    <template v-else>
      <ApiErrorAlert v-if="error" :error="error" @retry="load()" />

      <NSpace vertical size="large">
        <NSpace justify="end" align="center">
          <NText depth="3">Auto refresh</NText>
          <NSwitch v-model:value="autoRefresh" />
          <NButton :loading="refreshing" secondary @click="load(true)">Refresh</NButton>
        </NSpace>

        <NGrid class="metric-grid" :cols="4" :x-gap="12" :y-gap="12" responsive="screen">
          <NGridItem class="metric">
            <strong>{{ queue?.stream_length ?? 0 }}</strong>
            <span>Stream Length</span>
          </NGridItem>
          <NGridItem class="metric">
            <strong>{{ queue?.pending_count ?? 0 }}</strong>
            <span>Redis Signal Pending</span>
          </NGridItem>
          <NGridItem class="metric">
            <strong>{{ queue?.scheduled ?? 0 }}</strong>
            <span>Scheduled</span>
          </NGridItem>
          <NGridItem class="metric">
            <strong>{{ queue?.judging ?? 0 }}</strong>
            <span>Judging</span>
          </NGridItem>
        </NGrid>
        <NText depth="3">
          Task ownership is tracked in PostgreSQL leases; Redis Streams are trimmed signal history.
        </NText>

        <section>
          <h2>Workers</h2>
          <EmptyView v-if="workers.length === 0" description="No workers registered" />
          <NDataTable
            v-else
            :columns="workerColumns"
            :data="workers"
            :pagination="{ pageSize: 8 }"
          />
        </section>

        <section>
          <h2>Tasks</h2>
          <EmptyView v-if="tasks.length === 0" description="No judge tasks" />
          <NDataTable
            v-else
            :columns="taskColumns"
            :data="tasks"
            :pagination="{ pageSize: 10 }"
          />
        </section>
      </NSpace>
    </template>
  </PageCard>
</template>

<style scoped>
.metric-grid {
  margin-bottom: 4px;
}

.metric {
  border: 1px solid #e5e7eb;
  border-radius: 8px;
  padding: 14px;
  background: #fff;
}

.metric strong {
  display: block;
  font-size: 24px;
  line-height: 1.2;
}

.metric span {
  color: #667085;
  font-size: 13px;
}

section h2 {
  margin: 0 0 12px;
  font-size: 16px;
}
</style>
