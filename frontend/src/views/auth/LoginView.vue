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
  username: [{ required: true, message: 'Please enter your username', trigger: ['input', 'blur'] }],
  password: [{ required: true, message: 'Please enter your password', trigger: ['input', 'blur'] }],
}

async function submit(): Promise<void> {
  error.value = undefined
  await formRef.value?.validate()

  try {
    await auth.login(form)
    message.success('Signed in')
    const redirect = typeof route.query.redirect === 'string' ? route.query.redirect : '/dashboard'
    await router.push(redirect)
  } catch (err) {
    error.value = err
  }
}
</script>

<template>
  <main class="auth-page">
    <PageCard class="auth-card">
      <NSpace vertical size="large">
        <div class="auth-heading">
          <h1>Sign in to OJOS</h1>
          <p>Use your account to manage problems, submissions, and judge operations.</p>
        </div>

        <ApiErrorAlert :error="error" title="Sign in failed" />

        <NForm ref="formRef" :model="form" :rules="rules" label-placement="top" @submit.prevent>
          <NFormItem label="Username" path="username">
            <NInput
              v-model:value="form.username"
              autocomplete="username"
              placeholder="admin1"
              size="large"
            />
          </NFormItem>
          <NFormItem label="Password" path="password">
            <NInput
              v-model:value="form.password"
              type="password"
              autocomplete="current-password"
              placeholder="Password"
              show-password-on="click"
              size="large"
              @keydown.enter.prevent="submit"
            />
          </NFormItem>
          <NButton type="primary" block size="large" :loading="auth.loading" @click="submit">
            Sign in
          </NButton>
        </NForm>

        <NText depth="3">
          Need an account?
          <RouterLink to="/register" class="header-link">Create one</RouterLink>
        </NText>
      </NSpace>
    </PageCard>
  </main>
</template>
