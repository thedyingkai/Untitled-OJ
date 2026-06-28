<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import { NButton, NSpace, useMessage } from 'naive-ui'

import { deleteProblem, getProblem, updateProblem } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import ConfirmAction from '../../components/common/ConfirmAction.vue'
import EmptyView from '../../components/common/EmptyView.vue'
import LoadingView from '../../components/common/LoadingView.vue'
import PageCard from '../../components/common/PageCard.vue'
import ProblemForm from '../../components/problem/ProblemForm.vue'
import { useAuthStore } from '../../stores/auth'
import type { ProblemFormInput, ProblemItem } from '../../types/problem'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const message = useMessage()

const loading = ref(false)
const saving = ref(false)
const deleting = ref(false)
const error = ref<unknown>()
const problem = ref<ProblemItem | null>(null)

const problemId = computed(() => Number(route.params.id))

function canManageProblem(item: ProblemItem): boolean {
  return (
    item.created_by === auth.user?.user_id ||
    auth.hasPermission('problem.edit') ||
    auth.hasPermission('problem.delete') ||
    auth.hasPermission('system.admin') ||
    auth.hasAnyRole(['super_admin', 'admin'])
  )
}

async function load(): Promise<void> {
  if (!Number.isFinite(problemId.value) || problemId.value <= 0) {
    await router.replace('/404')
    return
  }

  loading.value = true
  error.value = undefined

  try {
    const data = await getProblem(problemId.value)
    problem.value = data.problem
    if (!canManageProblem(data.problem)) {
      await router.replace('/403')
    }
  } catch (err) {
    error.value = err
  } finally {
    loading.value = false
  }
}

async function handleSubmit(payload: ProblemFormInput): Promise<void> {
  if (!problem.value) {
    return
  }

  saving.value = true
  error.value = undefined

  try {
    const data = await updateProblem(problem.value.id, payload)
    problem.value = data.problem
    message.success('题目已保存')
    await router.push(`/problems/${problem.value.id}`)
  } catch (err) {
    error.value = err
  } finally {
    saving.value = false
  }
}

async function handleDelete(): Promise<void> {
  if (!problem.value) {
    return
  }

  deleting.value = true
  error.value = undefined

  try {
    await deleteProblem(problem.value.id)
    message.success('题目已删除')
    await router.push('/problems')
  } catch (err) {
    error.value = err
  } finally {
    deleting.value = false
  }
}

function cancel(): void {
  if (problem.value) {
    void router.push(`/problems/${problem.value.id}`)
    return
  }
  void router.push('/problems')
}

onMounted(() => {
  void load()
})
</script>

<template>
  <NSpace vertical size="large">
    <ApiErrorAlert :error="error" />
    <LoadingView v-if="loading && !problem" />
    <EmptyView v-else-if="!loading && !error && !problem" description="未找到题目" />

    <PageCard v-if="problem" :title="`编辑 ${problem.title}`">
      <template #headerExtra>
        <NSpace>
          <RouterLink :to="`/problems/${problem.id}`">
            <NButton secondary>查看</NButton>
          </RouterLink>
          <ConfirmAction
            label="删除"
            type="error"
            title="删除题目"
            content="已有提交记录的题目不能删除。此操作不可撤销。"
            :disabled="deleting || saving"
            @confirm="handleDelete"
          />
        </NSpace>
      </template>

      <ProblemForm
        mode="edit"
        :initial-value="problem"
        :loading="saving"
        @submit="handleSubmit"
        @cancel="cancel"
      />
    </PageCard>
  </NSpace>
</template>
