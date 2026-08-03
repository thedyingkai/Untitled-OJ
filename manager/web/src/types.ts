export interface ServiceRow {
  id: string;
  name: string;
  version: string;
  kind: string;
  endpoint: string;
  runtime: string;
  ui: string;
  health: string;
}

/** 一行对应一个 host + service 部署，不与 Service manifest 注册表混用。 */
export interface DeploymentRow {
  deployment_id: string;
  node_id: string;
  service_id: string;
  name: string;
  version: string;
  kind: string;
  runtime: string;
  host_ip: string;
  status: string;
  endpoint: string;
  protocol: string;
  health_path: string;
  endpoint_health: string;
  reachable: boolean;
  endpoint_count: number;
  endpoints: string[];
  container_id: string;
  artifact_digest: string;
  desired_state: string;
  observed_state: string;
  updated_at: string;
}

export interface EndpointRow {
  endpoint: string;
  service_id: string;
  protocol: string;
  expose: string;
  source: string;
  health_path: string;
  health: string;
  reachable: boolean;
  display_name: string;
  note: string;
}

export interface LinkRow {
  from: string;
  to: string;
  protocol: string;
  auth_mode: string;
  scope: string;
  /** "enabled" / "disabled"，对应 core 的 Link.enabled 启停开关 */
  enabled: string;
  source: string;
  health: string;
}

export interface NodeRow {
  node_id: string;
  host_ip: string;
  parent_node_id: string;
  role: string;
  labels: Record<string, unknown>;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface OperationRow {
  operation_id: string;
  action: string;
  target: string;
  status: string;
  risk: string;
  plan_required: string;
  mode: string;
  requires_confirmation: boolean;
  driver_authorized: boolean;
  rollback_available: boolean;
  fields: string;
  preview_target: string;
  preview_steps: string;
  preview_confirmation: string;
  result: string;
  error: string;
  log_count: number;
  summary: string;
  created_at: string;
  updated_at: string;
}

export interface OperationLog {
  operation_id: string;
  step_id: string;
  level: string;
  message: string;
  created_at: string;
  [key: string]: unknown;
}

export interface TopologyEndpoint {
  endpoint: string;
  service_id: string;
  protocol: string;
  health_path: string;
  display_name: string;
  note: string;
  config: Record<string, unknown>;
}

export interface TopologyLink {
  source_endpoint: string;
  target_endpoint: string;
  protocol: string;
  auth_mode: string;
  scope: string;
  enabled: boolean;
  config_ref: string;
  secret_ref: string;
  policy: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TopologySpec {
  api_version: "v1";
  topology_id: string;
  root_endpoint: string;
  authority: {
    root_endpoint: string;
    exposure_policy: string;
  };
  endpoints: TopologyEndpoint[];
  links: TopologyLink[];
}

export interface TopologyRevision {
  topology_id: string;
  revision_number: number;
  revision_id: string;
  parent_revision_id: string | null;
  rollback_of_revision_id: string | null;
  content_sha256: string;
  spec: TopologySpec;
  created_at: string;
  created_by: string;
  message: string;
}

export interface TopologyHeads {
  topology_id: string;
  draft_revision_id: string;
  applied_revision_id: string | null;
  applying_revision_id: string | null;
  applying_operation_id: string | null;
  last_operation_id: string | null;
}

export interface TopologyEndpointStatus {
  endpoint: string;
  health: string;
  reachable: boolean;
  latency_ms?: number | null;
  message: string;
  observed_at: string;
}

export interface TopologyLinkStatus {
  source_endpoint: string;
  target_endpoint: string;
  health: string;
  latency_ms?: number | null;
  message: string;
  observed_at: string;
}

export interface TopologyStatus {
  topology_id: string;
  desired_revision_id: string | null;
  observed_revision_id: string | null;
  state: string;
  deployments: Array<Record<string, unknown>>;
  endpoints: TopologyEndpointStatus[];
  links: TopologyLinkStatus[];
  drift: Array<{
    resource_kind: string;
    resource_id: string;
    kind: string;
    detail: string;
  }>;
  last_operation_id: string | null;
  updated_at: string;
}

export interface TopologyDetail {
  heads: TopologyHeads;
  draft: TopologyRevision;
  status: TopologyStatus | null;
}

export interface TopologyDiff {
  topology_id: string;
  from_revision_id: string | null;
  to_revision_id: string | null;
  from_sha256: string | null;
  to_sha256: string;
  changes: Array<Record<string, unknown>>;
  [key: string]: unknown;
}

export interface StoreModule {
  id: string;
  name: string;
  description: string;
  kind: string;
  tags: string[];
  repo: string;
  source_url: string;
  checksum: string;
  version: string;
  channel: string;
  platforms: Array<{ os: string; arch: string }>;
  min_orchestrator_version: string;
  oci_image: string;
  source_id: string;
  catalog_id: string;
}

export interface StoreIndexResponse {
  index_url: string;
  cached: boolean;
  index: {
    schema_version: number;
    name?: string;
    description?: string;
    updated_at?: string;
    modules: StoreModule[];
  };
  installed: Record<
    string,
    {
      version: string;
      versions: string[];
      kind: string;
      deployments: Array<{
        deployment_id: string;
        node_id: string;
        version: string;
        host_ip: string;
        status: string;
      }>;
    }
  >;
}

export interface AsyncOperationResult {
  operation_id: string;
  deployment_id?: string;
  revision_id?: string;
  topology_id?: string;
  [key: string]: unknown;
}

export interface StoreValidationResult {
  valid: boolean;
  catalog_source_id: string;
  catalog_id: string;
  verified_key_ids: string[];
  target_platform: { os: string; arch: string };
  plan: unknown;
  metadata: Array<Record<string, unknown>>;
  side_effects: {
    release_imports: number;
    operations: number;
    jobs: number;
    runtime_calls: number;
  };
}

export interface HealthInfo {
  status: string;
  service: string;
  store: string;
  warnings: string[];
}

export interface CapabilityRow {
  action: string;
  target_type: string;
  capability_status: string;
  required_permission: string;
}

export type LoadStatus = "idle" | "loading" | "ready" | "error";

export interface LayoutState {
  positions?: Record<string, { x: number; y: number }>;
  [key: string]: unknown;
}
