<script setup lang="ts">
import { h, onMounted, reactive, ref } from 'vue'
import { RouterLink } from 'vue-router'
import {
  NButton,
  NDataTable,
  NForm,
  NFormItemGi,
  NGrid,
  NInput,
  NInputNumber,
  NPagination,
  NSelect,
  NSpace,
  type DataTableColumns,
} from 'naive-ui'

import { listJudgeLanguages, listSubmissions } from '../../api/judge'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosLanguageTag from '../../components/oj/OjosLanguageTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosStatusTag from '../../components/oj/OjosStatusTag.vue'
import OjosToolbar from '../../components/oj/OjosToolbar.vue'
import type { JudgeLanguage, JudgeStatus, SubmissionItem } from '../../types/judge'
import { formatDateTime, formatDuration, formatMemory } from '../../utils/format'
import { getJudgeStatusMeta } from '../../utils/status'

const loading = ref(false)
const error = ref<unknown>()
const submissions = ref<SubmissionItem[]>([])
const total = ref(0)
const languages = ref<JudgeLanguage[]>([])

const filters = reactive({
  page: 1,
  pageSize: 20,
  status: '',
  problemId: null as number | null,
  userId: null as number | null,
  language: '',
  createdFrom: '',
  createdTo: '',
})

const statusOptions = [
  'PENDING',
  'JUDGING',
  'ACCEPTED',
  'WRONG_ANSWER',
  'COMPILE_ERROR',
  'RUNTIME_ERROR',
  'TIME_LIMIT_EXCEEDED',
  'MEMORY_LIMIT_EXCEEDED',
  'OUTPUT_LIMIT_EXCEEDED',
  'SYSTEM_ERROR',
  'CANCELLED',
  'UNSUPPORTED_LANGUAGE',
].map((value) => ({ label: getJudgeStatusMeta(value).label, value }))

const columns: DataTableColumns<SubmissionItem> = [
  {
    title: 'ID',
    key: 'id',
    width: 88,
    render: (row) =>
      h(RouterLink, { to: `/submissions/${row.id}`, class: 'table-link' }, { default: () => row.id }),
  },
  {
    title: '状态',
    key: 'status',
    width: 160,
    render: (row) => h(OjosStatusTag, { status: row.status }),
  },
  {
    title: '题目',
    key: 'problem_id',
    width: 110,
    render: (row) =>
      h(
        RouterLink,
        { to: `/problems/${row.problem_id}`, class: 'table-link' },
        { default: () => row.problem_id },
      ),
  },
  { title: '用户', key: 'user_id', width: 90 },
  {
    title: '语言',
    key: 'language',
    width: 130,
    render: (row) => h(OjosLanguageTag, { language: row.language }),
  },
  { title: '分数', key: 'score', width: 82 },
  { title: '耗时', key: 'time_ms', width: 100, render: (row) => formatDuration(row.time_ms) },
  { title: '内存', key: 'memory_kb', width: 110, render: (row) => formatMemory(row.memory_kb) },
  {
    title: '提交时间',
    key: 'created_at',
    width: 180,
    render: (row) => formatDateTime(row.created_at),
  },
  {
    title: '评测时间',
    key: 'judged_at',
    width: 180,
    render: (row) => formatDateTime(row.judged_at),
  },
]

async function load(): Promise<void> {
  loading.value = true
  error.value = undefined

  try {
    const data = await listSubmissions({
      page: filters.page,
      page_size: filters.pageSize,
      status: (filters.status || undefined) as JudgeStatus | undefined,
      problem_id: filters.problemId || undefined,
      user_id: filters.userId || undefined,
      language: filters.language || undefined,
      created_from: filters.createdFrom || undefined,
      created_to: filters.createdTo || undefined,
    })
    submissions.value = data.submissions
    total.value = data.total
  } catch (err) {
    error.value = err
  } finally {
    loading.value = false
  }
}

async function loadLanguages(): Promise<void> {
  try {
    const data = await listJudgeLanguages()
    languages.value = data.languages
  } catch {
    languages.value = []
  }
}

function search(): void {
  filters.page = 1
  void load()
}

onMounted(() => {
  void loadLanguages()
  void load()
})
</script>

<template>
  <div class="submissions-page">
    <OjosPageHeader
      title="提交记录"
      description="按评测状态、资源占用和时间排序的高密度提交历史。"
      eyebrow="评测"
    >
      <template #actions>
        <NButton secondary :loading="loading" @click="load">刷新</NButton>
      </template>
    </OjosPageHeader>

    <OjosToolbar>
      <NForm :model="filters" label-placement="top" class="submission-filter-form">
        <NGrid :cols="6" :x-gap="12" :y-gap="8" responsive="screen">
          <NFormItemGi label="状态">
            <NSelect v-model:value="filters.status" clearable :options="statusOptions" />
          </NFormItemGi>
          <NFormItemGi label="语言">
            <NSelect
              v-model:value="filters.language"
              clearable
              :options="languages.map((item) => ({
                label: item.display_name,
                value: item.id,
                disabled: !item.enabled,
              }))"
            />
          </NFormItemGi>
          <NFormItemGi label="题目">
            <NInputNumber v-model:value="filters.problemId" clearable :min="1" style="width: 100%" />
          </NFormItemGi>
          <NFormItemGi label="用户">
            <NInputNumber v-model:value="filters.userId" clearable :min="1" style="width: 100%" />
          </NFormItemGi>
          <NFormItemGi label="开始日期">
            <NInput
              v-model:value="filters.createdFrom"
              clearable
              placeholder="YYYY-MM-DD"
              @keydown.enter.prevent="search"
            />
          </NFormItemGi>
          <NFormItemGi label="结束日期">
            <NInput
              v-model:value="filters.createdTo"
              clearable
              placeholder="YYYY-MM-DD"
              @keydown.enter.prevent="search"
            />
          </NFormItemGi>
        </NGrid>
      </NForm>
      <template #actions>
        <NButton type="primary" @click="search">筛选</NButton>
      </template>
    </OjosToolbar>

    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && submissions.length === 0" />
    <EmptyView
      v-else-if="!loading && !error && submissions.length === 0"
      description="暂无提交"
    />
    <template v-else>
      <NDataTable
        :columns="columns"
        :data="submissions"
        :loading="loading"
        :bordered="false"
        class="ojos-data-table"
      />
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
  </div>
</template>

<style scoped>
.submissions-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.submission-filter-form {
  width: 100%;
}
</style>
