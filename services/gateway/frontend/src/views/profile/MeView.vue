<script setup lang="ts">
import { useRouter } from 'vue-router'
import { NButton, NDescriptions, NDescriptionsItem, NSpace, NTag } from 'naive-ui'

import OjosJsonViewer from '../../components/oj/OjosJsonViewer.vue'
import OjosPageHeader from '../../components/oj/OjosPageHeader.vue'
import OjosSection from '../../components/oj/OjosSection.vue'
import { useAuthStore } from '../../stores/auth'

const router = useRouter()
const auth = useAuthStore()

function logout(): void {
  auth.logout()
  void router.push({ name: 'login' })
}
</script>

<template>
  <div class="profile-page">
    <OjosPageHeader
      title="账号"
      description="当前账号身份、角色和生效权限快照。"
      eyebrow="Account"
    >
      <template #actions>
        <NButton secondary @click="auth.refreshCurrentUser()">刷新</NButton>
        <NButton tertiary @click="logout">退出</NButton>
      </template>
    </OjosPageHeader>

    <OjosSection title="身份">
      <NDescriptions bordered :column="1" label-placement="left">
        <NDescriptionsItem label="用户 ID">{{ auth.user?.user_id }}</NDescriptionsItem>
        <NDescriptionsItem label="用户名">{{ auth.user?.username }}</NDescriptionsItem>
        <NDescriptionsItem label="角色">
          <NSpace v-if="auth.roles.length">
            <NTag v-for="role in auth.roles" :key="role" size="small">{{ role }}</NTag>
          </NSpace>
          <span v-else>-</span>
        </NDescriptionsItem>
        <NDescriptionsItem label="权限">
          <NSpace v-if="auth.permissions.length">
            <NTag v-for="permission in auth.permissions" :key="permission" size="small">
              {{ permission }}
            </NTag>
          </NSpace>
          <span v-else>-</span>
        </NDescriptionsItem>
        <NDescriptionsItem label="登录凭据">
          {{ auth.token ? '已保存' : '未登录' }}
        </NDescriptionsItem>
      </NDescriptions>
    </OjosSection>

    <OjosSection title="权限快照">
      <OjosJsonViewer :value="{ roles: auth.roles, permissions: auth.permissions }" />
    </OjosSection>
  </div>
</template>

<style scoped>
.profile-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
</style>
