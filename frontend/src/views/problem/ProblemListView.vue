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
    title: '题目',
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
    title: '难度',
    key: 'difficulty',
    width: 120,
    render: (row) => h(OjosDifficultyTag, { difficulty: row.difficulty }),
  },
  {
    title: '状态',
    key: 'status',
    width: 120,
    render: (row) => h(OjosStatusTag, { status: row.status, domain: 'problem' }),
  },
  {
    title: '可见性',
    key: 'visibility',
    width: 120,
    render: (row) => h(OjosVisibilityTag, { visibility: row.visibility }),
  },
  {
    title: '标签',
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
    title: '限制',
    key: 'limits',
    width: 150,
    render: (row) => `${formatDuration(row.time_limit_ms)} / ${formatMemoryLimit(row.memory_limit_mb)}`,
  },
  {
    title: '更新',
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
      title="题目"
      description="按难度、可见性和标签筛选题目，快速进入题面或提交入口。"
      eyebrow="题库"
    >
      <template #actions>
        <RouterLink v-if="canCreate" to="/problems/new">
          <NButton type="primary">新建题目</NButton>
        </RouterLink>
        <NButton secondary :loading="loading" @click="load">刷新</NButton>
      </template>
    </OjosPageHeader>

    <OjosToolbar>
      <NForm :model="filters" label-placement="top" class="problem-filter-form">
        <NGrid :cols="4" :x-gap="12" :y-gap="8" responsive="screen">
          <NFormItemGi label="关键词">
            <NInput
              v-model:value="filters.keyword"
              clearable
              placeholder="标题或短标识"
              @keydown.enter.prevent="search"
            />
          </NFormItemGi>
          <NFormItemGi label="可见性">
            <NSelect
              v-model:value="filters.visibility"
              clearable
              :options="[
                { label: '公开', value: 'public' },
                { label: '私有', value: 'private' },
                { label: '仅比赛', value: 'contest_only' },
              ]"
            />
          </NFormItemGi>
          <NFormItemGi label="难度">
            <NSelect
              v-model:value="filters.difficulty"
              clearable
              :options="[
                { label: '简单', value: 'easy' },
                { label: '中等', value: 'medium' },
                { label: '困难', value: 'hard' },
              ]"
            />
          </NFormItemGi>
          <NFormItemGi label="标签">
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
        <NButton type="primary" @click="search">搜索</NButton>
      </template>
    </OjosToolbar>

    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && problems.length === 0" />
    <EmptyView v-else-if="!loading && !error && problems.length === 0" description="暂无题目" />

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
