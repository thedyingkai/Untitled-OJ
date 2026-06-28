import { apiClient } from './client'
import type {
  ListServicesResponse,
  ListServiceSetsResponse,
  ServiceDetailResponse,
  ServiceRuntimeRoutesResponse,
  ServiceRuntimeSnapshotResponse,
  ServiceTopologyResponse,
  RuntimeServicePlanResponse,
  RuntimeServiceResponse,
  RuntimeOperationsResponse,
  RuntimeServicesResponse,
} from '../types/service'

export function listServices(): Promise<ListServicesResponse> {
  return apiClient.get('/admin/services')
}

export function listServiceSets(): Promise<ListServiceSetsResponse> {
  return apiClient.get('/admin/sets')
}

export function getServiceTopology(): Promise<ServiceTopologyResponse> {
  return apiClient.get('/admin/topology')
}

export function getServiceRuntimeSnapshot(options?: {
  includeDisabled?: boolean
}): Promise<ServiceRuntimeSnapshotResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.get(`/admin/runtime/snapshot${query}`)
}

export function getServiceRuntimeRoutes(options?: {
  includeDisabled?: boolean
}): Promise<ServiceRuntimeRoutesResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.get(`/admin/runtime/routes${query}`)
}

export function reloadServiceRuntime(options?: {
  includeDisabled?: boolean
}): Promise<ServiceRuntimeRoutesResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.post(`/admin/runtime/reload${query}`, {})
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

export function reloadRuntimeServices(): Promise<ServiceRuntimeRoutesResponse> {
  return apiClient.post('/admin/runtime/reload', {})
}

export function getRuntimeOperations(): Promise<RuntimeOperationsResponse> {
  return apiClient.get('/admin/runtime/operations')
}

export function getServiceDetail(ServiceId: string): Promise<ServiceDetailResponse> {
  return apiClient.get(`/admin/services/${encodeURIComponent(ServiceId)}`)
}
