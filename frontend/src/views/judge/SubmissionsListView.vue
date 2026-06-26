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
].map((value) => ({ label: value, value }))

const columns: DataTableColumns<SubmissionItem> = [
  {
    title: 'ID',
    key: 'id',
    width: 88,
    render: (row) =>
      h(RouterLink, { to: `/submissions/${row.id}`, class: 'table-link' }, { default: () => row.id }),
  },
  {
    title: 'Status',
    key: 'status',
    width: 160,
    render: (row) => h(OjosStatusTag, { status: row.status }),
  },
  {
    title: 'Problem',
    key: 'problem_id',
    width: 110,
    render: (row) =>
      h(
        RouterLink,
        { to: `/problems/${row.problem_id}`, class: 'table-link' },
        { default: () => row.problem_id },
      ),
  },
  { title: 'User', key: 'user_id', width: 90 },
  {
    title: 'Language',
    key: 'language',
    width: 130,
    render: (row) => h(OjosLanguageTag, { language: row.language }),
  },
  { title: 'Score', key: 'score', width: 82 },
  { title: 'Time', key: 'time_ms', width: 100, render: (row) => formatDuration(row.time_ms) },
  { title: 'Memory', key: 'memory_kb', width: 110, render: (row) => formatMemory(row.memory_kb) },
  {
    title: 'Submitted',
    key: 'created_at',
    width: 180,
    render: (row) => formatDateTime(row.created_at),
  },
  {
    title: 'Judged',
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
      title="Submissions"
      description="High-density judging history with verdicts, resource usage, and timestamps."
      eyebrow="Judge"
    >
      <template #actions>
        <NButton secondary :loading="loading" @click="load">Refresh</NButton>
      </template>
    </OjosPageHeader>

    <OjosToolbar>
      <NForm :model="filters" label-placement="top" class="submission-filter-form">
        <NGrid :cols="6" :x-gap="12" :y-gap="8" responsive="screen">
          <NFormItemGi label="Status">
            <NSelect v-model:value="filters.status" clearable :options="statusOptions" />
          </NFormItemGi>
          <NFormItemGi label="Language">
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
          <NFormItemGi label="Problem">
            <NInputNumber v-model:value="filters.problemId" clearable :min="1" style="width: 100%" />
          </NFormItemGi>
          <NFormItemGi label="User">
            <NInputNumber v-model:value="filters.userId" clearable :min="1" style="width: 100%" />
          </NFormItemGi>
          <NFormItemGi label="From">
            <NInput
              v-model:value="filters.createdFrom"
              clearable
              placeholder="YYYY-MM-DD"
              @keydown.enter.prevent="search"
            />
          </NFormItemGi>
          <NFormItemGi label="To">
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
        <NButton type="primary" @click="search">Filter</NButton>
      </template>
    </OjosToolbar>

    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && submissions.length === 0" />
    <EmptyView
      v-else-if="!loading && !error && submissions.length === 0"
      description="No submissions"
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
