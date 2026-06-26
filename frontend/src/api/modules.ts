import { apiClient } from './client'
import type {
  ListModulesResponse,
  ListModuleSetsResponse,
  ModuleDetailResponse,
  ModuleTopologyResponse,
} from '../types/module'

export function listModules(): Promise<ListModulesResponse> {
  return apiClient.get('/admin/modules')
}

export function listModuleSets(): Promise<ListModuleSetsResponse> {
  return apiClient.get('/admin/modules/sets')
}

export function getModuleTopology(): Promise<ModuleTopologyResponse> {
  return apiClient.get('/admin/modules/topology')
}

export function getModuleDetail(moduleId: string): Promise<ModuleDetailResponse> {
  return apiClient.get(`/admin/modules/${encodeURIComponent(moduleId)}`)
}
