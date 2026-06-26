<script setup lang="ts">
import { computed, h, onMounted, ref } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import {
  NAlert,
  NButton,
  NDataTable,
  NDescriptions,
  NDescriptionsItem,
  NGrid,
  NGi,
  NList,
  NListItem,
  NSpace,
  NTag,
  NText,
  type DataTableColumns,
} from 'naive-ui'

import { getProblemPackage, listPackageCases, validateProblemPackage } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import type {
  PackageLanguageLimit,
  PackageSummary,
  PackageValidationIssue,
  PackageValidationResult,
  TestCaseItem,
} from '../../types/problem'

const route = useRoute()
const loading = ref(false)
const validating = ref(false)
const error = ref<unknown>()
const pkg = ref<PackageSummary | null>(null)
const validation = ref<PackageValidationResult | null>(null)
const cases = ref<TestCaseItem[]>([])

const problemId = computed(() => Number(route.params.id))

const caseColumns: DataTableColumns<TestCaseItem> = [
  { title: 'Case', key: 'no', width: 90 },
  { title: 'Input', key: 'input' },
  { title: 'Answer', key: 'answer' },
  { title: 'Score', key: 'score', width: 90 },
  { title: 'Group', key: 'group', width: 90 },
  {
    title: 'Flags',
    key: 'flags',
    width: 160,
    render: (row) =>
      h(
        NSpace,
        { size: 6 },
        {
          default: () => [
            row.sample
              ? h(NTag, { size: 'small', type: 'info' }, { default: () => 'sample' })
              : null,
            row.hidden
              ? h(NTag, { size: 'small', type: 'warning' }, { default: () => 'hidden' })
              : null,
          ],
        },
      ),
  },
  {
    title: 'Limits',
    key: 'limits',
    width: 170,
    render: (row) =>
      `${row.time_limit_ms || '-'} ms / ${row.memory_limit_mb || '-'} MB`,
  },
]

const languageLimitColumns: DataTableColumns<PackageLanguageLimit> = [
  { title: 'Language', key: 'language' },
  { title: 'Time', key: 'time_limit_ms', render: (row) => `${row.time_limit_ms} ms` },
  { title: 'Memory', key: 'memory_limit_mb', render: (row) => `${row.memory_limit_mb} MB` },
]

async function load(): Promise<void> {
  if (!Number.isFinite(problemId.value) || problemId.value <= 0) {
    error.value = new Error('Invalid problem id')
    return
  }

  loading.value = true
  error.value = undefined

  try {
    const [packageData, caseData] = await Promise.all([
      getProblemPackage(problemId.value),
      listPackageCases(problemId.value),
    ])
    pkg.value = packageData.package
    validation.value = packageData.validation
    cases.value = caseData.cases
  } catch (err) {
    error.value = err
  } finally {
    loading.value = false
  }
}

async function runValidation(): Promise<void> {
  validating.value = true
  error.value = undefined

  try {
    const data = await validateProblemPackage(problemId.value)
    validation.value = data.validation
  } catch (err) {
    error.value = err
  } finally {
    validating.value = false
  }
}

function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) {
    return '0 B'
  }
  const units = ['B', 'KiB', 'MiB', 'GiB']
  let current = value
  let index = 0
  while (current >= 1024 && index < units.length - 1) {
    current /= 1024
    index += 1
  }
  return `${current.toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}

function issueText(issue: PackageValidationIssue): string {
  const parts = [issue.code, issue.message]
  if (issue.path) {
    parts.push(issue.path)
  }
  if (issue.case_no) {
    parts.push(`case ${issue.case_no}`)
  }
  return parts.join(' | ')
}

onMounted(() => {
  void load()
})
</script>

<template>
  <NSpace vertical size="large">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !pkg" />
    <EmptyView v-else-if="!loading && !error && !pkg" description="Package not found" />

    <template v-if="pkg">
      <PageCard :title="`Package: ${pkg.title || pkg.slug}`">
        <template #headerExtra>
          <NSpace>
            <RouterLink :to="`/problems/${problemId}`">
              <NButton secondary>Back</NButton>
            </RouterLink>
            <NButton secondary :loading="loading" @click="load">Refresh</NButton>
            <NButton type="primary" :loading="validating" @click="runValidation">
              Validate
            </NButton>
          </NSpace>
        </template>

        <NDescriptions bordered :column="2" label-placement="left">
          <NDescriptionsItem label="Schema">{{ pkg.schema }}</NDescriptionsItem>
          <NDescriptionsItem label="Slug">{{ pkg.slug }}</NDescriptionsItem>
          <NDescriptionsItem label="Type">{{ pkg.problem_type }}</NDescriptionsItem>
          <NDescriptionsItem label="Visibility">{{ pkg.visibility }}</NDescriptionsItem>
          <NDescriptionsItem label="Status">{{ pkg.status }}</NDescriptionsItem>
          <NDescriptionsItem label="Source">{{ pkg.source_format }}</NDescriptionsItem>
          <NDescriptionsItem label="Cases">{{ pkg.total_cases }}</NDescriptionsItem>
          <NDescriptionsItem label="Total score">{{ pkg.total_score }}</NDescriptionsItem>
          <NDescriptionsItem label="Samples">{{ pkg.sample_count }}</NDescriptionsItem>
          <NDescriptionsItem label="Files">{{ pkg.file_count }}</NDescriptionsItem>
          <NDescriptionsItem label="Size">{{ formatBytes(pkg.size_bytes) }}</NDescriptionsItem>
          <NDescriptionsItem label="Manifest sha256">{{ pkg.manifest_sha256 || '-' }}</NDescriptionsItem>
        </NDescriptions>
      </PageCard>

      <PageCard title="Validation">
        <NSpace vertical size="medium">
          <NAlert :type="validation?.valid ? 'success' : 'error'" :show-icon="true">
            {{ validation?.valid ? 'Package is valid' : 'Package has validation errors' }}
          </NAlert>

          <NGrid :cols="2" :x-gap="16" :y-gap="12" responsive="screen">
            <NGi>
              <NText strong>Errors</NText>
              <NList v-if="validation?.errors.length" bordered>
                <NListItem v-for="issue in validation.errors" :key="`${issue.code}-${issue.path}-${issue.case_no}`">
                  {{ issueText(issue) }}
                </NListItem>
              </NList>
              <EmptyView v-else description="No errors" />
            </NGi>
            <NGi>
              <NText strong>Warnings</NText>
              <NList v-if="validation?.warnings.length" bordered>
                <NListItem
                  v-for="issue in validation.warnings"
                  :key="`${issue.code}-${issue.path}-${issue.case_no}`"
                >
                  {{ issueText(issue) }}
                </NListItem>
              </NList>
              <EmptyView v-else description="No warnings" />
            </NGi>
          </NGrid>
        </NSpace>
      </PageCard>

      <PageCard title="Limits">
        <NSpace vertical size="medium">
          <NDescriptions bordered :column="2" label-placement="left">
            <NDescriptionsItem label="Default time">
              {{ pkg.limits.default_time_limit_ms }} ms
            </NDescriptionsItem>
            <NDescriptionsItem label="Default memory">
              {{ pkg.limits.default_memory_limit_mb }} MB
            </NDescriptionsItem>
          </NDescriptions>
          <NDataTable
            :columns="languageLimitColumns"
            :data="pkg.limits.languages"
            :bordered="false"
          />
        </NSpace>
      </PageCard>

      <PageCard title="Components">
        <NDescriptions bordered :column="3" label-placement="top">
          <NDescriptionsItem label="Runner">
            {{ pkg.runner.type }} / {{ pkg.runner.name }} / {{ pkg.runner.config_path }}
          </NDescriptionsItem>
          <NDescriptionsItem label="Checker">
            {{ pkg.checker.type }} / {{ pkg.checker.name }} / {{ pkg.checker.config_path }}
          </NDescriptionsItem>
          <NDescriptionsItem label="Scorer">
            {{ pkg.scorer.type }} / {{ pkg.scorer.name }} / {{ pkg.scorer.config_path }}
          </NDescriptionsItem>
        </NDescriptions>
      </PageCard>

      <PageCard title="Cases">
        <NDataTable :columns="caseColumns" :data="cases" :loading="loading" :bordered="false" />
      </PageCard>
    </template>
  </NSpace>
</template>
