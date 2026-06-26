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

import { getSubmission, getSubmissionCases, getSubmissionDebugLogs } from '../../api/judge'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosCodeBlock from '../../components/oj/OjosCodeBlock.vue'
import OjosLanguageTag from '../../components/oj/OjosLanguageTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import OjosStatusTag from '../../components/oj/OjosStatusTag.vue'
import { useAuthStore } from '../../stores/auth'
import type {
  JudgeStatus,
  SubmissionCaseItem,
  SubmissionDebugLogsResponse,
  SubmissionItem,
} from '../../types/judge'
import { formatDateTime, formatDuration, formatMemory } from '../../utils/format'

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
  { title: 'Case', key: 'case_no', width: 82 },
  {
    title: 'Status',
    key: 'status',
    width: 160,
    render: (row) => h(OjosStatusTag, { status: row.status }),
  },
  { title: 'Score', key: 'score', width: 82 },
  { title: 'Time', key: 'time_ms', width: 105, render: (row) => formatDuration(row.time_ms) },
  { title: 'Memory', key: 'memory_kb', width: 120, render: (row) => formatMemory(row.memory_kb) },
  { title: 'Message', key: 'message', render: (row) => row.message || '-' },
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
  <div class="submission-detail-page">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !submission" />
    <EmptyView v-else-if="!loading && !error && !submission" description="Submission not found" />

    <template v-if="submission">
      <OjosPageHeader
        :title="`Submission #${submission.id}`"
        :description="isTerminal ? 'Final judge result is available.' : 'This submission is still being processed.'"
        eyebrow="Judge Result"
      >
        <template #actions>
          <RouterLink to="/submissions">
            <NButton secondary>Back</NButton>
          </RouterLink>
          <NButton secondary :loading="loading" @click="() => load()">Refresh</NButton>
        </template>
      </OjosPageHeader>

      <div class="submission-summary-grid">
        <OjosStatCard label="Score" :value="submission.score" tone="primary" />
        <OjosStatCard label="Time" :value="formatDuration(submission.time_ms)" />
        <OjosStatCard label="Memory" :value="formatMemory(submission.memory_kb)" />
        <OjosStatCard label="Cases" :value="cases.length" />
      </div>

      <OjosSection title="Overview">
        <NDescriptions :column="2" label-placement="left" bordered>
          <NDescriptionsItem label="Status">
            <OjosStatusTag :status="submission.status" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Language">
            <OjosLanguageTag :language="submission.language" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Problem">
            <RouterLink :to="`/problems/${submission.problem_id}`" class="table-link">
              {{ submission.problem_id }}
            </RouterLink>
          </NDescriptionsItem>
          <NDescriptionsItem label="User">{{ submission.user_id }}</NDescriptionsItem>
          <NDescriptionsItem label="Code sha256">{{ submission.code_sha256 || '-' }}</NDescriptionsItem>
          <NDescriptionsItem label="Created">{{ formatDateTime(submission.created_at) }}</NDescriptionsItem>
          <NDescriptionsItem label="Judged">{{ formatDateTime(submission.judged_at) }}</NDescriptionsItem>
          <NDescriptionsItem label="Message" :span="2">
            <NText>{{ submission.message || '-' }}</NText>
          </NDescriptionsItem>
        </NDescriptions>
      </OjosSection>

      <OjosSection title="Case Results">
        <NDataTable :columns="columns" :data="cases" :loading="loading" :bordered="false" />
      </OjosSection>

      <OjosSection v-if="canDebug" title="Debug Logs">
        <NSpace vertical size="medium">
          <NAlert type="warning" :show-icon="true">
            Logs are returned by the API with truncation. Internal paths must not be exposed.
          </NAlert>
          <NSpace align="center">
            <NText>Case</NText>
            <NInputNumber v-model:value="debugCaseNo" :min="1" style="width: 140px" />
            <NButton :loading="debugLoading" @click="loadDebugLogs">Load logs</NButton>
          </NSpace>
          <ApiErrorAlert :error="debugError" />
          <div v-if="debugLogs" class="debug-log-grid">
            <OjosCodeBlock label="stdout" :code="debugLogs.stdout" />
            <OjosCodeBlock label="stderr" :code="debugLogs.stderr" />
            <OjosCodeBlock label="checker log" :code="debugLogs.checker_log" />
          </div>
        </NSpace>
      </OjosSection>
    </template>
  </div>
</template>

<style scoped>
.submission-detail-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.submission-summary-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

.debug-log-grid {
  display: grid;
  grid-template-columns: 1fr;
  gap: 12px;
}

@media (max-width: 900px) {
  .submission-summary-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
