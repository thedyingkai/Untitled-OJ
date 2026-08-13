import type { RouteRecordRaw } from 'vue-router'
import { createRouter, createWebHistory } from 'vue-router'

import { useAuthStore } from '../stores/auth'

declare module 'vue-router' {
  interface RouteMeta {
    title?: string
    requiresAuth?: boolean
    guestOnly?: boolean
    roles?: string[]
    permissions?: string[]
  }
}

const adminPermissions = ['system.admin']
const adminRoles = ['super_admin', 'admin']

const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    component: () => import('../views/auth/LoginView.vue'),
    meta: { title: '登录', guestOnly: true },
  },
  {
    path: '/register',
    name: 'register',
    component: () => import('../views/auth/RegisterView.vue'),
    meta: { title: '注册', guestOnly: true },
  },
  {
    path: '/',
    name: 'app-shell',
    component: () => import('../layouts/AppLayout.vue'),
    redirect: '/dashboard',
    meta: { requiresAuth: true },
    children: [
      {
        path: 'dashboard',
        name: 'dashboard',
        component: () => import('../views/dashboard/DashboardView.vue'),
        meta: { title: '总览', requiresAuth: true },
      },
      {
        path: 'me',
        name: 'me',
        component: () => import('../views/profile/MeView.vue'),
        meta: { title: '账号', requiresAuth: true },
      },
      {
        path: 'admin/health',
        name: 'admin-health',
        component: () => import('../views/admin/AdminHealthView.vue'),
        meta: { title: '服务健康', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/services',
        name: 'admin-services',
        component: () => import('../views/admin/AdminServicesView.vue'),
        meta: { title: 'Services', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/services/status',
        name: 'admin-service-status-services',
        component: () => import('../views/admin/AdminServiceStatusView.vue'),
        meta: { title: 'Service 状态', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/services/status/:serviceId',
        name: 'admin-service-status-detail',
        component: () => import('../views/admin/AdminServiceStatusView.vue'),
        meta: {
          title: 'Service 状态详情',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/services/:id',
        name: 'admin-service-detail',
        component: () => import('../views/admin/AdminServiceDetailView.vue'),
        meta: { title: 'Service 详情', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/topology',
        name: 'admin-service-topology',
        component: () => import('../views/admin/AdminServiceTopologyView.vue'),
        meta: { title: 'Topology 只读视图', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/services/contributions',
        name: 'admin-service-contributions',
        component: () => import('../views/admin/AdminServiceContributionsView.vue'),
        meta: { title: 'Service UI Snapshot', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/services/contributions/:serviceId',
        name: 'admin-service-contribution-detail',
        component: () => import('../views/admin/AdminServiceContributionsView.vue'),
        meta: {
          title: 'Service UI Snapshot 详情',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/users',
        name: 'admin-users',
        component: () => import('../views/admin/AdminUsersView.vue'),
        meta: { title: '用户', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/permissions',
        name: 'admin-permissions',
        component: () => import('../views/admin/AdminPermissionsView.vue'),
        meta: { title: '权限', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
      {
        path: 'admin/permission-check',
        name: 'admin-permission-check',
        component: () => import('../views/admin/AdminPermissionCheckView.vue'),
        meta: { title: '权限检查', requiresAuth: true, roles: adminRoles, permissions: adminPermissions },
      },
    ],
  },
  {
    path: '/403',
    name: 'forbidden',
    component: () => import('../views/errors/ForbiddenView.vue'),
    meta: { title: '无权访问' },
  },
  {
    path: '/500',
    name: 'server-error',
    component: () => import('../views/errors/ServerErrorView.vue'),
    meta: { title: '服务错误' },
  },
  {
    path: '/:pathMatch(.*)*',
    name: 'not-found',
    component: () => import('../views/errors/NotFoundView.vue'),
    meta: { title: '页面不存在' },
  },
]

export const router = createRouter({
  history: createWebHistory(),
  routes,
})

router.beforeEach(async (to) => {
  const auth = useAuthStore()

  if (!auth.initialized) {
    await auth.restore()
  }

  if (to.meta.guestOnly && auth.isAuthenticated) {
    return { name: 'dashboard' }
  }

  if (to.meta.requiresAuth && !auth.isAuthenticated) {
    return { name: 'login', query: { redirect: to.fullPath } }
  }

  const allowedByRole = !to.meta.roles?.length || auth.hasAnyRole(to.meta.roles)
  const allowedByPermission =
    !to.meta.permissions?.length || auth.hasAnyPermission(to.meta.permissions)

  if (to.meta.requiresAuth && !allowedByRole && !allowedByPermission) {
    return { name: 'forbidden' }
  }

  return true
})

router.afterEach((to) => {
  const title = to.meta.title ? `${to.meta.title} - OJOS` : 'OJOS'
  document.title = title
})
