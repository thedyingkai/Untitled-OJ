import { defineStore } from 'pinia'

import { apiClient, toApiClientError } from '../api/client'
import {
  clearAuthSnapshot,
  getAuthToken,
  getStoredUser,
  saveAuthSnapshot,
} from './authSession'
import type {
  CurrentUser,
  LoginData,
  LoginRequest,
  ProfileData,
  RegisterData,
  RegisterRequest,
} from '../types/auth'

interface AuthState {
  token: string
  user: CurrentUser | null
  loading: boolean
  initialized: boolean
}

export const useAuthStore = defineStore('auth', {
  state: (): AuthState => ({
    token: getAuthToken(),
    user: getStoredUser(),
    loading: false,
    initialized: false,
  }),

  getters: {
    isAuthenticated: (state) => Boolean(state.token && state.user),
    roles: (state) => state.user?.roles ?? [],
    permissions: (state) => state.user?.permissions ?? [],
    hasRole: (state) => (role: string) => Boolean(state.user?.roles.includes(role)),
    hasAnyRole: (state) => (roles: string[]) =>
      roles.length === 0 || roles.some((role) => state.user?.roles.includes(role)),
    hasPermission: (state) => (permission: string) =>
      Boolean(state.user?.permissions.includes(permission)),
    hasAnyPermission: (state) => (permissions: string[]) =>
      permissions.length === 0 ||
      permissions.some((permission) => state.user?.permissions.includes(permission)),
  },

  actions: {
    async login(payload: LoginRequest): Promise<CurrentUser> {
      this.loading = true
      try {
        const data = await apiClient.post<LoginData, LoginRequest>('/auth/login', payload)
        const user = normalizeUser(data)
        this.token = data.token
        this.user = user
        saveAuthSnapshot({ token: data.token, user })
        return user
      } finally {
        this.loading = false
      }
    },

    async register(payload: RegisterRequest): Promise<RegisterData> {
      this.loading = true
      try {
        return await apiClient.post<RegisterData, RegisterRequest>('/auth/register', payload)
      } finally {
        this.loading = false
      }
    },

    async refreshCurrentUser(): Promise<CurrentUser | null> {
      if (!this.token) {
        this.clearAuth()
        return null
      }

      this.loading = true
      try {
        const data = await apiClient.get<ProfileData>('/auth/profile')
        const user = normalizeUser({ ...data, token: this.token })
        this.user = user
        saveAuthSnapshot({ token: this.token, user })
        return user
      } catch (error) {
        const apiError = toApiClientError(error)
        if (apiError.status === 401) {
          this.clearAuth()
          return null
        }
        throw apiError
      } finally {
        this.loading = false
      }
    },

    async restore(): Promise<void> {
      if (this.initialized) {
        return
      }

      this.token = getAuthToken()
      this.user = getStoredUser()

      if (this.token) {
        await this.refreshCurrentUser()
      }

      this.initialized = true
    },

    logout(): void {
      this.clearAuth()
    },

    clearAuth(): void {
      this.token = ''
      this.user = null
      this.initialized = true
      clearAuthSnapshot()
    },
  },
})

function normalizeUser(data: LoginData | (ProfileData & { token?: string })): CurrentUser {
  return {
    user_id: data.user_id,
    username: data.username,
    roles: Array.isArray(data.roles) ? data.roles : [],
    permissions: Array.isArray(data.permissions) ? data.permissions : [],
  }
}
