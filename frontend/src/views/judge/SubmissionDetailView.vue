<script setup lang="ts">
import { computed, h, onMounted, onUnmounted, ref } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import {
  NAlert,
  NButton,
  NDataTable,
  NDescriptions,
  NDescriptionsItem,
  NInputNumber,
  NSpace,
  NText,
  type DataTableColumns,
} from 'naive-ui'

import {
  getSubmission,
  getSubmissionCases,
  getSubmissionDebugLogs,
} from '../../api/judge'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import JsonViewer from '../../components/common/JsonViewer.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import StatusTag from '../../components/common/StatusTag.vue'
import TimeText from '../../components/common/TimeText.vue'
import { useAuthStore } from '../../stores/auth'
import type {
  JudgeStatus,
  SubmissionCaseItem,
  SubmissionDebugLogsResponse,
  SubmissionItem,
} from '../../types/judge'

const route = useRoute()
const auth = useAuthStore()

const loading = ref(false)
const debugLoading = ref(false)
const error = ref<unknown>()
const debugError = ref<unknown>()
const submission = ref<SubmissionItem | null>(null)
const cases = ref<SubmissionCaseItem[]>([])
const debugCaseNo = ref<number | null>(null)
const debugLogs = ref<SubmissionDebugLogsResponse | null>(null)
const pollTimer = ref<number | undefined>()
const pollDelay = ref(1500)

const submissionId = computed(() => Number(route.params.id))
const isTerminal = computed(() => Boolean(submission.value && isTerminalStatus(submission.value.status)))
const canDebug = computed(
  () =>
    auth.hasAnyRole(['super_admin', 'admin']) ||
    auth.hasAnyPermission(['system.admin', 'submission.view.all', 'problem.manage.data']),
)

const columns: DataTableColumns<SubmissionCaseItem> = [
  { title: 'Case', key: 'case_no', width: 90 },
  {
    title: 'Status',
    key: 'status',
    width: 170,
    render: (row) => h(StatusTag, { status: row.status }),
  },
  { title: 'Score', key: 'score', width: 90 },
  { title: 'Time', key: 'time_ms', width: 110, render: (row) => `${row.time_ms} ms` },
  { title: 'Memory', key: 'memory_kb', width: 130, render: (row) => `${row.memory_kb} KB` },
  { title: 'Message', key: 'message' },
]

async function load(showLoading = true): Promise<void> {
  if (!Number.isFinite(submissionId.value) || submissionId.value <= 0) {
    error.value = new Error('Invalid submission id')
    return
  }

  if (showLoading) {
    loading.value = true
  }
  error.value = undefined

  try {
    const [submissionData, caseData] = await Promise.all([
      getSubmission(submissionId.value),
      getSubmissionCases(submissionId.value),
    ])
    submission.value = submissionData
    cases.value = caseData.cases
    if (!debugCaseNo.value && cases.value.length > 0) {
      debugCaseNo.value = cases.value[0].case_no
    }
    pollDelay.value = 1500
    schedulePoll()
  } catch (err) {
    error.value = err
    pollDelay.value = Math.min(pollDelay.value * 2, 10000)
    schedulePoll()
  } finally {
    loading.value = false
  }
}

function schedulePoll(): void {
  clearPoll()
  if (isTerminal.value) {
    return
  }
  pollTimer.value = window.setTimeout(() => {
    void load(false)
  }, pollDelay.value)
}

function clearPoll(): void {
  if (pollTimer.value) {
    window.clearTimeout(pollTimer.value)
    pollTimer.value = undefined
  }
}

async function loadDebugLogs(): Promise<void> {
  debugLoading.value = true
  debugError.value = undefined

  try {
    debugLogs.value = await getSubmissionDebugLogs(submissionId.value, {
      case_no: debugCaseNo.value || undefined,
      max_bytes: 32768,
    })
  } catch (err) {
    debugError.value = err
  } finally {
    debugLoading.value = false
  }
}

function isTerminalStatus(status: JudgeStatus): boolean {
  return !['PENDING', 'JUDGING'].includes(status)
}

onMounted(() => {
  void load()
})

onUnmounted(() => {
  clearPoll()
})
</script>

<template>
  <NSpace vertical size="large">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !submission" />
    <EmptyView v-else-if="!loading && !error && !submission" description="Submission not found" />

    <template v-if="submission">
      <PageCard :title="`Submission #${submission.id}`">
        <template #headerExtra>
          <NSpace>
            <RouterLink to="/submissions">
              <NButton secondary>Back</NButton>
            </RouterLink>
            <NButton secondary :loading="loading" @click="() => load()">Refresh</NButton>
          </NSpace>
        </template>

        <NDescriptions bordered :column="2" label-placement="left">
          <NDescriptionsItem label="Status">
            <StatusTag :status="submission.status" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Language">{{ submission.language }}</NDescriptionsItem>
          <NDescriptionsItem label="Problem">
            <RouterLink :to="`/problems/${submission.problem_id}`" class="table-link">
              {{ submission.problem_id }}
            </RouterLink>
          </NDescriptionsItem>
          <NDescriptionsItem label="User">{{ submission.user_id }}</NDescriptionsItem>
          <NDescriptionsItem label="Score">{{ submission.score }}</NDescriptionsItem>
          <NDescriptionsItem label="Time">{{ submission.time_ms }} ms</NDescriptionsItem>
          <NDescriptionsItem label="Memory">{{ submission.memory_kb }} KB</NDescriptionsItem>
          <NDescriptionsItem label="Code sha256">{{ submission.code_sha256 || '-' }}</NDescriptionsItem>
          <NDescriptionsItem label="Created">
            <TimeText :value="submission.created_at" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Judged">
            <TimeText :value="submission.judged_at" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Message" :span="2">
            <NText>{{ submission.message || '-' }}</NText>
          </NDescriptionsItem>
        </NDescriptions>
      </PageCard>

      <PageCard title="Cases">
        <NDataTable :columns="columns" :data="cases" :loading="loading" :bordered="false" />
      </PageCard>

      <PageCard v-if="canDebug" title="Debug Logs">
        <NSpace vertical size="medium">
          <NAlert type="warning" :show-icon="true">
            Logs are truncated by API and paths are never exposed.
          </NAlert>
          <NSpace align="center">
            <NText>Case</NText>
            <NInputNumber v-model:value="debugCaseNo" :min="1" style="width: 140px" />
            <NButton :loading="debugLoading" @click="loadDebugLogs">Load logs</NButton>
          </NSpace>
          <ApiErrorAlert :error="debugError" />
          <JsonViewer v-if="debugLogs" :value="debugLogs" />
        </NSpace>
      </PageCard>
    </template>
  </NSpace>
</template>
