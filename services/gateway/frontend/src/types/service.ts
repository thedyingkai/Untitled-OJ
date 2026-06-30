export type ServiceStatus =
  | 'ENABLED'
  | 'DISABLED'
  | 'INSTALLING'
  | 'FAILED_INSTALL'
  | 'UPGRADING'
  | 'FAILED_UPGRADE'
  | 'REMOVED'

export interface EndpointGroupItem {
  service_name: string
  selector: string
  endpoint_count: number
  endpoints: string[]
}

export interface ServiceDefinitionItem {
  service_id: string
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

export interface ServiceEndpointItem {
  endpoint: string
  service_id: string
  protocol: string
  health_path: string
  health: string
  reachable: boolean
  display_name: string
  note: string
  config: unknown
}

export interface ListServicesResponse {
  services: ServiceDefinitionItem[]
}

export interface ListEndpointGroupsResponse {
  endpoint_groups: EndpointGroupItem[]
}

export interface ServiceTopologyResponse {
  endpoint_groups: EndpointGroupItem[]
  nodes: ServiceTopologyNode[]
  edges: ServiceTopologyEdge[]
  components: ServiceComponentItem[]
  service_definitions: ServiceDefinitionItem[]
  dependency_edges: ServiceEdgeItem[]
}

export interface ServiceStatusComponent {
  service_id: string
  component_id: string
  type: string
  status: string
  config: unknown
}

export interface ServiceStatusItem {
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

export interface ServiceTopologyGraph {
  nodes: ServiceTopologyNode[]
  edges: ServiceTopologyEdge[]
  service_definitions: ServiceDefinitionItem[]
  dependency_edges: ServiceEdgeItem[]
}

export interface OrchestratorSnapshotItem {
  service_id: string
  id: string
  type: string
  status: string
  enabled: boolean
  config: unknown
}

export interface ServiceTopologyNode {
  id: string
  service_id: string
  label: string
  type: string
  status: string
  source: string
  config: unknown
}

export interface ServiceTopologyEdge {
  id: string
  service_id: string
  from: string
  to: string
  type: string
  required: boolean
  source: string
}

export interface OrchestratorSnapshotResponse {
  version: string
  generated_at: string
  service_definitions: ServiceDefinitionItem[]
  permissions: ServicePermissionItem[]
  roles: OrchestratorSnapshotItem[]
  menus: ServiceMenuItem[]
  frontend_routes: ServiceFrontendRouteItem[]
  gateway_routes: ServiceGatewayRouteItem[]
  components: ServiceStatusComponent[]
  services: ServiceStatusItem[]
  workers: ServiceStatusItem[]
  storage_buckets: OrchestratorSnapshotItem[]
  health_checks: ServiceStatusComponent[]
  operations: OrchestratorSnapshotItem[]
  topology: ServiceTopologyGraph
  warnings: string[]
}

export interface OrchestratorRouteItem {
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
  service_status?: string
  service_health?: string
  conflicts: string[]
  warnings: string[]
  blocked_by: string[]
}

export interface OrchestratorRoutesResponse {
  version: string
  generated_at: string
  routes: OrchestratorRouteItem[]
  warnings: string[]
  can_proxy: boolean
}

export interface ServiceStatusListResponse {
  services: ServiceStatusItem[]
  workers: ServiceStatusItem[]
}

export interface ServiceStatusItemResponse {
  service: ServiceStatusItem
}

export interface ServiceStatusOperationItem {
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

export interface ServiceStatusOperationsResponse {
  operations: ServiceStatusOperationItem[]
}

export interface ServiceDetailResponse {
  service: ServiceDefinitionItem
  dependencies: ServiceEdgeItem[]
  dependents: ServiceEdgeItem[]
  components: ServiceComponentItem[]
  permissions: ServicePermissionItem[]
  menus: ServiceMenuItem[]
  frontend_routes: ServiceFrontendRouteItem[]
  gateway_routes: ServiceGatewayRouteItem[]
  endpoints: ServiceEndpointItem[]
  health_checks: ServiceComponentItem[]
}
