<script setup lang="ts">
import { reactive, ref } from 'vue'
import { RouterLink, useRoute, useRouter } from 'vue-router'
import {
  NButton,
  NForm,
  NFormItem,
  NInput,
  NSpace,
  NText,
  useMessage,
  type FormInst,
  type FormRules,
} from 'naive-ui'

import ApiErrorAlert from '../../components/common/ApiErrorAlert.vue'
import PageCard from '../../components/common/PageCard.vue'
import { useAuthStore } from '../../stores/auth'
import type { LoginRequest } from '../../types/auth'

const route = useRoute()
const router = useRouter()
const message = useMessage()
const auth = useAuthStore()
const formRef = ref<FormInst | null>(null)
const error = ref<unknown>()

const form = reactive<LoginRequest>({
  username: '',
  password: '',
})

const rules: FormRules = {
  username: [{ required: true, message: '请输入用户名', trigger: ['input', 'blur'] }],
  password: [{ required: true, message: '请输入密码', trigger: ['input', 'blur'] }],
}

async function submit(): Promise<void> {
  error.value = undefined
  await formRef.value?.validate()

  try {
    await auth.login(form)
    message.success('登录成功')
    const redirect = typeof route.query.redirect === 'string' ? route.query.redirect : '/dashboard'
    await router.push(redirect)
  } catch (err) {
    error.value = err
  }
}
</script>

<template>
  <main class="auth-page">
    <PageCard title="登录 OJOS" class="auth-card">
      <NSpace vertical size="large">
        <ApiErrorAlert :error="error" title="登录失败" />

        <NForm ref="formRef" :model="form" :rules="rules" label-placement="top" @submit.prevent>
          <NFormItem label="用户名" path="username">
            <NInput v-model:value="form.username" autocomplete="username" />
          </NFormItem>
          <NFormItem label="密码" path="password">
            <NInput
              v-model:value="form.password"
              type="password"
              autocomplete="current-password"
              show-password-on="click"
              @keydown.enter.prevent="submit"
            />
          </NFormItem>
          <NButton type="primary" block :loading="auth.loading" @click="submit">
            登录
          </NButton>
        </NForm>

        <NText depth="3">
          没有账号？<RouterLink to="/register">注册</RouterLink>
        </NText>
      </NSpace>
    </PageCard>
  </main>
</template>
