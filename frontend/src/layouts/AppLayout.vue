<script setup lang="ts">
import { computed, h, ref } from 'vue'
import { RouterLink, RouterView, useRoute, useRouter } from 'vue-router'
import {
  NBreadcrumb,
  NBreadcrumbItem,
  NButton,
  NLayout,
  NLayoutContent,
  NLayoutHeader,
  NLayoutSider,
  NMenu,
  NSpace,
  NTag,
  NText,
  type MenuOption,
} from 'naive-ui'

import { useAuthStore } from '../stores/auth'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const collapsed = ref(false)

const canUseAdmin = computed(
  () => auth.hasAnyRole(['super_admin', 'admin']) || auth.hasAnyPermission(['system.admin']),
)

const menuOptions = computed<MenuOption[]>(() => {
  const options: MenuOption[] = [
    menuLink('/dashboard', 'Dashboard'),
    menuLink('/problems', 'Problems'),
    menuLink('/submissions', 'Submissions'),
    menuLink('/me', 'Profile'),
  ]

  if (canUseAdmin.value) {
    options.push({
      key: 'admin',
      label: 'Admin',
      children: [
        menuLink('/admin/health', 'Service Health'),
        menuLink('/admin/judge', 'Judge Cluster'),
        menuLink('/admin/modules', 'Modules'),
        menuLink('/admin/modules/topology', 'Module Topology'),
        menuLink('/admin/users', 'Users'),
        menuLink('/admin/permissions', 'Permissions'),
        menuLink('/admin/permission-check', 'Permission Check'),
      ],
    })
  }

  return options
})

const selectedKeys = computed(() => [route.path])
const breadcrumbs = computed(() =>
  route.matched.filter((item) => item.meta.title).map((item) => String(item.meta.title)),
)

function menuLink(path: string, label: string): MenuOption {
  return {
    key: path,
    label: () =>
      h(
        RouterLink,
        { to: path },
        {
          default: () => label,
        },
      ),
  }
}

function logout(): void {
  auth.logout()
  void router.push({ name: 'login' })
}
</script>

<template>
  <NLayout has-sider class="app-shell">
    <NLayoutSider
      bordered
      collapse-mode="width"
      :collapsed-width="0"
      :width="232"
      :collapsed="collapsed"
      class="app-sider"
    >
      <div class="brand">
        <strong>OJOS</strong>
        <span>Distributed Judge</span>
      </div>
      <NMenu :options="menuOptions" :value="selectedKeys[0]" />
    </NLayoutSider>

    <NLayout class="app-main">
      <NLayoutHeader bordered class="app-header">
        <NSpace align="center" justify="space-between" :wrap="false">
          <NSpace align="center" :wrap="false">
            <NButton size="small" quaternary @click="collapsed = !collapsed">
              {{ collapsed ? 'Menu' : 'Collapse' }}
            </NButton>
            <div>
              <h1>{{ route.meta.title || 'OJOS' }}</h1>
              <NBreadcrumb v-if="breadcrumbs.length > 1">
                <NBreadcrumbItem v-for="item in breadcrumbs" :key="item">
                  {{ item }}
                </NBreadcrumbItem>
              </NBreadcrumb>
            </div>
          </NSpace>

          <NSpace align="center" :wrap="false">
            <RouterLink v-if="canUseAdmin" to="/admin/health" class="header-link">
              Health
            </RouterLink>
            <NTag size="small" type="success" round>
              {{ auth.user?.username }}
            </NTag>
            <NText depth="3" class="role-text">
              {{ auth.roles.join(', ') || 'user' }}
            </NText>
            <NButton size="small" secondary @click="logout">Logout</NButton>
          </NSpace>
        </NSpace>
      </NLayoutHeader>

      <NLayoutContent class="app-content">
        <RouterView />
      </NLayoutContent>
    </NLayout>
  </NLayout>
</template>
