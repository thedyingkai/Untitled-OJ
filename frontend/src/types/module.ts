export type ModuleStatus =
  | 'ENABLED'
  | 'DISABLED'
  | 'INSTALLING'
  | 'FAILED_INSTALL'
  | 'UPGRADING'
  | 'FAILED_UPGRADE'

export interface ModuleSetItem {
  set_id: string
  name: string
  description: string
  sort_order: number
}

export interface ModuleNodeItem {
  module_id: string
  set_id: string
  name: string
  version: string
  status: ModuleStatus | string
  kind: string
  description: string
  manifest?: unknown
}

export interface ModuleEdgeItem {
  from_module_id: string
  to_module_id: string
  edge_type: string
  version_constraint: string
  required: boolean
}

export interface ModuleComponentItem {
  module_id: string
  component_id: string
  component_type: string
  status: string
  config: unknown
}

export interface ModulePermissionItem {
  module_id: string
  permission_key: string
  description: string
}

export interface ModuleMenuItem {
  module_id: string
  menu_key: string
  title: string
  route_path: string
  icon: string
  parent_key: string
  sort_order: number
  required_permission: string
  enabled: boolean
}

export interface ModuleFrontendRouteItem {
  module_id: string
  route_path: string
  route_name: string
  component_key: string
  required_permission: string
  enabled: boolean
}

export interface ModuleGatewayRouteItem {
  module_id: string
  prefix: string
  target_service: string
  auth_mode: string
  enabled: boolean
}

export interface ModuleInstallationItem {
  module_id: string
  name: string
  version: string
  status: string
  manifest: unknown
  enabled_at?: string
  disabled_at?: string
}

export interface ListModulesResponse {
  modules: ModuleNodeItem[]
}

export interface ListModuleSetsResponse {
  sets: ModuleSetItem[]
}

export interface ModuleTopologyResponse {
  sets: ModuleSetItem[]
  nodes: ModuleNodeItem[]
  edges: ModuleEdgeItem[]
  components: ModuleComponentItem[]
}

export interface ModuleDetailResponse {
  module: ModuleNodeItem
  dependencies: ModuleEdgeItem[]
  dependents: ModuleEdgeItem[]
  components: ModuleComponentItem[]
  permissions: ModulePermissionItem[]
  menus: ModuleMenuItem[]
  frontend_routes: ModuleFrontendRouteItem[]
  gateway_routes: ModuleGatewayRouteItem[]
  installations: ModuleInstallationItem[]
  health_checks: ModuleComponentItem[]
}
