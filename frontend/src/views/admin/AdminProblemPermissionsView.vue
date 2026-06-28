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
    message.warning('请填写用户 ID')
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
      message.success('题目角色已授予')
    } else {
      await removeProblemRole(payload)
      message.success('题目角色已移除')
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
      title="题目权限"
      :description="`授予或移除题目 #${problemId} 的作用域角色。`"
      eyebrow="管理"
    />

    <OjosSection
      title="作用域角色绑定"
      description="题目角色通过 Auth 管理 API 生效，不存储在前端。"
    >
      <NForm :model="form" label-placement="left" label-width="120" class="problem-permission-form">
      <NFormItem label="题目 ID">
        <NInputNumber :value="problemId" disabled />
      </NFormItem>
      <NFormItem label="用户 ID" required>
        <NInputNumber v-model:value="form.user_id" :min="1" />
      </NFormItem>
      <NFormItem label="角色" required>
        <NSelect v-model:value="form.role" :options="roleOptions" style="width: 220px" />
      </NFormItem>
    </NForm>

      <OjosToolbar>
        <NSpace align="center">
          <span class="muted-text">当前角色</span>
          <OjosRoleTag :role="form.role" />
        </NSpace>
        <template #actions>
          <NButton type="primary" :loading="saving" @click="submit(true)">授予</NButton>
          <NButton secondary :loading="saving" @click="submit(false)">移除</NButton>
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
