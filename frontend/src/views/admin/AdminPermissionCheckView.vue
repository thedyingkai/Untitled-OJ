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
    message.warning('请填写用户 ID 和权限')
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
      title="权限检查"
      description="使用用户、权限和可选作用域检查实时授权服务。"
      eyebrow="管理"
    />

    <OjosSection
      title="授权探测"
      description="检查由真实 Auth API 执行；空作用域默认视为 system。"
    >
      <OjosErrorState v-if="error" :error="error" />

      <NForm :model="form" label-placement="left" label-width="120" class="permission-check-form">
        <NFormItem label="用户 ID" required>
          <NInputNumber v-model:value="form.user_id" :min="1" />
        </NFormItem>
        <NFormItem label="权限" required>
          <NInput v-model:value="form.permission" placeholder="problem.edit" />
        </NFormItem>
        <NFormItem label="作用域类型">
          <NInput v-model:value="form.scope_type" placeholder="system 或 problem" />
        </NFormItem>
        <NFormItem label="作用域 ID">
          <NInputNumber v-model:value="form.scope_id" :min="0" />
        </NFormItem>
      </NForm>

      <OjosToolbar>
        <span class="muted-text">
          结果不会缓存在前端，也不会由前端模拟权限决策。
        </span>
        <template #actions>
          <NButton type="primary" :loading="loading" @click="submit">检查</NButton>
        </template>
      </OjosToolbar>

      <NResult
        v-if="allowed !== null"
        :status="allowed ? 'success' : '403'"
        :title="allowed ? '允许' : '拒绝'"
        :description="allowed ? '该用户当前拥有此权限。' : '该用户在此权限和作用域下未被授权。'"
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
