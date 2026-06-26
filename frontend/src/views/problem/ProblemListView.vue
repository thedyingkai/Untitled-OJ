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
import PageCard from '../../components/common/PageCard.vue'
import TimeText from '../../components/common/TimeText.vue'
import { useAuthStore } from '../../stores/auth'
import type { ProblemItem, ProblemVisibility } from '../../types/problem'

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
    title: 'Title',
    key: 'title',
    render: (row) =>
      h(
        RouterLink,
        { to: `/problems/${row.id}`, class: 'table-link' },
        { default: () => row.title },
      ),
  },
  {
    title: 'Visibility',
    key: 'visibility',
    width: 120,
    render: (row) =>
      h(
        NTag,
        { size: 'small', type: row.visibility === 'public' ? 'success' : 'warning', round: true },
        { default: () => visibilityLabel(row.visibility) },
      ),
  },
  {
    title: 'Difficulty',
    key: 'difficulty',
    width: 110,
    render: (row) =>
      h(
        NTag,
        { size: 'small', type: difficultyType(row.difficulty), round: true },
        { default: () => difficultyLabel(row.difficulty) },
      ),
  },
  {
    title: 'Tags',
    key: 'tags',
    render: (row) =>
      h(
        NSpace,
        { size: 6 },
        {
          default: () =>
            splitTags(row.tags).map((tag) =>
              h(NTag, { key: tag, size: 'small' }, { default: () => tag }),
            ),
        },
      ),
  },
  {
    title: 'Limits',
    key: 'limits',
    width: 160,
    render: (row) => `${row.time_limit_ms} ms / ${row.memory_limit_mb} MB`,
  },
  {
    title: 'Updated',
    key: 'updated_at',
    width: 190,
    render: (row) => h(TimeText, { value: row.updated_at }),
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

function visibilityLabel(value: string): string {
  if (value === 'public') return 'Public'
  if (value === 'contest_only') return 'Contest'
  return 'Private'
}

function difficultyLabel(value: string): string {
  if (value === 'easy') return 'Easy'
  if (value === 'hard') return 'Hard'
  return 'Medium'
}

function difficultyType(value: string): 'success' | 'warning' | 'error' {
  if (value === 'easy') return 'success'
  if (value === 'hard') return 'error'
  return 'warning'
}

function splitTags(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)
}

onMounted(() => {
  void load()
})
</script>

<template>
  <PageCard title="Problems">
    <template #headerExtra>
      <RouterLink v-if="canCreate" to="/problems/new">
        <NButton type="primary">New problem</NButton>
      </RouterLink>
    </template>

    <NSpace vertical size="large">
      <NForm :model="filters" label-placement="top">
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
        <NSpace justify="end">
          <NButton @click="search">Search</NButton>
          <NButton secondary @click="load">Refresh</NButton>
        </NSpace>
      </NForm>

      <ApiErrorAlert :error="error" />
      <LoadingView v-if="loading && problems.length === 0" />
      <EmptyView v-else-if="!loading && !error && problems.length === 0" description="No problems" />
      <template v-else>
        <NDataTable :columns="columns" :data="problems" :loading="loading" :bordered="false" />
        <NSpace justify="end">
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
    </NSpace>
  </PageCard>
</template>
