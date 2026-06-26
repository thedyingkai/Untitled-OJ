<script setup lang="ts">
import { reactive, ref } from 'vue'
import { NButton, NForm, NFormItem, NInput, NInputNumber, NResult, NSpace, useMessage } from 'naive-ui'

import { checkPermission } from '../../api/authAdmin'
import { toApiClientError, type ApiClientError } from '../../api/client'
import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import PageCard from '../../components/common/PageCard.vue'

const form = reactive({
  user_id: null as number | null,
  permission: '',
  scope_type: 'system',
  scope_id: 0,
})
const message = useMessage()
const loading = ref(false)
const error = ref<ApiClientError | null>(null)
const allowed = ref<boolean | null>(null)

async function submit(): Promise<void> {
  if (!form.user_id || !form.permission.trim()) {
    message.warning('User ID and permission are required')
    return
  }
  loading.value = true
  error.value = null
  allowed.value = null
  try {
    const result = await checkPermission({
      user_id: form.user_id,
      permission: form.permission.trim(),
      scope_type: form.scope_type.trim() || 'system',
      scope_id: form.scope_id,
    })
    allowed.value = result.allowed
  } catch (err) {
    error.value = toApiClientError(err)
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <PageCard title="Permission Check">
    <NSpace vertical size="large">
      <ApiErrorAlert v-if="error" :error="error" />
      <NForm :model="form" label-placement="left" label-width="120">
        <NFormItem label="User ID" required>
          <NInputNumber v-model:value="form.user_id" :min="1" />
        </NFormItem>
        <NFormItem label="Permission" required>
          <NInput v-model:value="form.permission" placeholder="problem.edit" />
        </NFormItem>
        <NFormItem label="Scope Type">
          <NInput v-model:value="form.scope_type" placeholder="system or problem" />
        </NFormItem>
        <NFormItem label="Scope ID">
          <NInputNumber v-model:value="form.scope_id" :min="0" />
        </NFormItem>
        <NFormItem>
          <NButton type="primary" :loading="loading" @click="submit">Check</NButton>
        </NFormItem>
      </NForm>

      <NResult
        v-if="allowed !== null"
        :status="allowed ? 'success' : '403'"
        :title="allowed ? 'Allowed' : 'Denied'"
      />
    </NSpace>
  </PageCard>
</template>
