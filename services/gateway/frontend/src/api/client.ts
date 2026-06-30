import axios, { AxiosError, type AxiosRequestConfig } from 'axios'

import { clearAuthSnapshot, getAuthToken } from '../stores/authSession'
import type { ApiEnvelope, ApiRequestError } from '../types/common'

const defaultBaseUrl = '/api'

export const apiBaseUrl = (import.meta.env.VITE_API_BASE_URL || defaultBaseUrl).replace(/\/+$/, '')

const statusMessages: Record<number, string> = {
  400: '请求参数有误，请检查后重试。',
  401: '登录状态已失效，请重新登录。',
  403: '当前账号没有执行此操作的权限。',
  404: '请求的资源不存在。',
  409: '当前操作与已有数据冲突。',
  429: '请求过于频繁，请稍后再试。',
  500: '服务暂时不可用，请稍后重试。',
}

let unauthorizedHandler: ((error: ApiClientError) => void) | undefined

export class ApiClientError extends Error implements ApiRequestError {
  status: number
  code?: number
  requestId?: string
  details?: unknown
  fieldErrors?: Record<string, string[]>

  constructor(error: ApiRequestError) {
    super(error.message)
    this.name = 'ApiClientError'
    this.status = error.status
    this.code = error.code
    this.requestId = error.requestId
    this.details = error.details
    this.fieldErrors = error.fieldErrors
  }
}

export function setUnauthorizedHandler(handler: (error: ApiClientError) => void): void {
  unauthorizedHandler = handler
}

export function isApiClientError(error: unknown): error is ApiClientError {
  return error instanceof ApiClientError
}

const http = axios.create({
  baseURL: apiBaseUrl,
  timeout: 30000,
  headers: {
    Accept: 'application/json',
    'Content-Type': 'application/json; charset=utf-8',
  },
})

http.interceptors.request.use((config) => {
  const token = getAuthToken()
  if (token) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

http.interceptors.response.use(
  (response) => response,
  (error: AxiosError) => {
    const apiError = normalizeAxiosError(error)
    if (apiError.status === 401) {
      clearAuthSnapshot()
      unauthorizedHandler?.(apiError)
    }
    return Promise.reject(apiError)
  },
)

export const apiClient = {
  async request<T>(config: AxiosRequestConfig): Promise<T> {
    const response = await http.request<ApiEnvelope<T> | T>(config)
    return unwrapResponse<T>(
      response.data,
      extractRequestId(response.headers as Record<string, string | string[] | undefined>),
    )
  },

  get<T>(url: string, config?: AxiosRequestConfig): Promise<T> {
    return this.request<T>({ ...config, method: 'GET', url })
  },

  post<T, B = unknown>(url: string, body?: B, config?: AxiosRequestConfig): Promise<T> {
    return this.request<T>({ ...config, method: 'POST', url, data: body })
  },

  put<T, B = unknown>(url: string, body?: B, config?: AxiosRequestConfig): Promise<T> {
    return this.request<T>({ ...config, method: 'PUT', url, data: body })
  },

  patch<T, B = unknown>(url: string, body?: B, config?: AxiosRequestConfig): Promise<T> {
    return this.request<T>({ ...config, method: 'PATCH', url, data: body })
  },

  delete<T>(url: string, config?: AxiosRequestConfig): Promise<T> {
    return this.request<T>({ ...config, method: 'DELETE', url })
  },
}

export function toApiClientError(error: unknown): ApiClientError {
  if (isApiClientError(error)) {
    return error
  }

  if (error instanceof Error) {
    return new ApiClientError({
      status: 0,
      message: error.message || '请求失败，请稍后重试。',
    })
  }

  return new ApiClientError({
    status: 0,
    message: '请求失败，请稍后重试。',
    details: error,
  })
}

function unwrapResponse<T>(payload: ApiEnvelope<T> | T, requestId?: string): T {
  if (!isEnvelope(payload)) {
    return payload as T
  }

  const code = typeof payload.code === 'number' ? payload.code : 0
  if (code !== 0) {
    const error = new ApiClientError({
      status: statusFromBusinessCode(code),
      code,
      message: payload.msg || statusMessages[statusFromBusinessCode(code)] || '请求处理失败。',
      requestId: payload.request_id || requestId,
      details: payload,
    })

    if (error.status === 401) {
      clearAuthSnapshot()
      unauthorizedHandler?.(error)
    }

    throw error
  }

  return (payload.data ?? payload) as T
}

function normalizeAxiosError(error: AxiosError): ApiClientError {
  const status = error.response?.status ?? 0
  const payload = error.response?.data as Partial<ApiEnvelope> | undefined
  const requestId = extractRequestId(error.response?.headers as Record<string, string | string[] | undefined>)
  const message =
    payload?.msg ||
    (typeof payload === 'object' && payload && 'message' in payload
      ? String((payload as { message?: unknown }).message || '')
      : '') ||
    statusMessages[status] ||
    error.message ||
    '请求失败，请稍后重试。'

  return new ApiClientError({
    status,
    code: typeof payload?.code === 'number' ? payload.code : undefined,
    message,
    requestId: payload?.request_id || requestId,
    details: payload,
  })
}

function statusFromBusinessCode(code: number): number {
  const family = Math.floor(code / 100)
  if (family === 401) return 401
  if (family === 403) return 403
  if (family === 404) return 404
  if (family === 409) return 409
  if (family === 429) return 429
  if (family >= 500) return 500
  return 400
}

function extractRequestId(headers?: Record<string, string | string[] | undefined>): string | undefined {
  if (!headers) {
    return undefined
  }
  const value = headers['x-request-id'] || headers['X-Request-Id'] || headers['x-ojos-request-id']
  if (Array.isArray(value)) {
    return value[0]
  }
  return value
}

function isEnvelope<T>(payload: ApiEnvelope<T> | T): payload is ApiEnvelope<T> {
  return Boolean(
    payload &&
      typeof payload === 'object' &&
      ('code' in payload || 'msg' in payload || 'data' in payload),
  )
}
