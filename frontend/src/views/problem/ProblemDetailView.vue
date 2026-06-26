<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink, useRoute } from 'vue-router'
import { NButton, NDescriptions, NDescriptionsItem, NSpace, NTag, NText } from 'naive-ui'

import { getProblem } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import OjosCodeBlock from '../../components/oj/OjosCodeBlock.vue'
import OjosDifficultyTag from '../../components/oj/OjosDifficultyTag.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosStatusTag from '../../components/oj/OjosStatusTag.vue'
import OjosVisibilityTag from '../../components/oj/OjosVisibilityTag.vue'
import { useAuthStore } from '../../stores/auth'
import type { ProblemItem } from '../../types/problem'
import { formatDateTime, formatDuration, formatMemoryLimit, splitCsv } from '../../utils/format'

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

onMounted(() => {
  void load()
})
</script>

<template>
  <div class="problem-detail-page">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !problem" />
    <EmptyView v-else-if="!loading && !error && !problem" description="Problem not found" />

    <template v-if="problem">
      <OjosPageHeader
        :title="problem.title"
        :description="`${problem.id} · ${problem.slug}`"
        eyebrow="Problem"
      >
        <template #actions>
          <RouterLink :to="`/problems/${problem.id}/submit`">
            <NButton type="primary">Submit</NButton>
          </RouterLink>
          <RouterLink v-if="canManage" :to="`/problems/${problem.id}/edit`">
            <NButton secondary>Edit</NButton>
          </RouterLink>
          <RouterLink v-if="canManage" :to="`/problems/${problem.id}/package`">
            <NButton secondary>Package</NButton>
          </RouterLink>
        </template>
      </OjosPageHeader>

      <div class="problem-reading-layout">
        <main class="problem-main">
          <OjosSection title="Statement">
            <article class="statement-view">
              {{ problem.statement || 'No statement yet.' }}
            </article>
          </OjosSection>

          <OjosSection v-if="problem.samples?.length" title="Samples">
            <NSpace vertical size="medium">
              <div v-for="sample in problem.samples" :key="sample.case_no" class="sample-block">
                <NText strong>Sample {{ sample.case_no }}</NText>
                <div class="sample-grid">
                  <OjosCodeBlock label="Input" :code="sample.input" />
                  <OjosCodeBlock label="Output" :code="sample.output" />
                </div>
              </div>
            </NSpace>
          </OjosSection>
        </main>

        <aside class="problem-aside">
          <OjosSection title="Summary">
            <NDescriptions :column="1" label-placement="left">
              <NDescriptionsItem label="Difficulty">
                <OjosDifficultyTag :difficulty="problem.difficulty" />
              </NDescriptionsItem>
              <NDescriptionsItem label="Visibility">
                <OjosVisibilityTag :visibility="problem.visibility" />
              </NDescriptionsItem>
              <NDescriptionsItem label="Status">
                <OjosStatusTag :status="problem.status" domain="problem" />
              </NDescriptionsItem>
              <NDescriptionsItem label="Time">
                {{ formatDuration(problem.time_limit_ms) }}
              </NDescriptionsItem>
              <NDescriptionsItem label="Memory">
                {{ formatMemoryLimit(problem.memory_limit_mb) }}
              </NDescriptionsItem>
              <NDescriptionsItem label="Updated">
                {{ formatDateTime(problem.updated_at) }}
              </NDescriptionsItem>
            </NDescriptions>
          </OjosSection>

          <OjosSection title="Tags">
            <NSpace v-if="splitCsv(problem.tags).length">
              <NTag v-for="tag in splitCsv(problem.tags)" :key="tag" size="small">{{ tag }}</NTag>
            </NSpace>
            <NText v-else depth="3">No tags</NText>
          </OjosSection>
        </aside>
      </div>
    </template>
  </div>
</template>

<style scoped>
.problem-detail-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.problem-reading-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 300px;
  gap: 18px;
  align-items: start;
}

.problem-main {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 16px;
}

.problem-aside {
  position: sticky;
  top: 92px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.statement-view {
  max-width: 860px;
  color: var(--text);
  font-family: var(--sans);
  font-size: 15px;
  line-height: 1.8;
  white-space: pre-wrap;
}

@media (max-width: 1100px) {
  .problem-reading-layout {
    grid-template-columns: 1fr;
  }

  .problem-aside {
    position: static;
  }
}
</style>
