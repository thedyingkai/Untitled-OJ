export type ServiceStatus =
  | 'ENABLED'
  | 'DISABLED'
  | 'INSTALLING'
  | 'FAILED_INSTALL'
  | 'UPGRADING'
  | 'FAILED_UPGRADE'
  | 'REMOVED'

export interface ServiceSetItem {
  set_id: string
  name: string
  description: string
  sort_order: number
}

export interface ServiceNodeItem {
  service_id: string
  set_id: string
  name: string
  version: string
  status: ServiceStatus | string
  kind: string
  description: string
  manifest?: unknown
}

export interface ServiceEdgeItem {
  from_service_id: string
  to_service_id: string
  edge_type: string
  version_constraint: string
  required: boolean
}

export interface ServiceComponentItem {
  service_id: string
  component_id: string
  component_type: string
  status: string
  config: unknown
}

export interface ServicePermissionItem {
  service_id: string
  permission_key: string
  description: string
}

export interface ServiceMenuItem {
  service_id: string
  menu_key: string
  title: string
  route_path: string
  icon: string
  parent_key: string
  sort_order: number
  required_permission: string
  enabled: boolean
}

export interface ServiceFrontendRouteItem {
  service_id: string
  route_path: string
  route_name: string
  component_key: string
  required_permission: string
  enabled: boolean
}

export interface ServiceGatewayRouteItem {
  service_id: string
  prefix: string
  target_service: string
  auth_mode: string
  enabled: boolean
}

export interface ServiceInstallationItem {
  service_id: string
  name: string
  version: string
  status: string
  manifest: unknown
  enabled_at?: string
  disabled_at?: string
}

export interface ListServicesResponse {
  services: ServiceNodeItem[]
}

export interface ListServiceSetsResponse {
  sets: ServiceSetItem[]
}

export interface ServiceTopologyResponse {
  sets: ServiceSetItem[]
  nodes: ServiceRuntimeTopologyNode[]
  edges: ServiceRuntimeTopologyEdge[]
  components: ServiceComponentItem[]
  service_nodes: ServiceNodeItem[]
  dependency_edges: ServiceEdgeItem[]
}

export interface ServiceRuntimeComponent {
  service_id: string
  component_id: string
  type: string
  status: string
  config: unknown
}

export interface ServiceRuntimeService {
  owner_service_id: string
  service_id: string
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

export interface ServiceRuntimeTopology {
  nodes: ServiceRuntimeTopologyNode[]
  edges: ServiceRuntimeTopologyEdge[]
  service_nodes: ServiceNodeItem[]
  dependency_edges: ServiceEdgeItem[]
}

export interface ServiceRuntimeManifestItem {
  service_id: string
  id: string
  type: string
  status: string
  enabled: boolean
  config: unknown
}

export interface ServiceRuntimeTopologyNode {
  id: string
  service_id: string
  label: string
  type: string
  status: string
  source: string
  config: unknown
}

export interface ServiceRuntimeTopologyEdge {
  id: string
  service_id: string
  from: string
  to: string
  type: string
  required: boolean
  source: string
}

export interface ServiceRuntimeSnapshotResponse {
  version: string
  generated_at: string
  service_nodes: ServiceNodeItem[]
  permissions: ServicePermissionItem[]
  roles: ServiceRuntimeManifestItem[]
  menus: ServiceMenuItem[]
  frontend_routes: ServiceFrontendRouteItem[]
  gateway_routes: ServiceGatewayRouteItem[]
  components: ServiceRuntimeComponent[]
  services: ServiceRuntimeService[]
  workers: ServiceRuntimeService[]
  storage_buckets: ServiceRuntimeManifestItem[]
  health_checks: ServiceRuntimeComponent[]
  operations: ServiceRuntimeManifestItem[]
  topology: ServiceRuntimeTopology
  warnings: string[]
}

export interface ServiceRuntimeRouteItem {
  route_id: string
  owner_service_id: string
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

export interface ServiceRuntimeRoutesResponse {
  version: string
  generated_at: string
  routes: ServiceRuntimeRouteItem[]
  warnings: string[]
  can_proxy: boolean
  reloaded?: boolean
}

export interface RuntimeServicesResponse {
  services: ServiceRuntimeService[]
  workers: ServiceRuntimeService[]
}

export interface RuntimeServiceResponse {
  service: ServiceRuntimeService
}

export interface RuntimePlanCommand {
  kind: string
  argv: string[]
}

export interface RuntimePlanItem {
  plan_id: string
  operation_id: string
  action: string
  service_id: string
  driver: string
  can_apply: boolean
  apply_enabled: boolean
  requires_confirmation: boolean
  dry_run: boolean
  allowed_targets: string[]
  commands: RuntimePlanCommand[]
  affected: string[]
  blocked_by: string[]
  warnings: string[]
  created_at: string
  expires_at: string
}

export interface RuntimeServicePlanResponse {
  plan: RuntimePlanItem
}

export interface RuntimeOperationItem {
  operation_id: string
  service_id: string
  action: string
  status: string
  actor_username: string
  request: unknown
  plan: unknown
  result: unknown
  error_message: string
  created_at: string
  updated_at: string
}

export interface RuntimeOperationsResponse {
  operations: RuntimeOperationItem[]
}

export interface ServiceDetailResponse {
  service: ServiceNodeItem
  dependencies: ServiceEdgeItem[]
  dependents: ServiceEdgeItem[]
  components: ServiceComponentItem[]
  permissions: ServicePermissionItem[]
  menus: ServiceMenuItem[]
  frontend_routes: ServiceFrontendRouteItem[]
  gateway_routes: ServiceGatewayRouteItem[]
  installations: ServiceInstallationItem[]
  health_checks: ServiceComponentItem[]
}
