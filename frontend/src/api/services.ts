import { apiClient } from './client'
import type {
  ListServicesResponse,
  ListServiceSetsResponse,
  ServiceDetailResponse,
  OrchestratorRoutesResponse,
  OrchestratorSnapshotResponse,
  ServiceTopologyResponse,
  ServiceStatusItemResponse,
  ServiceStatusOperationsResponse,
  ServiceStatusListResponse,
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

export function getOrchestratorSnapshot(options?: {
  includeDisabled?: boolean
}): Promise<OrchestratorSnapshotResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.get(`/admin/orchestrator/snapshot${query}`)
}

export function getOrchestratorRoutes(options?: {
  includeDisabled?: boolean
}): Promise<OrchestratorRoutesResponse> {
  const query = options?.includeDisabled ? '?include_disabled=true' : ''
  return apiClient.get(`/admin/orchestrator/routes${query}`)
}

export function getServiceStatusList(): Promise<ServiceStatusListResponse> {
  return apiClient.get('/admin/services/status')
}

export function getServiceStatusItem(serviceId: string): Promise<ServiceStatusItemResponse> {
  return apiClient.get(`/admin/services/status/${encodeURIComponent(serviceId)}`)
}

export function getServiceStatusOperations(): Promise<ServiceStatusOperationsResponse> {
  return apiClient.get('/admin/services/status/operations')
}

export function getServiceDetail(ServiceId: string): Promise<ServiceDetailResponse> {
  return apiClient.get(`/admin/services/${encodeURIComponent(ServiceId)}`)
}
