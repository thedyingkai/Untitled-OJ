import { apiClient } from './client'
import type {
  AuditLogItem,
  PermissionCheckRequest,
  PermissionCheckResult,
  PermissionItem,
  ProblemRoleRequest,
  RoleItem,
  UserAdminItem,
  UserRoleRequest,
} from '../types/permission'

export function listAdminUsers(): Promise<UserAdminItem[]> {
  return apiClient.get('/auth/admin/users')
}

export function listAdminRoles(): Promise<RoleItem[]> {
  return apiClient.get('/auth/admin/roles')
}

export function listAdminPermissions(): Promise<PermissionItem[]> {
  return apiClient.get('/auth/admin/permissions')
}

export function addUserRole(payload: UserRoleRequest): Promise<{ ok: boolean }> {
  return apiClient.post('/auth/admin/users/roles', payload)
}

export function removeUserRole(payload: UserRoleRequest): Promise<{ ok: boolean }> {
  return apiClient.delete('/auth/admin/users/roles', { data: payload })
}

export function addProblemRole(payload: ProblemRoleRequest): Promise<{ ok: boolean }> {
  return apiClient.post('/auth/admin/problems/roles', payload)
}

export function removeProblemRole(payload: ProblemRoleRequest): Promise<{ ok: boolean }> {
  return apiClient.delete('/auth/admin/problems/roles', { data: payload })
}

export function checkPermission(
  payload: PermissionCheckRequest,
): Promise<PermissionCheckResult> {
  return apiClient.post('/auth/admin/permission-check', payload)
}

export function listAuditLogs(): Promise<AuditLogItem[]> {
  return apiClient.get('/auth/admin/audit-logs')
}
