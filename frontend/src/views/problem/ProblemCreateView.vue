<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { NSpace, useMessage } from 'naive-ui'

import { createProblem } from '../../api/problem'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import PageCard from '../../components/common/PageCard.vue'
import ProblemForm from '../../components/problem/ProblemForm.vue'
import type { ProblemFormInput } from '../../types/problem'

const router = useRouter()
const message = useMessage()
const saving = ref(false)
const error = ref<unknown>()

async function handleSubmit(payload: ProblemFormInput): Promise<void> {
  saving.value = true
  error.value = undefined

  try {
    const data = await createProblem(payload)
    message.success('题目已创建')
    await router.push(`/problems/${data.problem_id}`)
  } catch (err) {
    error.value = err
  } finally {
    saving.value = false
  }
}

function cancel(): void {
  void router.push('/problems')
}
</script>

<template>
  <NSpace vertical size="large">
    <ApiErrorAlert :error="error" />
    <PageCard title="新建题目">
      <ProblemForm mode="create" :loading="saving" @submit="handleSubmit" @cancel="cancel" />
    </PageCard>
  </NSpace>
</template>
