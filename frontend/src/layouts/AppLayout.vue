<script setup lang="ts">
import { computed, h, onMounted, ref, watch } from 'vue'
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

import { getServiceRuntimeSnapshot } from '../api/services'
import { useAuthStore } from '../stores/auth'
import type { ServiceMenuItem } from '../types/service'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const collapsed = ref(false)
const runtimeMenus = ref<ServiceMenuItem[]>([])

const canUseAdmin = computed(
  () => auth.hasAnyRole(['super_admin', 'admin']) || auth.hasAnyPermission(['system.admin']),
)

const menuOptions = computed<MenuOption[]>(() => {
  const primaryMenus = runtimeMenuOptions(
    runtimeMenus.value.filter((item) => !item.route_path.startsWith('/admin')),
  )
  const options: MenuOption[] = [
    menuLink('/dashboard', '总览'),
    ...(primaryMenus.length > 0
      ? primaryMenus
      : [menuLink('/problems', '题库'), menuLink('/submissions', '提交')]),
    menuLink('/me', '账号'),
  ]

  if (canUseAdmin.value) {
    options.push({
      key: 'admin',
      label: '管理',
      children: [
        menuLink('/admin/health', '健康'),
        ...runtimeMenuOptions(runtimeMenus.value.filter((item) => item.route_path.startsWith('/admin'))),
        menuLink('/admin/runtime/services', 'Runtime 服务'),
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
  if (route.path.startsWith('/admin/runtime/services')) return '/admin/runtime/services'
  if (route.path.startsWith('/problems')) return '/problems'
  if (route.path.startsWith('/submissions')) return '/submissions'
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

function runtimeMenuOptions(items: ServiceMenuItem[]): MenuOption[] {
  const seen = new Set<string>()
  return items
    .filter((item) => item.enabled)
    .filter((item) => canUseMenu(item))
    .sort((a, b) => a.sort_order - b.sort_order || a.menu_key.localeCompare(b.menu_key))
    .flatMap((item) => {
      if (seen.has(item.route_path)) return []
      seen.add(item.route_path)
      const routePath = routeExists(item.route_path)
        ? item.route_path
        : `/admin/services/contributions/${encodeURIComponent(item.service_id)}`
      return [menuLink(routePath, item.title)]
    })
}

function routeExists(path: string): boolean {
  return router.getRoutes().some((item) => item.path === path || item.path.replace(/\/:.*$/, '') === path)
}

function canUseMenu(item: ServiceMenuItem): boolean {
  if (!item.required_permission) return true
  return auth.hasAnyPermission([item.required_permission, 'system.admin'])
}

async function loadRuntimeMenus(): Promise<void> {
  if (!canUseAdmin.value && !auth.isAuthenticated) {
    runtimeMenus.value = []
    return
  }
  try {
    const snapshot = await getServiceRuntimeSnapshot()
    runtimeMenus.value = snapshot.menus
  } catch {
    runtimeMenus.value = []
  }
}

function logout(): void {
  auth.logout()
  void router.push({ name: 'login' })
}

watch(
  () => [auth.isAuthenticated, auth.permissions.join(','), auth.roles.join(',')],
  () => void loadRuntimeMenus(),
)

onMounted(() => void loadRuntimeMenus())
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
          <span>Service Runtime</span>
        </div>
      </div>
      <NMenu :options="menuOptions" :value="selectedKey" :default-expanded-keys="expandedKeys" />
      <div class="sider-footer">安装、热插拔和全局拓扑变更请使用 Root Installer GUI/TUI/CLI</div>
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
