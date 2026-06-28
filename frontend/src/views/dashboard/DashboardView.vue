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
    title: '题目',
    key: 'title',
    render: (row) =>
      h(
        RouterLink,
        { to: `/problems/${row.id}`, class: 'table-link' },
        { default: () => row.title },
      ),
  },
  {
    title: '难度',
    key: 'difficulty',
    width: 120,
    render: (row) => h(OjosDifficultyTag, { difficulty: row.difficulty }),
  },
  {
    title: '可见性',
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
    title: '状态',
    key: 'status',
    width: 150,
    render: (row) => h(OjosStatusTag, { status: row.status }),
  },
  {
    title: '题目',
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
    title: '语言',
    key: 'language',
    width: 120,
    render: (row) => h(OjosLanguageTag, { language: row.language }),
  },
  { title: '耗时', key: 'time_ms', width: 100, render: (row) => formatDuration(row.time_ms) },
  { title: '内存', key: 'memory_kb', width: 110, render: (row) => formatMemory(row.memory_kb) },
  { title: '提交时间', key: 'created_at', width: 180, render: (row) => formatDateTime(row.created_at) },
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
      :description="`当前用户 ${auth.user?.username || 'user'}，角色 ${auth.roles.join(', ') || 'user'}`"
      eyebrow="Online Judge"
    >
      <template #actions>
        <RouterLink to="/problems">
          <NButton type="primary">题库</NButton>
        </RouterLink>
        <RouterLink to="/submissions">
          <NButton secondary>提交</NButton>
        </RouterLink>
        <NButton secondary :loading="loading" @click="load">刷新</NButton>
      </template>
    </OjosPageHeader>

    <ApiErrorAlert :error="error" />

    <div class="dashboard-stats">
      <OjosStatCard label="可见题目" :value="problems.length" tone="primary" />
      <OjosStatCard label="最近提交" :value="submissions.length" />
      <OjosStatCard label="角色数" :value="auth.roles.length || 1" />
      <div class="dashboard-health-card">
        <span>系统健康</span>
        <OjosHealthBadge v-if="health" :status="health.status" />
        <NTag v-else size="small">用户视图</NTag>
      </div>
    </div>

    <div class="quick-grid">
      <RouterLink to="/problems" class="quick-card">
        <strong>题库</strong>
        <span>检索题目、阅读题面并提交代码。</span>
      </RouterLink>
      <RouterLink to="/submissions" class="quick-card">
        <strong>提交记录</strong>
        <span>查看评测结果、资源占用和测试点详情。</span>
      </RouterLink>
      <RouterLink to="/me" class="quick-card">
        <strong>账号</strong>
        <span>查看角色和当前生效权限。</span>
      </RouterLink>
    </div>

    <div class="dashboard-grid">
      <OjosSection title="最近题目">
        <EmptyView v-if="!loading && problems.length === 0" description="暂无题目" />
        <NDataTable
          v-else
          :columns="problemColumns"
          :data="problems"
          :loading="loading"
          :bordered="false"
        />
      </OjosSection>

      <OjosSection title="最近提交">
        <EmptyView v-if="!loading && submissions.length === 0" description="暂无提交" />
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
      <OjosSection title="运维入口">
        <div class="quick-grid">
          <RouterLink to="/admin/health" class="quick-card">
            <strong>服务健康</strong>
            <span>查看 Gateway、API、数据库、Redis、存储、Worker 和队列。</span>
          </RouterLink>
          <RouterLink to="/admin/judge" class="quick-card">
            <strong>评测集群</strong>
            <span>查看 Worker、任务租约、队列信号和重排队操作。</span>
          </RouterLink>
          <RouterLink to="/admin/modules" class="quick-card">
            <strong>模块中心</strong>
            <span>查看模块注册表、manifest、组件和拓扑。</span>
          </RouterLink>
          <RouterLink to="/admin/permissions" class="quick-card">
            <strong>权限</strong>
            <span>管理角色、授权和题目级权限。</span>
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
