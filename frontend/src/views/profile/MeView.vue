<script setup lang="ts">
import { useRouter } from 'vue-router'
import { NButton, NDescriptions, NDescriptionsItem, NSpace, NTag } from 'naive-ui'

import JsonViewer from '../../components/common/JsonViewer.vue'
import PageCard from '../../components/common/PageCard.vue'
import { useAuthStore } from '../../stores/auth'

const router = useRouter()
const auth = useAuthStore()

function logout(): void {
  auth.logout()
  void router.push({ name: 'login' })
}
</script>

<template>
  <NSpace vertical size="large">
    <PageCard title="个人信息">
      <NDescriptions bordered :column="1" label-placement="left">
        <NDescriptionsItem label="用户 ID">{{ auth.user?.user_id }}</NDescriptionsItem>
        <NDescriptionsItem label="用户名">{{ auth.user?.username }}</NDescriptionsItem>
        <NDescriptionsItem label="角色">
          <NSpace>
            <NTag v-for="role in auth.roles" :key="role" size="small">{{ role }}</NTag>
          </NSpace>
        </NDescriptionsItem>
        <NDescriptionsItem label="权限">
          <NSpace v-if="auth.permissions.length">
            <NTag v-for="permission in auth.permissions" :key="permission" size="small">
              {{ permission }}
            </NTag>
          </NSpace>
          <span v-else>-</span>
        </NDescriptionsItem>
        <NDescriptionsItem label="Token 状态">
          {{ auth.token ? '有效' : '未登录' }}
        </NDescriptionsItem>
      </NDescriptions>
    </PageCard>

    <PageCard title="权限调试">
      <JsonViewer :value="{ roles: auth.roles, permissions: auth.permissions }" />
    </PageCard>

    <NSpace>
      <NButton type="primary" ghost @click="auth.refreshCurrentUser()">刷新当前用户</NButton>
      <NButton secondary @click="logout">退出登录</NButton>
    </NSpace>
  </NSpace>
</template>
