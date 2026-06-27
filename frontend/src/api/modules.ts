import { apiClient } from './client'
import type {
  ListModulesResponse,
  ListModuleSetsResponse,
  ModuleDiscoverData,
  ModuleDetailResponse,
  ModuleHealthData,
  ModuleInstallData,
  ModuleInstallerRequest,
  ModuleOperationsData,
  ModulePlan,
  ModuleRuntimeRoutesResponse,
  ModuleRuntimeSnapshotResponse,
  ModuleTopologyResponse,
  ModuleValidateData,
  RuntimeServicePlanResponse,
  RuntimeServiceResponse,
  RuntimeOperationsResponse,
  RuntimeServicesResponse,
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

export function getModuleRuntimeSnapshot(options?: {
  includeDisabled?: boolean
}): Promise<ModuleRuntimeSnapshotResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.get(`/admin/modules/runtime-snapshot${query}`)
}

export function getModuleRuntimeRoutes(options?: {
  includeDisabled?: boolean
}): Promise<ModuleRuntimeRoutesResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.get(`/admin/modules/runtime/routes${query}`)
}

export function reloadModuleRuntime(options?: {
  includeDisabled?: boolean
}): Promise<ModuleRuntimeRoutesResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.post(`/admin/modules/runtime/reload${query}`, {})
}

export function getRuntimeServices(): Promise<RuntimeServicesResponse> {
  return apiClient.get('/admin/runtime/services')
}

export function getRuntimeService(serviceId: string): Promise<RuntimeServiceResponse> {
  return apiClient.get(`/admin/runtime/services/${encodeURIComponent(serviceId)}`)
}

export function planRuntimeServiceStart(serviceId: string): Promise<RuntimeServicePlanResponse> {
  return apiClient.post(`/admin/runtime/services/${encodeURIComponent(serviceId)}/plan-start`, {})
}

export function planRuntimeServiceStop(serviceId: string): Promise<RuntimeServicePlanResponse> {
  return apiClient.post(`/admin/runtime/services/${encodeURIComponent(serviceId)}/plan-stop`, {})
}

export function planRuntimeServiceRestart(serviceId: string): Promise<RuntimeServicePlanResponse> {
  return apiClient.post(`/admin/runtime/services/${encodeURIComponent(serviceId)}/plan-restart`, {})
}

export function planRuntimeServiceReload(serviceId: string): Promise<RuntimeServicePlanResponse> {
  return apiClient.post(`/admin/runtime/services/${encodeURIComponent(serviceId)}/plan-reload`, {})
}

export function reloadRuntimeServices(): Promise<ModuleRuntimeRoutesResponse> {
  return apiClient.post('/admin/runtime/reload', {})
}

export function getRuntimeOperations(): Promise<RuntimeOperationsResponse> {
  return apiClient.get('/admin/runtime/operations')
}

export function getModuleDetail(moduleId: string): Promise<ModuleDetailResponse> {
  return apiClient.get(`/admin/modules/${encodeURIComponent(moduleId)}`)
}

function installerData<T>(promise: Promise<{ data: T } | T>): Promise<T> {
  return promise.then((value) => {
    if (value && typeof value === 'object' && 'data' in value) {
      return (value as { data: T }).data
    }
    return value as T
  })
}

export function discoverModules(): Promise<ModuleDiscoverData> {
  return installerData(apiClient.get('/admin/modules/discover'))
}

export function validateModule(req: ModuleInstallerRequest): Promise<ModuleValidateData> {
  return installerData(apiClient.post('/admin/modules/validate', req))
}

export function planModule(req: ModuleInstallerRequest): Promise<ModulePlan> {
  return installerData(apiClient.post('/admin/modules/plan', req))
}

export function installModule(req: ModuleInstallerRequest): Promise<ModuleInstallData> {
  return installerData(apiClient.post('/admin/modules/install', req))
}

export function enableModule(moduleId: string): Promise<ModuleInstallData> {
  return installerData(apiClient.post(`/admin/modules/${encodeURIComponent(moduleId)}/enable`, {}))
}

export function disableModule(moduleId: string): Promise<ModuleInstallData> {
  return installerData(apiClient.post(`/admin/modules/${encodeURIComponent(moduleId)}/disable`, {}))
}

export function upgradePlanModule(moduleId: string, req: ModuleInstallerRequest): Promise<ModulePlan> {
  return installerData(apiClient.post(`/admin/modules/${encodeURIComponent(moduleId)}/upgrade-plan`, req))
}

export function rollbackPlanModule(moduleId: string): Promise<ModulePlan> {
  return installerData(apiClient.post(`/admin/modules/${encodeURIComponent(moduleId)}/rollback-plan`, {}))
}

export function uninstallDryRunModule(moduleId: string): Promise<ModulePlan> {
  return installerData(apiClient.post(`/admin/modules/${encodeURIComponent(moduleId)}/uninstall-dry-run`, {}))
}

export function getModuleInstallerHealth(moduleId: string): Promise<ModuleHealthData> {
  return installerData(apiClient.get(`/admin/modules/${encodeURIComponent(moduleId)}/health`))
}

export function listModuleOperations(moduleId: string): Promise<ModuleOperationsData> {
  return installerData(apiClient.get(`/admin/modules/${encodeURIComponent(moduleId)}/operations`))
}
