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
  NTag,
  NText,
  type MenuOption,
} from 'naive-ui'

import { userFrontendContributions } from '../ojos-frontend/shell-host'
import { useAuthStore } from '../stores/auth'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const collapsed = ref(false)

const canUseAdmin = computed(
  () => auth.hasAnyRole(['super_admin', 'admin']) || auth.hasAnyPermission(['system.admin']),
)

const menuOptions = computed<MenuOption[]>(() => {
  const primaryMenus = userFrontendContributions.value.menus.map((item) =>
    menuLink(item.path, item.title),
  )
  const options: MenuOption[] = [
    menuLink('/dashboard', '总览'),
    ...primaryMenus,
    menuLink('/me', '账号'),
  ]

  if (canUseAdmin.value) {
    options.push({
      key: 'admin',
      label: '管理',
      children: [
        menuLink('/admin/health', '健康'),
        menuLink('/admin/services/status', 'Service 状态'),
        menuLink('/admin/topology', 'Topology 只读'),
        menuLink('/admin/services/contributions', 'Service UI'),
        menuLink('/admin/users', '用户'),
        menuLink('/admin/permissions', '权限'),
        menuLink('/admin/permission-check', '权限检查'),
      ],
    })
  }

  return options
})

const selectedKey = computed(() => {
  if (route.path.startsWith('/admin/topology')) return '/admin/topology'
  if (route.path.startsWith('/admin/services/contributions')) return '/admin/services/contributions'
  if (route.path.startsWith('/admin/services/status')) return '/admin/services/status'
  return route.path
})

const expandedKeys = computed(() => (route.path.startsWith('/admin') ? ['admin'] : []))
const breadcrumbs = computed(() =>
  route.matched.filter((item) => item.meta.title).map((item) => String(item.meta.title)),
)

function menuLink(path: string, label: string): MenuOption {
  return {
    key: path,
    label: () => h(RouterLink, { to: path }, { default: () => label }),
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
      :width="248"
      :collapsed="collapsed"
      class="app-sider"
    >
      <div class="brand">
        <div class="brand-mark">OJ</div>
        <div class="brand-copy">
          <strong>OJOS</strong>
          <span>Web Shell</span>
        </div>
      </div>
      <NMenu :options="menuOptions" :value="selectedKey" :default-expanded-keys="expandedKeys" />
      <div class="sider-footer">安装、连接、启停和拓扑变更由 OJOS Orchestrator Web/TUI 处理。</div>
    </NLayoutSider>

    <NLayout class="app-main">
      <NLayoutHeader class="app-header">
        <div class="header-inner">
          <div class="header-left">
            <NButton size="small" secondary @click="collapsed = !collapsed">
              {{ collapsed ? '菜单' : '收起' }}
            </NButton>
            <div class="header-title">
              <h1>{{ route.meta.title || 'OJOS' }}</h1>
              <NBreadcrumb v-if="breadcrumbs.length > 1">
                <NBreadcrumbItem v-for="item in breadcrumbs" :key="item">
                  {{ item }}
                </NBreadcrumbItem>
              </NBreadcrumb>
            </div>
          </div>

          <div class="header-actions">
            <RouterLink v-if="canUseAdmin" to="/admin/health" class="header-link">
              健康
            </RouterLink>
            <NTag size="small" type="success">
              {{ auth.user?.username || 'user' }}
            </NTag>
            <NText class="role-text">
              {{ auth.roles.join(', ') || 'user' }}
            </NText>
            <NButton size="small" tertiary @click="logout">退出</NButton>
          </div>
        </div>
      </NLayoutHeader>

      <NLayoutContent class="app-content">
        <RouterView />
      </NLayoutContent>
    </NLayout>
  </NLayout>
</template>
