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
    meta: { title: 'Login', guestOnly: true },
  },
  {
    path: '/register',
    name: 'register',
    component: () => import('../views/auth/RegisterView.vue'),
    meta: { title: 'Register', guestOnly: true },
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
        meta: { title: 'Dashboard', requiresAuth: true },
      },
      {
        path: 'me',
        name: 'me',
        component: () => import('../views/profile/MeView.vue'),
        meta: { title: 'Profile', requiresAuth: true },
      },
      {
        path: 'problems',
        name: 'problems',
        component: () => import('../views/problem/ProblemListView.vue'),
        meta: { title: 'Problems', requiresAuth: true },
      },
      {
        path: 'problems/new',
        name: 'problem-create',
        component: () => import('../views/problem/ProblemCreateView.vue'),
        meta: {
          title: 'New Problem',
          requiresAuth: true,
          roles: adminRoles,
          permissions: ['problem.create', 'system.admin'],
        },
      },
      {
        path: 'problems/:id/edit',
        name: 'problem-edit',
        component: () => import('../views/problem/ProblemEditView.vue'),
        meta: { title: 'Edit Problem', requiresAuth: true },
      },
      {
        path: 'problems/:id/package',
        name: 'problem-package',
        component: () => import('../views/problem/ProblemPackageView.vue'),
        meta: { title: 'Problem Package', requiresAuth: true },
      },
      {
        path: 'problems/:id/submit',
        name: 'problem-submit',
        component: () => import('../views/judge/ProblemSubmitView.vue'),
        meta: { title: 'Submit', requiresAuth: true },
      },
      {
        path: 'problems/:id',
        name: 'problem-detail',
        component: () => import('../views/problem/ProblemDetailView.vue'),
        meta: { title: 'Problem Detail', requiresAuth: true },
      },
      {
        path: 'submissions',
        name: 'submissions',
        component: () => import('../views/judge/SubmissionsListView.vue'),
        meta: { title: 'Submissions', requiresAuth: true },
      },
      {
        path: 'submissions/:id',
        name: 'submission-detail',
        component: () => import('../views/judge/SubmissionDetailView.vue'),
        meta: { title: 'Submission Detail', requiresAuth: true },
      },
      {
        path: 'admin/health',
        name: 'admin-health',
        component: () => import('../views/admin/AdminHealthView.vue'),
        meta: {
          title: 'Service Health',
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
          title: 'Judge Cluster',
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
          title: 'Modules',
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
          title: 'Module Topology',
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
          title: 'Module Installer',
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
          title: 'Module Contributions',
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
          title: 'Module Detail',
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
          title: 'Users',
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
          title: 'Permissions',
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
          title: 'Permission Check',
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
          title: 'Problem Permissions',
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
    meta: { title: 'Forbidden' },
  },
  {
    path: '/500',
    name: 'server-error',
    component: () => import('../views/errors/ServerErrorView.vue'),
    meta: { title: 'Server Error' },
  },
  {
    path: '/:pathMatch(.*)*',
    name: 'not-found',
    component: () => import('../views/errors/NotFoundView.vue'),
    meta: { title: 'Not Found' },
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
