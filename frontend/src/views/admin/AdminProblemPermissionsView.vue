<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import { useRoute } from 'vue-router'
import { NButton, NForm, NFormItem, NInputNumber, NSelect, NSpace, useMessage } from 'naive-ui'

import { addProblemRole, removeProblemRole } from '../../api/authAdmin'
import { toApiClientError } from '../../api/client'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosRoleTag from '../../components/oj/OjosRoleTag.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosToolbar from '../../components/oj/OjosToolbar.vue'

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
  <div class="problem-permissions-page">
    <OjosPageHeader
      title="Problem Permissions"
      :description="`Grant or remove scoped roles for problem #${problemId}.`"
      eyebrow="Admin"
    />

    <OjosSection
      title="Scoped Role Binding"
      description="Problem roles are applied through Auth admin APIs and are not stored in the frontend."
    >
      <NForm :model="form" label-placement="left" label-width="120" class="problem-permission-form">
      <NFormItem label="Problem ID">
        <NInputNumber :value="problemId" disabled />
      </NFormItem>
      <NFormItem label="User ID" required>
        <NInputNumber v-model:value="form.user_id" :min="1" />
      </NFormItem>
      <NFormItem label="Role" required>
        <NSelect v-model:value="form.role" :options="roleOptions" style="width: 220px" />
      </NFormItem>
    </NForm>

      <OjosToolbar>
        <NSpace align="center">
          <span class="muted-text">Selected role</span>
          <OjosRoleTag :role="form.role" />
        </NSpace>
        <template #actions>
          <NButton type="primary" :loading="saving" @click="submit(true)">Grant</NButton>
          <NButton secondary :loading="saving" @click="submit(false)">Remove</NButton>
        </template>
      </OjosToolbar>
    </OjosSection>
  </div>
</template>

<style scoped>
.problem-permissions-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.problem-permission-form {
  max-width: 720px;
}

.muted-text {
  color: var(--muted);
  font-size: 12px;
}
</style>
