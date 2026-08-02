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
}

export interface EndpointRow {
  endpoint: string;
  service_id: string;
  protocol: string;
  expose: string;
  source: string;
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
  health: string;
  reachable: boolean;
  display_name: string;
  note: string;
  [key: string]: unknown;
}

export interface TopologyLink {
  source_endpoint: string;
  target_endpoint: string;
  [key: string]: unknown;
}

export interface TopologyData {
  root_host: string;
  root_endpoint: string;
  services: string[];
  endpoints: TopologyEndpoint[];
  links: TopologyLink[];
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
        version: string;
        host_ip: string;
        status: string;
      }>;
    }
  >;
}

export interface StoreStatus {
  index_url: string;
  package_load_enabled: boolean;
  github_token_configured: boolean;
  require_release_checksum: boolean;
  allow_private_release_source: boolean;
  store: string;
}

export interface GithubAsset {
  name: string;
  size: number;
  browser_download_url: string;
  content_type: string;
}

export interface GithubRelease {
  tag_name: string;
  name: string;
  prerelease: boolean;
  published_at: string;
  html_url: string;
  assets: GithubAsset[];
}

export interface HealthInfo {
  status: string;
  service: string;
  store: string;
  warnings: string[];
}

export interface LayoutState {
  positions?: Record<string, { x: number; y: number }>;
  [key: string]: unknown;
}
