<script setup lang="ts">
import { reactive, ref } from 'vue'
import { RouterLink, useRouter } from 'vue-router'
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

interface RegisterForm {
  username: string
  email: string
  password: string
  confirmPassword: string
}

const router = useRouter()
const message = useMessage()
const auth = useAuthStore()
const formRef = ref<FormInst | null>(null)
const error = ref<unknown>()

const form = reactive<RegisterForm>({
  username: '',
  email: '',
  password: '',
  confirmPassword: '',
})

const rules: FormRules = {
  username: [
    { required: true, message: 'Please enter a username', trigger: ['input', 'blur'] },
    { min: 3, max: 32, message: 'Username must be 3 to 32 characters', trigger: ['input', 'blur'] },
  ],
  email: [{ type: 'email', message: 'Please enter a valid email', trigger: ['input', 'blur'] }],
  password: [
    { required: true, message: 'Please enter a password', trigger: ['input', 'blur'] },
    { min: 6, message: 'Password must be at least 6 characters', trigger: ['input', 'blur'] },
  ],
  confirmPassword: [
    { required: true, message: 'Please confirm your password', trigger: ['input', 'blur'] },
    {
      validator: (_rule, value: string) => value === form.password,
      message: 'Passwords do not match',
      trigger: ['input', 'blur'],
    },
  ],
}

async function submit(): Promise<void> {
  error.value = undefined
  await formRef.value?.validate()

  try {
    await auth.register({
      username: form.username,
      email: form.email || undefined,
      password: form.password,
    })
    message.success('Account created')
    await router.push({ name: 'login' })
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
          <h1>Create an OJOS account</h1>
          <p>Register a user account for problem browsing and submissions.</p>
        </div>

        <ApiErrorAlert :error="error" title="Registration failed" />

        <NForm ref="formRef" :model="form" :rules="rules" label-placement="top" @submit.prevent>
          <NFormItem label="Username" path="username">
            <NInput v-model:value="form.username" autocomplete="username" size="large" />
          </NFormItem>
          <NFormItem label="Email" path="email">
            <NInput v-model:value="form.email" autocomplete="email" size="large" />
          </NFormItem>
          <NFormItem label="Password" path="password">
            <NInput
              v-model:value="form.password"
              type="password"
              autocomplete="new-password"
              show-password-on="click"
              size="large"
            />
          </NFormItem>
          <NFormItem label="Confirm password" path="confirmPassword">
            <NInput
              v-model:value="form.confirmPassword"
              type="password"
              autocomplete="new-password"
              show-password-on="click"
              size="large"
              @keydown.enter.prevent="submit"
            />
          </NFormItem>
          <NButton type="primary" block size="large" :loading="auth.loading" @click="submit">
            Create account
          </NButton>
        </NForm>

        <NText depth="3">
          Already registered?
          <RouterLink to="/login" class="header-link">Sign in</RouterLink>
        </NText>
      </NSpace>
    </PageCard>
  </main>
</template>
