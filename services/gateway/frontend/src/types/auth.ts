export type RoleName = string
export type PermissionCode = string

export interface LoginRequest {
  username: string
  password: string
}

export interface RegisterRequest {
  username: string
  email?: string
  password: string
}

export interface LoginData {
  token: string
  user_id: number
  username: string
  roles: RoleName[]
  permissions?: PermissionCode[]
}

export interface RegisterData {
  user_id: number
  username: string
}

export interface ProfileData {
  user_id: number
  username: string
  roles: RoleName[]
  permissions?: PermissionCode[]
}

export interface CurrentUser {
  user_id: number
  username: string
  roles: RoleName[]
  permissions: PermissionCode[]
}

export interface AuthSnapshot {
  token: string
  user: CurrentUser | null
}
