<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import { NButton, NDescriptions, NDescriptionsItem, NSpace, NTag, NText } from 'naive-ui'

import { getProblem } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import TimeText from '../../components/common/TimeText.vue'
import { useAuthStore } from '../../stores/auth'
import type { ProblemItem } from '../../types/problem'

const route = useRoute()
const auth = useAuthStore()
const loading = ref(false)
const error = ref<unknown>()
const problem = ref<ProblemItem | null>(null)

const problemId = computed(() => Number(route.params.id))
const canManage = computed(() => {
  if (!problem.value) {
    return false
  }
  return (
    problem.value.created_by === auth.user?.user_id ||
    auth.hasPermission('problem.edit') ||
    auth.hasPermission('system.admin') ||
    auth.hasAnyRole(['super_admin', 'admin'])
  )
})

async function load(): Promise<void> {
  if (!Number.isFinite(problemId.value) || problemId.value <= 0) {
    error.value = new Error('Invalid problem id')
    return
  }

  loading.value = true
  error.value = undefined

  try {
    const data = await getProblem(problemId.value)
    problem.value = data.problem
  } catch (err) {
    error.value = err
  } finally {
    loading.value = false
  }
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
  <NSpace vertical size="large">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !problem" />

    <template v-if="problem">
      <PageCard :title="problem.title">
        <template #headerExtra>
          <NSpace>
            <RouterLink :to="`/problems/${problem.id}/submit`">
              <NButton type="primary">Submit</NButton>
            </RouterLink>
            <RouterLink v-if="canManage" :to="`/problems/${problem.id}/edit`">
              <NButton secondary>Edit</NButton>
            </RouterLink>
            <RouterLink v-if="canManage" :to="`/problems/${problem.id}/package`">
              <NButton secondary>Package</NButton>
            </RouterLink>
          </NSpace>
        </template>

        <NDescriptions bordered :column="2" label-placement="left">
          <NDescriptionsItem label="ID">{{ problem.id }}</NDescriptionsItem>
          <NDescriptionsItem label="Slug">{{ problem.slug }}</NDescriptionsItem>
          <NDescriptionsItem label="Visibility">
            <NTag size="small" :type="problem.visibility === 'public' ? 'success' : 'warning'">
              {{ problem.visibility }}
            </NTag>
          </NDescriptionsItem>
          <NDescriptionsItem label="Status">{{ problem.status }}</NDescriptionsItem>
          <NDescriptionsItem label="Time limit">{{ problem.time_limit_ms }} ms</NDescriptionsItem>
          <NDescriptionsItem label="Memory limit">{{ problem.memory_limit_mb }} MB</NDescriptionsItem>
          <NDescriptionsItem label="Difficulty">{{ problem.difficulty }}</NDescriptionsItem>
          <NDescriptionsItem label="Updated">
            <TimeText :value="problem.updated_at" />
          </NDescriptionsItem>
          <NDescriptionsItem label="Tags" :span="2">
            <NSpace v-if="splitTags(problem.tags).length">
              <NTag v-for="tag in splitTags(problem.tags)" :key="tag" size="small">{{ tag }}</NTag>
            </NSpace>
            <NText v-else depth="3">-</NText>
          </NDescriptionsItem>
        </NDescriptions>
      </PageCard>

      <PageCard title="Statement">
        <pre class="statement-view">{{ problem.statement || 'No statement yet' }}</pre>
      </PageCard>

      <PageCard v-if="problem.samples?.length" title="Samples">
        <NSpace vertical size="medium">
          <div v-for="sample in problem.samples" :key="sample.case_no" class="sample-block">
            <NText strong>Sample {{ sample.case_no }}</NText>
            <div class="sample-grid">
              <div>
                <NText depth="3">Input</NText>
                <pre>{{ sample.input }}</pre>
              </div>
              <div>
                <NText depth="3">Output</NText>
                <pre>{{ sample.output }}</pre>
              </div>
            </div>
          </div>
        </NSpace>
      </PageCard>
    </template>
  </NSpace>
</template>
