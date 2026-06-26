<script setup lang="ts">
import { RouterLink } from 'vue-router'
import { NCard, NGi, NGrid, NSpace, NText } from 'naive-ui'

import PageCard from '../../components/common/PageCard.vue'
import PermissionGuard from '../../components/common/PermissionGuard.vue'
import { useAuthStore } from '../../stores/auth'

const auth = useAuthStore()
</script>

<template>
  <NSpace vertical size="large">
    <PageCard title="Current User">
      <NText>{{ auth.user?.username }}</NText>
      <NText depth="3" class="inline-meta">
        ID {{ auth.user?.user_id }} / {{ auth.roles.join(', ') || 'user' }}
      </NText>
    </PageCard>

    <NGrid :cols="3" :x-gap="16" :y-gap="16" responsive="screen">
      <NGi>
        <RouterLink to="/problems" class="nav-card">
          <NCard title="Problems" hoverable>
            <NText depth="3">Browse and manage accessible problems.</NText>
          </NCard>
        </RouterLink>
      </NGi>
      <NGi>
        <RouterLink to="/submissions" class="nav-card">
          <NCard title="Submissions" hoverable>
            <NText depth="3">Review judging history and results.</NText>
          </NCard>
        </RouterLink>
      </NGi>
      <NGi>
        <RouterLink to="/me" class="nav-card">
          <NCard title="Account" hoverable>
            <NText depth="3">Inspect roles, permissions, and token state.</NText>
          </NCard>
        </RouterLink>
      </NGi>

      <PermissionGuard :roles="['super_admin', 'admin']" :permissions="['system.admin']">
        <NGi>
          <RouterLink to="/admin/judge" class="nav-card">
            <NCard title="Judge" hoverable>
              <NText depth="3">Monitor queue and worker state.</NText>
            </NCard>
          </RouterLink>
        </NGi>
        <NGi>
          <RouterLink to="/admin/permissions" class="nav-card">
            <NCard title="Permissions" hoverable>
              <NText depth="3">Manage roles and resource grants.</NText>
            </NCard>
          </RouterLink>
        </NGi>
        <NGi>
          <RouterLink to="/admin/health" class="nav-card">
            <NCard title="Health" hoverable>
              <NText depth="3">Check services and infrastructure.</NText>
            </NCard>
          </RouterLink>
        </NGi>
        <NGi>
          <RouterLink to="/admin/modules" class="nav-card">
            <NCard title="Modules" hoverable>
              <NText depth="3">Inspect builtin modules and topology.</NText>
            </NCard>
          </RouterLink>
        </NGi>
      </PermissionGuard>
    </NGrid>
  </NSpace>
</template>
