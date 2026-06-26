<script setup lang="ts">
import { reactive, ref } from 'vue'
import { NButton, NForm, NFormItem, NInput, NInputNumber, NResult, NSpace, useMessage } from 'naive-ui'

import { checkPermission } from '../../api/authAdmin'
import { toApiClientError, type ApiClientError } from '../../api/client'
import OjosErrorState from '../../components/oj/OjosErrorState.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosPermissionTag from '../../components/oj/OjosPermissionTag.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import OjosToolbar from '../../components/oj/OjosToolbar.vue'

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
  <div class="permission-check-page">
    <OjosPageHeader
      title="Permission Check"
      description="Probe the live authorization service with a user, permission, and optional scope."
      eyebrow="Admin"
    />

    <OjosSection
      title="Authorization Probe"
      description="The check is evaluated by the real Auth API. Empty scope defaults to system scope."
    >
      <OjosErrorState v-if="error" :error="error" />

      <NForm :model="form" label-placement="left" label-width="120" class="permission-check-form">
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
      </NForm>

      <OjosToolbar>
        <span class="muted-text">
          Result is not cached in the frontend and no permission decision is simulated.
        </span>
        <template #actions>
          <NButton type="primary" :loading="loading" @click="submit">Check</NButton>
        </template>
      </OjosToolbar>

      <NResult
        v-if="allowed !== null"
        :status="allowed ? 'success' : '403'"
        :title="allowed ? 'Allowed' : 'Denied'"
        :description="allowed ? 'The user currently has this permission.' : 'The user is not authorized for this permission and scope.'"
      >
        <template #footer>
          <NSpace justify="center">
            <OjosPermissionTag :permission="form.permission" />
          </NSpace>
        </template>
      </NResult>
    </OjosSection>
  </div>
</template>

<style scoped>
.permission-check-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.permission-check-form {
  max-width: 760px;
}

.muted-text {
  color: var(--muted);
  font-size: 12px;
}
</style>
