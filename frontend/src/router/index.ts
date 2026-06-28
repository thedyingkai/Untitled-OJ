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
        path: 'problems',
        name: 'problems',
        component: () => import('../views/problem/ProblemListView.vue'),
        meta: { title: '题目', requiresAuth: true },
      },
      {
        path: 'problems/new',
        name: 'problem-create',
        component: () => import('../views/problem/ProblemCreateView.vue'),
        meta: {
          title: '新建题目',
          requiresAuth: true,
          roles: adminRoles,
          permissions: ['problem.create', 'system.admin'],
        },
      },
      {
        path: 'problems/:id/edit',
        name: 'problem-edit',
        component: () => import('../views/problem/ProblemEditView.vue'),
        meta: { title: '编辑题目', requiresAuth: true },
      },
      {
        path: 'problems/:id/package',
        name: 'problem-package',
        component: () => import('../views/problem/ProblemPackageView.vue'),
        meta: { title: '题目数据包', requiresAuth: true },
      },
      {
        path: 'problems/:id/submit',
        name: 'problem-submit',
        component: () => import('../views/judge/ProblemSubmitView.vue'),
        meta: { title: '提交代码', requiresAuth: true },
      },
      {
        path: 'problems/:id',
        name: 'problem-detail',
        component: () => import('../views/problem/ProblemDetailView.vue'),
        meta: { title: '题目详情', requiresAuth: true },
      },
      {
        path: 'submissions',
        name: 'submissions',
        component: () => import('../views/judge/SubmissionsListView.vue'),
        meta: { title: '提交记录', requiresAuth: true },
      },
      {
        path: 'submissions/:id',
        name: 'submission-detail',
        component: () => import('../views/judge/SubmissionDetailView.vue'),
        meta: { title: '提交详情', requiresAuth: true },
      },
      {
        path: 'admin/health',
        name: 'admin-health',
        component: () => import('../views/admin/AdminHealthView.vue'),
        meta: {
          title: '服务健康',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/judge',
        name: 'admin-judge',
        component: () => import('../views/admin/AdminJudgeView.vue'),
        meta: {
          title: '评测集群',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/modules',
        name: 'admin-modules',
        component: () => import('../views/admin/AdminModulesView.vue'),
        meta: {
          title: '模块中心',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/modules/topology',
        name: 'admin-module-topology',
        component: () => import('../views/admin/AdminModuleTopologyView.vue'),
        meta: {
          title: '模块拓扑',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/modules/installer',
        name: 'admin-module-installer',
        component: () => import('../views/admin/AdminModuleInstallerView.vue'),
        meta: {
          title: '安装器视图',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/modules/contributions',
        name: 'admin-module-contributions',
        component: () => import('../views/admin/AdminModuleContributionsView.vue'),
        meta: {
          title: '模块贡献',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/modules/contributions/:moduleId',
        name: 'admin-module-contribution-detail',
        component: () => import('../views/admin/AdminModuleContributionsView.vue'),
        meta: {
          title: '模块贡献详情',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/runtime/services',
        name: 'admin-runtime-services',
        component: () => import('../views/admin/AdminRuntimeServicesView.vue'),
        meta: {
          title: 'Runtime 服务',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/runtime/services/:serviceId',
        name: 'admin-runtime-service-detail',
        component: () => import('../views/admin/AdminRuntimeServicesView.vue'),
        meta: {
          title: 'Runtime 服务详情',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/modules/:id',
        name: 'admin-module-detail',
        component: () => import('../views/admin/AdminModuleDetailView.vue'),
        meta: {
          title: '模块详情',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/users',
        name: 'admin-users',
        component: () => import('../views/admin/AdminUsersView.vue'),
        meta: {
          title: '用户',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/permissions',
        name: 'admin-permissions',
        component: () => import('../views/admin/AdminPermissionsView.vue'),
        meta: {
          title: '权限',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/permission-check',
        name: 'admin-permission-check',
        component: () => import('../views/admin/AdminPermissionCheckView.vue'),
        meta: {
          title: '权限检查',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
      },
      {
        path: 'admin/problems/:id/permissions',
        name: 'admin-problem-permissions',
        component: () => import('../views/admin/AdminProblemPermissionsView.vue'),
        meta: {
          title: '题目权限',
          requiresAuth: true,
          roles: adminRoles,
          permissions: adminPermissions,
        },
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
    return {
      name: 'login',
      query: { redirect: to.fullPath },
    }
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
