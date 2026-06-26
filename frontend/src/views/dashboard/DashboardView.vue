<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { NButton, NDataTable, NTag, type DataTableColumns } from 'naive-ui'

import { getAdminHealth } from '../../api/health'
import { listSubmissions } from '../../api/judge'
import { listProblems } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import OjosDifficultyTag from '../../components/oj/OjosDifficultyTag.vue'
import OjosHealthBadge from '../../components/oj/OjosHealthBadge.vue'
import OjosLanguageTag from '../../components/oj/OjosLanguageTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import OjosStatusTag from '../../components/oj/OjosStatusTag.vue'
import OjosVisibilityTag from '../../components/oj/OjosVisibilityTag.vue'
import PermissionGuard from '../../components/common/PermissionGuard.vue'
import { useAuthStore } from '../../stores/auth'
import type { AdminHealthResponse } from '../../types/health'
import type { SubmissionItem } from '../../types/judge'
import type { ProblemItem } from '../../types/problem'
import { formatDateTime, formatDuration, formatMemory } from '../../utils/format'

const auth = useAuthStore()
const loading = ref(false)
const error = ref<unknown>()
const problems = ref<ProblemItem[]>([])
const submissions = ref<SubmissionItem[]>([])
const health = ref<AdminHealthResponse | null>(null)

const canUseAdmin = computed(
  () => auth.hasAnyRole(['super_admin', 'admin']) || auth.hasAnyPermission(['system.admin']),
)

const problemColumns: DataTableColumns<ProblemItem> = [
  {
    title: 'Problem',
    key: 'title',
    render: (row) =>
      h(
        RouterLink,
        { to: `/problems/${row.id}`, class: 'table-link' },
        { default: () => row.title },
      ),
  },
  {
    title: 'Difficulty',
    key: 'difficulty',
    width: 120,
    render: (row) => h(OjosDifficultyTag, { difficulty: row.difficulty }),
  },
  {
    title: 'Visibility',
    key: 'visibility',
    width: 120,
    render: (row) => h(OjosVisibilityTag, { visibility: row.visibility }),
  },
]

const submissionColumns: DataTableColumns<SubmissionItem> = [
  {
    title: 'ID',
    key: 'id',
    width: 86,
    render: (row) =>
      h(RouterLink, { to: `/submissions/${row.id}`, class: 'table-link' }, { default: () => row.id }),
  },
  {
    title: 'Status',
    key: 'status',
    width: 150,
    render: (row) => h(OjosStatusTag, { status: row.status }),
  },
  {
    title: 'Problem',
    key: 'problem_id',
    width: 100,
    render: (row) =>
      h(
        RouterLink,
        { to: `/problems/${row.problem_id}`, class: 'table-link' },
        { default: () => row.problem_id },
      ),
  },
  {
    title: 'Language',
    key: 'language',
    width: 120,
    render: (row) => h(OjosLanguageTag, { language: row.language }),
  },
  { title: 'Time', key: 'time_ms', width: 100, render: (row) => formatDuration(row.time_ms) },
  { title: 'Memory', key: 'memory_kb', width: 110, render: (row) => formatMemory(row.memory_kb) },
  { title: 'Submitted', key: 'created_at', width: 180, render: (row) => formatDateTime(row.created_at) },
]

async function load(): Promise<void> {
  loading.value = true
  error.value = undefined

  try {
    const [problemResp, submissionResp, healthResp] = await Promise.all([
      listProblems({ page: 1, page_size: 6 }),
      listSubmissions({ page: 1, page_size: 8 }),
      canUseAdmin.value ? getAdminHealth() : Promise.resolve(null),
    ])
    problems.value = problemResp.problems
    submissions.value = submissionResp.submissions
    health.value = healthResp
  } catch (err) {
    error.value = err
  } finally {
    loading.value = false
  }
}

onMounted(() => void load())
</script>

<template>
  <div class="dashboard-page">
    <OjosPageHeader
      title="OJOS"
      :description="`Signed in as ${auth.user?.username || 'user'} · ${auth.roles.join(', ') || 'user'}`"
      eyebrow="Online Judge"
    >
      <template #actions>
        <RouterLink to="/problems">
          <NButton type="primary">Problems</NButton>
        </RouterLink>
        <RouterLink to="/submissions">
          <NButton secondary>Submissions</NButton>
        </RouterLink>
        <NButton secondary :loading="loading" @click="load">Refresh</NButton>
      </template>
    </OjosPageHeader>

    <ApiErrorAlert :error="error" />

    <div class="dashboard-stats">
      <OjosStatCard label="Visible Problems" :value="problems.length" tone="primary" />
      <OjosStatCard label="Recent Submissions" :value="submissions.length" />
      <OjosStatCard label="Roles" :value="auth.roles.length || 1" />
      <div class="dashboard-health-card">
        <span>System Health</span>
        <OjosHealthBadge v-if="health" :status="health.status" />
        <NTag v-else size="small">User scope</NTag>
      </div>
    </div>

    <div class="quick-grid">
      <RouterLink to="/problems" class="quick-card">
        <strong>Problem Set</strong>
        <span>Search, read statements, and submit solutions.</span>
      </RouterLink>
      <RouterLink to="/submissions" class="quick-card">
        <strong>Submissions</strong>
        <span>Inspect verdicts, resource usage, and case results.</span>
      </RouterLink>
      <RouterLink to="/me" class="quick-card">
        <strong>Account</strong>
        <span>Review your roles and effective permissions.</span>
      </RouterLink>
    </div>

    <div class="dashboard-grid">
      <OjosSection title="Recent Problems">
        <EmptyView v-if="!loading && problems.length === 0" description="No problems" />
        <NDataTable
          v-else
          :columns="problemColumns"
          :data="problems"
          :loading="loading"
          :bordered="false"
        />
      </OjosSection>

      <OjosSection title="Recent Submissions">
        <EmptyView v-if="!loading && submissions.length === 0" description="No submissions" />
        <NDataTable
          v-else
          :columns="submissionColumns"
          :data="submissions"
          :loading="loading"
          :bordered="false"
        />
      </OjosSection>
    </div>

    <PermissionGuard :roles="['super_admin', 'admin']" :permissions="['system.admin']">
      <OjosSection title="Operations">
        <div class="quick-grid">
          <RouterLink to="/admin/health" class="quick-card">
            <strong>Service Health</strong>
            <span>Check gateway, APIs, database, Redis, storage, workers, and queue.</span>
          </RouterLink>
          <RouterLink to="/admin/judge" class="quick-card">
            <strong>Judge Cluster</strong>
            <span>Inspect workers, task leases, queue signals, and requeue actions.</span>
          </RouterLink>
          <RouterLink to="/admin/modules" class="quick-card">
            <strong>Modules</strong>
            <span>Browse module registry, manifests, components, and topology.</span>
          </RouterLink>
          <RouterLink to="/admin/permissions" class="quick-card">
            <strong>Permissions</strong>
            <span>Manage roles, grants, and problem-level authorization.</span>
          </RouterLink>
        </div>
      </OjosSection>
    </PermissionGuard>
  </div>
</template>

<style scoped>
.dashboard-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.dashboard-stats {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

.dashboard-health-card {
  display: flex;
  min-height: 92px;
  flex-direction: column;
  justify-content: center;
  gap: 8px;
  border: 1px solid var(--border-soft);
  border-radius: var(--ojos-radius);
  padding: 14px 16px;
  background: var(--surface);
  box-shadow: var(--shadow-sm);
}

.dashboard-health-card span {
  color: var(--muted);
  font-size: 12px;
  font-weight: 650;
}

.dashboard-grid {
  display: grid;
  grid-template-columns: minmax(0, 0.9fr) minmax(0, 1.1fr);
  gap: 16px;
}

@media (max-width: 1200px) {
  .dashboard-grid,
  .dashboard-stats {
    grid-template-columns: 1fr;
  }
}
</style>
