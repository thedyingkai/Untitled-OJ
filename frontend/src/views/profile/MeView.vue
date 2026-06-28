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
      title="Profile"
      description="Current account identity, roles, and effective permission snapshot."
      eyebrow="Account"
    >
      <template #actions>
        <NButton secondary @click="auth.refreshCurrentUser()">刷新</NButton>
        <NButton tertiary @click="logout">Logout</NButton>
      </template>
    </OjosPageHeader>

    <OjosSection title="Identity">
      <NDescriptions bordered :column="1" label-placement="left">
        <NDescriptionsItem label="User ID">{{ auth.user?.user_id }}</NDescriptionsItem>
        <NDescriptionsItem label="Username">{{ auth.user?.username }}</NDescriptionsItem>
        <NDescriptionsItem label="Roles">
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
        <NDescriptionsItem label="Token">
          {{ auth.token ? 'Available' : 'Not signed in' }}
        </NDescriptionsItem>
      </NDescriptions>
    </OjosSection>

    <OjosSection title="Permission Snapshot">
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
