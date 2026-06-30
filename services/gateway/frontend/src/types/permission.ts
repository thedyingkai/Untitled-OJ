import type { PermissionCode, RoleName } from './auth'

export interface RoleItem {
  id: number
  name: RoleName
  service_code?: string
  description?: string
  is_system?: boolean
}

export interface PermissionItem {
  code: PermissionCode
  service_code: string
  name: string
  description?: string
}

export interface PermissionCheckRequest {
  user_id: number
  permission: PermissionCode
  scope_type: string
  scope_id: number
}

export interface PermissionCheckResult {
  allowed: boolean
}

export interface UserAdminItem {
  user_id: number
  username: string
  email?: string
  roles: RoleName[]
  created_at: string
}

export interface UserRoleRequest {
  user_id: number
  role: string
}

export interface ProblemRoleRequest {
  user_id: number
  problem_id: number
  role: string
}

export interface AuditLogItem {
  id: number
  actor_type: string
  actor_id: number
  action: string
  target_type: string
  target_id: number
  permission_code: string
  role_name: string
  scope_type: string
  scope_id: number
  effect: string
  created_at: string
}

export interface RoleBinding {
  principal_type: string
  principal_id: number
  role_name: RoleName
  scope_type: string
  scope_id: number
  expires_at?: string
}
