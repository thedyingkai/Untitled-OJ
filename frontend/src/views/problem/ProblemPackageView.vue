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
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatCard from '../../components/oj/OjosStatCard.vue'
import OjosStatusTag from '../../components/oj/OjosStatusTag.vue'
import OjosVisibilityTag from '../../components/oj/OjosVisibilityTag.vue'
import type {
  PackageLanguageLimit,
  PackageSummary,
  PackageValidationIssue,
  PackageValidationResult,
  TestCaseItem,
} from '../../types/problem'
import { formatBytes, formatDuration, formatMemoryLimit } from '../../utils/format'

const route = useRoute()
const loading = ref(false)
const validating = ref(false)
const error = ref<unknown>()
const pkg = ref<PackageSummary | null>(null)
const validation = ref<PackageValidationResult | null>(null)
const cases = ref<TestCaseItem[]>([])

const problemId = computed(() => Number(route.params.id))

const caseColumns: DataTableColumns<TestCaseItem> = [
  { title: 'Case', key: 'no', width: 82 },
  { title: 'Input', key: 'input', minWidth: 180 },
  { title: 'Answer', key: 'answer', minWidth: 180 },
  { title: 'Score', key: 'score', width: 82 },
  { title: 'Group', key: 'group', width: 82 },
  {
    title: 'Flags',
    key: 'flags',
    width: 150,
    render: (row) =>
      h(
        NSpace,
        { size: 6 },
        {
          default: () => [
            row.sample ? h(NTag, { size: 'small', type: 'info' }, { default: () => 'sample' }) : null,
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
      `${row.time_limit_ms ? formatDuration(row.time_limit_ms) : '-'} / ${
        row.memory_limit_mb ? formatMemoryLimit(row.memory_limit_mb) : '-'
      }`,
  },
]

const languageLimitColumns: DataTableColumns<PackageLanguageLimit> = [
  { title: 'Language', key: 'language' },
  { title: 'Time', key: 'time_limit_ms', render: (row) => formatDuration(row.time_limit_ms) },
  { title: 'Memory', key: 'memory_limit_mb', render: (row) => formatMemoryLimit(row.memory_limit_mb) },
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
  <div class="package-page">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !pkg" />
    <EmptyView v-else-if="!loading && !error && !pkg" description="Package not found" />

    <template v-if="pkg">
      <OjosPageHeader
        :title="`Package: ${pkg.title || pkg.slug}`"
        :description="`Manifest ${pkg.manifest_sha256 || 'not available'}`"
        eyebrow="Problem Data"
      >
        <template #actions>
          <RouterLink :to="`/problems/${problemId}`">
            <NButton secondary>Back</NButton>
          </RouterLink>
          <NButton secondary :loading="loading" @click="load">Refresh</NButton>
          <NButton type="primary" :loading="validating" @click="runValidation">Validate</NButton>
        </template>
      </OjosPageHeader>

      <div class="package-summary-grid">
        <OjosStatCard label="Cases" :value="pkg.total_cases" />
        <OjosStatCard label="Score" :value="pkg.total_score" />
        <OjosStatCard label="Samples" :value="pkg.sample_count" />
        <OjosStatCard label="Size" :value="formatBytes(pkg.size_bytes)" />
      </div>

      <OjosSection title="Package Summary">
        <NDescriptions :column="2" label-placement="left" bordered>
          <NDescriptionsItem label="Schema">{{ pkg.schema }}</NDescriptionsItem>
          <NDescriptionsItem label="Slug">{{ pkg.slug }}</NDescriptionsItem>
          <NDescriptionsItem label="Type">{{ pkg.problem_type }}</NDescriptionsItem>
          <NDescriptionsItem label="Visibility">
            <OjosVisibilityTag :visibility="pkg.visibility" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Status">
            <OjosStatusTag :status="pkg.status" domain="problem" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Source">{{ pkg.source_format }}</NDescriptionsItem>
          <NDescriptionsItem label="Files">{{ pkg.file_count }}</NDescriptionsItem>
          <NDescriptionsItem label="Manifest">{{ pkg.manifest_sha256 || '-' }}</NDescriptionsItem>
        </NDescriptions>
      </OjosSection>

      <OjosSection title="Validation">
        <NSpace vertical size="medium">
          <NAlert :type="validation?.valid ? 'success' : 'error'" :show-icon="true">
            {{ validation?.valid ? 'Package is valid' : 'Package has validation errors' }}
          </NAlert>

          <NGrid :cols="2" :x-gap="16" :y-gap="12" responsive="screen">
            <NGi>
              <NText strong>Errors</NText>
              <NList v-if="validation?.errors.length" bordered>
                <NListItem
                  v-for="issue in validation.errors"
                  :key="`${issue.code}-${issue.path}-${issue.case_no}`"
                >
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
      </OjosSection>

      <OjosSection title="Limits">
        <NSpace vertical size="medium">
          <NDescriptions :column="2" label-placement="left" bordered>
            <NDescriptionsItem label="Default time">
              {{ formatDuration(pkg.limits.default_time_limit_ms) }}
            </NDescriptionsItem>
            <NDescriptionsItem label="Default memory">
              {{ formatMemoryLimit(pkg.limits.default_memory_limit_mb) }}
            </NDescriptionsItem>
          </NDescriptions>
          <NDataTable :columns="languageLimitColumns" :data="pkg.limits.languages" :bordered="false" />
        </NSpace>
      </OjosSection>

      <OjosSection title="Components">
        <NDescriptions :column="3" label-placement="top" bordered>
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
      </OjosSection>

      <OjosSection title="Cases">
        <NDataTable :columns="caseColumns" :data="cases" :loading="loading" :bordered="false" />
      </OjosSection>
    </template>
  </div>
</template>

<style scoped>
.package-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.package-summary-grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
}

@media (max-width: 900px) {
  .package-summary-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
