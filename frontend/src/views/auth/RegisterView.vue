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
    { required: true, message: '请输入用户名', trigger: ['input', 'blur'] },
    { min: 3, max: 32, message: '用户名长度必须为 3 到 32 个字符', trigger: ['input', 'blur'] },
  ],
  email: [{ type: 'email', message: '请输入有效邮箱', trigger: ['input', 'blur'] }],
  password: [
    { required: true, message: '请输入密码', trigger: ['input', 'blur'] },
    { min: 6, message: '密码至少 6 个字符', trigger: ['input', 'blur'] },
  ],
  confirmPassword: [
    { required: true, message: '请再次输入密码', trigger: ['input', 'blur'] },
    {
      validator: (_rule, value: string) => value === form.password,
      message: '两次输入的密码不一致',
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
    message.success('账号已创建')
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
          <h1>创建 OJOS 账号</h1>
          <p>注册后可以浏览题目、提交代码并查看评测结果。</p>
        </div>

        <ApiErrorAlert :error="error" title="注册失败" />

        <NForm ref="formRef" :model="form" :rules="rules" label-placement="top" @submit.prevent>
          <NFormItem label="用户名" path="username">
            <NInput v-model:value="form.username" autocomplete="username" size="large" />
          </NFormItem>
          <NFormItem label="邮箱" path="email">
            <NInput v-model:value="form.email" autocomplete="email" size="large" />
          </NFormItem>
          <NFormItem label="密码" path="password">
            <NInput
              v-model:value="form.password"
              type="password"
              autocomplete="new-password"
              show-password-on="click"
              size="large"
            />
          </NFormItem>
          <NFormItem label="确认密码" path="confirmPassword">
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
            创建账号
          </NButton>
        </NForm>

        <NText depth="3">
          已有账号？
          <RouterLink to="/login" class="header-link">去登录</RouterLink>
        </NText>
      </NSpace>
    </PageCard>
  </main>
</template>
