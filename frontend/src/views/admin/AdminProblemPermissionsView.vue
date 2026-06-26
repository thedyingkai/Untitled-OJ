<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import { useRoute } from 'vue-router'
import { NButton, NForm, NFormItem, NInputNumber, NSelect, NSpace, useMessage } from 'naive-ui'

import { addProblemRole, removeProblemRole } from '../../api/authAdmin'
import { toApiClientError } from '../../api/client'
import PageCard from '../../components/common/PageCard.vue'

const route = useRoute()
const message = useMessage()
const problemId = computed(() => Number(route.params.id))
const form = reactive({
  user_id: null as number | null,
  role: 'problem_owner',
})
const saving = ref(false)
const roleOptions = [
  { label: 'problem_owner', value: 'problem_owner' },
  { label: 'problem_setter', value: 'problem_setter' },
  { label: 'problem_viewer', value: 'problem_viewer' },
  { label: 'problem_data_manager', value: 'problem_data_manager' },
]

async function submit(add: boolean): Promise<void> {
  if (!form.user_id || !problemId.value) {
    message.warning('User ID is required')
    return
  }
  const payload = {
    user_id: form.user_id,
    problem_id: problemId.value,
    role: form.role,
  }
  saving.value = true
  try {
    if (add) {
      await addProblemRole(payload)
      message.success('Problem role granted')
    } else {
      await removeProblemRole(payload)
      message.success('Problem role removed')
    }
  } catch (err) {
    message.error(toApiClientError(err).message)
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <PageCard title="Problem Permissions">
    <NForm :model="form" label-placement="left" label-width="120">
      <NFormItem label="Problem ID">
        <NInputNumber :value="problemId" disabled />
      </NFormItem>
      <NFormItem label="User ID" required>
        <NInputNumber v-model:value="form.user_id" :min="1" />
      </NFormItem>
      <NFormItem label="Role" required>
        <NSelect v-model:value="form.role" :options="roleOptions" style="width: 220px" />
      </NFormItem>
      <NFormItem>
        <NSpace>
          <NButton type="primary" :loading="saving" @click="submit(true)">Grant</NButton>
          <NButton secondary :loading="saving" @click="submit(false)">Remove</NButton>
        </NSpace>
      </NFormItem>
    </NForm>
  </PageCard>
</template>
