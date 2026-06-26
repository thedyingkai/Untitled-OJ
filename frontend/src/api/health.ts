import { apiClient } from './client'
import type { AdminHealthResponse } from '../types/health'

export function getAdminHealth(): Promise<AdminHealthResponse> {
  return apiClient.get('/admin/health')
}
