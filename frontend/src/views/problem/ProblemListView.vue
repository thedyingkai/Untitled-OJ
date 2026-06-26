<script setup lang="ts">
import { computed, h, onMounted, reactive, ref } from 'vue'
import { RouterLink } from 'vue-router'
import {
  NButton,
  NDataTable,
  NForm,
  NFormItemGi,
  NGrid,
  NInput,
  NPagination,
  NSelect,
  NSpace,
  NTag,
  type DataTableColumns,
} from 'naive-ui'

import { listProblems } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosDifficultyTag from '../../components/oj/OjosDifficultyTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosStatusTag from '../../components/oj/OjosStatusTag.vue'
import OjosToolbar from '../../components/oj/OjosToolbar.vue'
import OjosVisibilityTag from '../../components/oj/OjosVisibilityTag.vue'
import { useAuthStore } from '../../stores/auth'
import type { ProblemItem, ProblemVisibility } from '../../types/problem'
import { formatDateTime, formatDuration, formatMemoryLimit, splitCsv } from '../../utils/format'

const auth = useAuthStore()
const loading = ref(false)
const error = ref<unknown>()
const problems = ref<ProblemItem[]>([])
const total = ref(0)

const filters = reactive({
  keyword: '',
  visibility: '',
  difficulty: '',
  tags: '',
  page: 1,
  pageSize: 20,
})

const canCreate = computed(
  () =>
    auth.hasPermission('problem.create') ||
    auth.hasPermission('system.admin') ||
    auth.hasAnyRole(['super_admin', 'admin']),
)

const columns: DataTableColumns<ProblemItem> = [
  {
    title: 'Problem',
    key: 'title',
    minWidth: 280,
    render: (row) =>
      h('div', { class: 'problem-title-cell' }, [
        h(
          RouterLink,
          { to: `/problems/${row.id}`, class: 'table-link problem-title-link' },
          { default: () => row.title },
        ),
        h('span', { class: 'problem-slug' }, `${row.id} · ${row.slug}`),
      ]),
  },
  {
    title: 'Difficulty',
    key: 'difficulty',
    width: 120,
    render: (row) => h(OjosDifficultyTag, { difficulty: row.difficulty }),
  },
  {
    title: 'State',
    key: 'status',
    width: 120,
    render: (row) => h(OjosStatusTag, { status: row.status, domain: 'problem' }),
  },
  {
    title: 'Visibility',
    key: 'visibility',
    width: 120,
    render: (row) => h(OjosVisibilityTag, { visibility: row.visibility }),
  },
  {
    title: 'Tags',
    key: 'tags',
    minWidth: 180,
    render: (row) =>
      h(
        NSpace,
        { size: 6 },
        {
          default: () => {
            const tags = splitCsv(row.tags)
            if (!tags.length) return '-'
            return tags.map((tag) => h(NTag, { key: tag, size: 'small' }, { default: () => tag }))
          },
        },
      ),
  },
  {
    title: 'Limits',
    key: 'limits',
    width: 150,
    render: (row) => `${formatDuration(row.time_limit_ms)} / ${formatMemoryLimit(row.memory_limit_mb)}`,
  },
  {
    title: 'Updated',
    key: 'updated_at',
    width: 180,
    render: (row) => formatDateTime(row.updated_at),
  },
]

async function load(): Promise<void> {
  loading.value = true
  error.value = undefined

  try {
    const data = await listProblems({
      page: filters.page,
      page_size: filters.pageSize,
      keyword: filters.keyword || undefined,
      visibility: (filters.visibility || undefined) as ProblemVisibility | undefined,
      difficulty: filters.difficulty || undefined,
      tags: filters.tags || undefined,
    })
    problems.value = data.problems
    total.value = data.total
  } catch (err) {
    error.value = err
  } finally {
    loading.value = false
  }
}

function search(): void {
  filters.page = 1
  void load()
}

onMounted(() => {
  void load()
})
</script>

<template>
  <div class="problem-list-page">
    <OjosPageHeader
      title="Problems"
      description="Browse available problems, filter by difficulty or tags, and jump directly into submissions."
      eyebrow="Online Judge"
    >
      <template #actions>
        <RouterLink v-if="canCreate" to="/problems/new">
          <NButton type="primary">New problem</NButton>
        </RouterLink>
        <NButton secondary :loading="loading" @click="load">Refresh</NButton>
      </template>
    </OjosPageHeader>

    <OjosToolbar>
      <NForm :model="filters" label-placement="top" class="problem-filter-form">
        <NGrid :cols="4" :x-gap="12" :y-gap="8" responsive="screen">
          <NFormItemGi label="Keyword">
            <NInput
              v-model:value="filters.keyword"
              clearable
              placeholder="Title or slug"
              @keydown.enter.prevent="search"
            />
          </NFormItemGi>
          <NFormItemGi label="Visibility">
            <NSelect
              v-model:value="filters.visibility"
              clearable
              :options="[
                { label: 'Public', value: 'public' },
                { label: 'Private', value: 'private' },
                { label: 'Contest', value: 'contest_only' },
              ]"
            />
          </NFormItemGi>
          <NFormItemGi label="Difficulty">
            <NSelect
              v-model:value="filters.difficulty"
              clearable
              :options="[
                { label: 'Easy', value: 'easy' },
                { label: 'Medium', value: 'medium' },
                { label: 'Hard', value: 'hard' },
              ]"
            />
          </NFormItemGi>
          <NFormItemGi label="Tags">
            <NInput
              v-model:value="filters.tags"
              clearable
              placeholder="dp, graph"
              @keydown.enter.prevent="search"
            />
          </NFormItemGi>
        </NGrid>
      </NForm>
      <template #actions>
        <NButton type="primary" @click="search">Search</NButton>
      </template>
    </OjosToolbar>

    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && problems.length === 0" />
    <EmptyView v-else-if="!loading && !error && problems.length === 0" description="No problems" />

    <template v-else>
      <NDataTable
        :columns="columns"
        :data="problems"
        :loading="loading"
        :bordered="false"
        class="ojos-data-table"
      />
      <NSpace justify="end" class="list-pagination">
        <NPagination
          v-model:page="filters.page"
          v-model:page-size="filters.pageSize"
          :item-count="total"
          show-size-picker
          :page-sizes="[10, 20, 50, 100]"
          @update:page="load"
          @update:page-size="search"
        />
      </NSpace>
    </template>
  </div>
</template>

<style scoped>
.problem-list-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.problem-filter-form {
  width: 100%;
}

.problem-title-cell {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 3px;
}

.problem-title-link {
  overflow: hidden;
  color: var(--text-strong);
  font-size: 14px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.problem-slug {
  color: var(--muted);
  font-size: 12px;
}

.list-pagination {
  padding-top: 4px;
}
</style>
