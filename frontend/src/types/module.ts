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
  nodes: ModuleRuntimeTopologyNode[]
  edges: ModuleRuntimeTopologyEdge[]
  components: ModuleComponentItem[]
  module_nodes: ModuleNodeItem[]
  dependency_edges: ModuleEdgeItem[]
}

export interface ModuleRuntimeComponent {
  module_id: string
  component_id: string
  type: string
  status: string
  config: unknown
}

export interface ModuleRuntimeService {
  service_id: string
  module_id: string
  name: string
  kind: string
  lifecycle: string
  runtime: string
  compose_service?: string
  state: string
  health: string
  required: boolean
  routes: string[]
  health_check_id?: string
  status: string
  blocked_by: string[]
  warnings: string[]
}

export interface ModuleRuntimeTopology {
  nodes: ModuleRuntimeTopologyNode[]
  edges: ModuleRuntimeTopologyEdge[]
  module_nodes: ModuleNodeItem[]
  dependency_edges: ModuleEdgeItem[]
}

export interface ModuleRuntimeManifestItem {
  module_id: string
  id: string
  type: string
  status: string
  enabled: boolean
  config: unknown
}

export interface ModuleRuntimeTopologyNode {
  id: string
  module_id: string
  label: string
  type: string
  status: string
  source: string
  config: unknown
}

export interface ModuleRuntimeTopologyEdge {
  id: string
  module_id: string
  from: string
  to: string
  type: string
  required: boolean
  source: string
}

export interface ModuleRuntimeSnapshotResponse {
  version: string
  generated_at: string
  modules: ModuleNodeItem[]
  permissions: ModulePermissionItem[]
  roles: ModuleRuntimeManifestItem[]
  menus: ModuleMenuItem[]
  frontend_routes: ModuleFrontendRouteItem[]
  gateway_routes: ModuleGatewayRouteItem[]
  components: ModuleRuntimeComponent[]
  services: ModuleRuntimeService[]
  workers: ModuleRuntimeService[]
  storage_buckets: ModuleRuntimeManifestItem[]
  health_checks: ModuleRuntimeComponent[]
  operations: ModuleRuntimeManifestItem[]
  topology: ModuleRuntimeTopology
  warnings: string[]
}

export interface ModuleRuntimeRouteItem {
  route_id: string
  module_id: string
  prefix: string
  service_id: string
  target_service: string
  upstream_base?: string
  auth_mode: string
  methods: string[]
  enabled: boolean
  proxy_enabled: boolean
  priority: number
  strip_prefix?: string
  rewrite_prefix?: string
  health_check_id?: string
  created_from: string
  status: string
  service_state?: string
  service_health?: string
  conflicts: string[]
  warnings: string[]
  blocked_by: string[]
}

export interface ModuleRuntimeRoutesResponse {
  version: string
  generated_at: string
  routes: ModuleRuntimeRouteItem[]
  warnings: string[]
  can_proxy: boolean
  reloaded?: boolean
}

export interface RuntimeServicesResponse {
  services: ModuleRuntimeService[]
  workers: ModuleRuntimeService[]
}

export interface RuntimeServiceResponse {
  service: ModuleRuntimeService
}

export interface RuntimePlanCommand {
  tool: string
  args: string[]
}

export interface RuntimePlanItem {
  plan_id: string
  action: string
  service_id: string
  module_id: string
  driver: string
  can_apply: boolean
  apply_enabled: boolean
  commands: RuntimePlanCommand[]
  affected: string[]
  blocked_by: string[]
  warnings: string[]
  created_at: string
}

export interface RuntimeServicePlanResponse {
  plan: RuntimePlanItem
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

export interface ModulePlanAction {
  action: string
  target: string
  detail?: string
}

export interface ModulePlanWarning {
  code: string
  message: string
}

export interface ModulePlan {
  kind: string
  module_id: string
  version: string
  dry_run: boolean
  can_apply: boolean
  actions: ModulePlanAction[]
  affected_tables: string[]
  affected_modules: string[]
  dependencies: string[]
  blocked_by: string[]
  warnings: ModulePlanWarning[]
}

export interface ModuleInstallerEnvelope<T = unknown> {
  code: number
  msg: string
  data: T
}

export interface ModuleInstallerRequest {
  manifest_path?: string
  manifest?: unknown
  dry_run?: boolean
}

export interface ModuleDiscoverItem {
  manifest_path: string
  module_id?: string
  name?: string
  version?: string
  status?: string
  valid?: boolean
  error?: string
}

export interface ModuleDiscoverData {
  modules: ModuleDiscoverItem[]
}

export interface ModuleValidateData {
  valid: boolean
  manifest: unknown
}

export interface ModuleInstallData {
  plan: ModulePlan
  result?: unknown
}

export interface ModuleHealthData {
  module_id: string
  status: string
  module_status: string
}

export interface ModuleOperationItem {
  operation_id: string
  module_id: string
  action: string
  status: string
  actor_user_id?: number
  actor_username: string
  request: unknown
  plan: unknown
  result: unknown
  error_message: string
  created_at: string
  updated_at: string
}

export interface ModuleOperationsData {
  operations: ModuleOperationItem[]
}
