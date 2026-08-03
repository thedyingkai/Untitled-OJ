//! Web backend 与 TUI 共用的管理面应用服务。
//!
//! 这里集中 Store 索引、GitHub Release、安装策略和“已部署”投影；HTTP 与终端层只
//! 负责输入输出，不再各自复制一套业务规则。

pub mod catalog_v2;
pub mod release_v2;

pub use release_v2::{InstallModeV2, MigrationPolicyV2, ReleaseStateV2, RuntimeDesiredStateV2};

use anyhow::{Context, Result, anyhow};
use orchestrator_legacy::{
    ActionDispatchResult, ActionRequest, DeploymentViewRow, ExternalReleaseImport,
    OrchestratorActionConsole,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;

const INDEX_CACHE_TTL: Duration = Duration::from_secs(60);
const MAX_FETCH_BYTES: usize = 1024 * 1024;
const MAX_REDIRECTS: u32 = 5;
const USER_AGENT: &str = "ojos-orchestrator-manager";
pub const DEFAULT_STORE_INDEX_PATH: &str = "store/index.json";

#[derive(Debug, Error)]
#[error("{message}")]
pub struct StoreRequestError {
    status: u16,
    message: String,
}

impl StoreRequestError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn status(&self) -> u16 {
        self.status
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreStatusView {
    pub index_url: String,
    pub package_load_enabled: bool,
    pub github_token_configured: bool,
    pub require_release_checksum: bool,
    pub allow_private_release_source: bool,
    pub store: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreDeploymentView {
    pub version: String,
    pub host_ip: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledServiceView {
    pub version: String,
    pub versions: Vec<String>,
    pub kind: String,
    pub deployments: Vec<StoreDeploymentView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoreIndexView {
    pub index_url: String,
    pub cached: bool,
    pub index: Value,
    pub installed: BTreeMap<String, InstalledServiceView>,
}

impl StoreIndexView {
    /// 将索引里的模块数组转换成 Web/TUI 共用的强类型条目。
    pub fn modules(&self) -> Result<Vec<StoreModuleView>> {
        modules_from_index(&self.index)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreModuleView {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub checksum: String,
}

impl StoreModuleView {
    pub fn source(&self) -> &str {
        if self.source_url.trim().is_empty() {
            self.repo.as_str()
        } else {
            self.source_url.as_str()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubAssetView {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    pub content_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubReleaseView {
    pub tag_name: String,
    pub name: String,
    pub prerelease: bool,
    pub published_at: String,
    pub html_url: String,
    pub assets: Vec<GithubAssetView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubReleaseListView {
    pub repo: String,
    pub releases: Vec<GithubReleaseView>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreImportRequest {
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreImportResult {
    pub imported: ExternalReleaseImport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreInstallRequest {
    #[serde(default)]
    pub service_id: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub checksum: String,
    #[serde(default)]
    pub version: String,
    /// v2 target selector. Managed installs resolve this identifier to one registered Node.
    #[serde(default)]
    pub target_node_id: String,
    #[serde(default)]
    pub mode: InstallModeV2,
    #[serde(default = "default_true")]
    pub start: bool,
    #[serde(default)]
    pub migration_policy: MigrationPolicyV2,
    /// Deprecated v1 selector. It is accepted only when it resolves to exactly one Node.
    #[serde(default)]
    pub host_ip: String,
    #[serde(default)]
    pub endpoint: String,
    /// Deprecated v1 execution toggle. False no longer permits a planned-only install.
    #[serde(default)]
    pub execute_service_driver: bool,
    /// Deprecated v1 mode toggle. True maps to `mode=External`.
    #[serde(default)]
    pub external_service_running: bool,
    /// Deprecated v1 migration toggle. True maps to `migration_policy=DryRun`.
    #[serde(default)]
    pub migration_dry_run: bool,
    #[serde(default)]
    pub gateway_node_id: String,
}

impl Default for StoreInstallRequest {
    fn default() -> Self {
        Self {
            service_id: String::new(),
            source_url: String::new(),
            checksum: String::new(),
            version: String::new(),
            target_node_id: String::new(),
            mode: InstallModeV2::Managed,
            start: true,
            migration_policy: MigrationPolicyV2::Apply,
            host_ip: String::new(),
            endpoint: String::new(),
            execute_service_driver: false,
            external_service_running: false,
            migration_dry_run: false,
            gateway_node_id: String::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreInstallRuntimeView {
    pub mode: InstallModeV2,
    pub target_node_id: String,
    pub host_ip: String,
    pub endpoint: String,
    pub desired_state: RuntimeDesiredStateV2,
    pub observed_state: ReleaseStateV2,
    pub deployment_status: String,
    pub operation_id: String,
    pub operation_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreInstallResult {
    pub service_id: String,
    pub imported: Option<ExternalReleaseImport>,
    pub action_result: ActionDispatchResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_action_result: Option<ActionDispatchResult>,
    pub lifecycle: ReleaseStateV2,
    pub installed: bool,
    pub runtime: StoreInstallRuntimeView,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub struct StoreCatalog {
    index_cache: Mutex<Option<(Instant, String, Value)>>,
}

impl Default for StoreCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreCatalog {
    pub fn new() -> Self {
        Self {
            index_cache: Mutex::new(None),
        }
    }

    pub fn status(&self, console: &OrchestratorActionConsole) -> StoreStatusView {
        StoreStatusView {
            index_url: configured_index_url(),
            package_load_enabled: package_load_enabled(),
            github_token_configured: github_token().is_some(),
            require_release_checksum: require_release_checksum(),
            allow_private_release_source: allow_private_release_source(),
            store: if console.uses_persistent_store() {
                "persistent".to_string()
            } else {
                "memory".to_string()
            },
        }
    }

    pub fn load_index(&self, repo_root: &Path, refresh: bool) -> Result<(String, bool, Value)> {
        let index_url = configured_index_url();
        let mut cache = self
            .index_cache
            .lock()
            .map_err(|_| anyhow!("store index cache lock poisoned"))?;
        let cached = if refresh {
            None
        } else {
            cache.as_ref().and_then(|(at, url, value)| {
                (url == &index_url && at.elapsed() < INDEX_CACHE_TTL).then(|| value.clone())
            })
        };
        let (index, from_cache) = match cached {
            Some(value) => (value, true),
            None => {
                let value = load_index_document(repo_root, &index_url)?;
                *cache = Some((Instant::now(), index_url.clone(), value.clone()));
                (value, false)
            }
        };
        Ok((index_url, from_cache, index))
    }

    pub fn index(
        &self,
        console: &OrchestratorActionConsole,
        repo_root: &Path,
        refresh: bool,
    ) -> Result<StoreIndexView> {
        let (index_url, cached, index) = self.load_index(repo_root, refresh)?;
        Ok(StoreIndexView {
            index_url,
            cached,
            index,
            installed: installed_services(console)?,
        })
    }

    pub fn github_releases(&self, repo: &str, per_page: u8) -> Result<GithubReleaseListView> {
        github_releases(repo, per_page)
    }

    pub fn import_release(
        &self,
        console: &mut OrchestratorActionConsole,
        repo_root: &Path,
        request: StoreImportRequest,
    ) -> Result<StoreImportResult> {
        let source_url = required_text(&request.source_url, "source_url")?;
        ensure_package_loading()?;
        let checksum = optional_text(&request.checksum);
        ensure_checksum(checksum)?;
        let imported = import_release_metadata(console, repo_root, source_url, checksum)?;
        Ok(StoreImportResult { imported })
    }

    pub fn install(
        &self,
        console: &mut OrchestratorActionConsole,
        repo_root: &Path,
        request: StoreInstallRequest,
    ) -> Result<StoreInstallResult> {
        let source_url = optional_text(&request.source_url);
        let requested_service_id = optional_text(&request.service_id).map(str::to_string);
        if source_url.is_none() && requested_service_id.is_none() {
            return Err(StoreRequestError::new(400, "service_id or source_url is required").into());
        }
        // Resolve and validate the runtime target before importing metadata so a bad target
        // cannot leave a partially imported release behind.
        let normalized = normalize_store_install_request(console, &request)?;
        ensure_package_loading()?;
        let mut imported = None;
        if let Some(source_url) = source_url {
            let checksum = optional_text(&request.checksum);
            ensure_checksum(checksum)?;
            imported = Some(import_release_metadata(
                console, repo_root, source_url, checksum,
            )?);
        }
        let service_id = requested_service_id
            .or_else(|| imported.as_ref().map(|value| value.service.id.clone()))
            .ok_or_else(|| StoreRequestError::new(400, "service_id or source_url is required"))?;
        let version = optional_text(&request.version)
            .map(str::to_string)
            .or_else(|| imported.as_ref().map(|value| value.release.version.clone()));
        let action = release_install_action(
            &service_id,
            version.as_deref(),
            &request.gateway_node_id,
            &normalized,
        )?;
        validate_required_action_fields(console, &action)?;
        let mut action_result = console.dispatch(action)?;
        extend_unique(&mut action_result.warnings, &normalized.warnings);

        let mut lifecycle_action_result = None;
        if normalized.mode == InstallModeV2::Managed
            && !normalized.start
            && action_succeeded(&action_result)
        {
            let stop = release_stop_action(&service_id, version.as_deref(), &normalized);
            validate_required_action_fields(console, &stop)?;
            lifecycle_action_result = Some(console.dispatch(stop)?);
        }

        let final_action = lifecycle_action_result.as_ref().unwrap_or(&action_result);
        let deployment = console.view()?.deployments.into_iter().find(|deployment| {
            deployment.service_id == service_id && deployment.host_ip == normalized.host_ip
        });
        let lifecycle = observed_install_lifecycle(
            &final_action.status,
            deployment
                .as_ref()
                .map(|deployment| deployment.status.as_str()),
        );
        let deployment_status = deployment
            .as_ref()
            .map(|deployment| deployment.status.clone())
            .unwrap_or_default();
        let installed = matches!(lifecycle, ReleaseStateV2::Running | ReleaseStateV2::Stopped);
        let endpoint = deployment
            .as_ref()
            .map(|deployment| deployment.endpoint.clone())
            .filter(|endpoint| !endpoint.trim().is_empty())
            .unwrap_or_else(|| normalized.endpoint.clone());
        let runtime = StoreInstallRuntimeView {
            mode: normalized.mode,
            target_node_id: normalized.target_node_id.clone(),
            host_ip: normalized.host_ip.clone(),
            endpoint,
            desired_state: if normalized.start {
                RuntimeDesiredStateV2::Running
            } else {
                RuntimeDesiredStateV2::Stopped
            },
            observed_state: projected_runtime_state(Some(deployment_status.as_str())),
            deployment_status,
            operation_id: final_action.operation_id.clone(),
            operation_status: final_action.status.clone(),
        };
        Ok(StoreInstallResult {
            service_id,
            imported,
            action_result,
            lifecycle_action_result,
            lifecycle,
            installed,
            runtime,
            warnings: normalized.warnings,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedStoreInstallRequest {
    mode: InstallModeV2,
    target_node_id: String,
    host_ip: String,
    endpoint: String,
    start: bool,
    migration_policy: MigrationPolicyV2,
    execute_service_driver: bool,
    external_service_running: bool,
    warnings: Vec<String>,
}

fn normalize_store_install_request(
    console: &OrchestratorActionConsole,
    request: &StoreInstallRequest,
) -> Result<NormalizedStoreInstallRequest> {
    if request.execute_service_driver && request.external_service_running {
        return Err(StoreRequestError::new(
            400,
            "deprecated execute_service_driver and external_service_running cannot both be true",
        )
        .into());
    }

    let mut warnings = Vec::new();
    let mut mode = request.mode;
    if request.external_service_running {
        if optional_text(&request.target_node_id).is_some() {
            return Err(StoreRequestError::new(
                400,
                "external_service_running conflicts with target_node_id",
            )
            .into());
        }
        mode = InstallModeV2::External;
        warnings.push(
            "external_service_running is deprecated; use mode=External and endpoint".to_string(),
        );
    }
    if request.execute_service_driver {
        warnings.push(
            "execute_service_driver is deprecated; Managed installs execute the driver by default"
                .to_string(),
        );
    }

    let migration_policy = if request.migration_dry_run {
        if request.migration_policy == MigrationPolicyV2::Skip {
            return Err(StoreRequestError::new(
                400,
                "migration_dry_run conflicts with migration_policy=Skip",
            )
            .into());
        }
        warnings.push("migration_dry_run is deprecated; use migration_policy=DryRun".to_string());
        MigrationPolicyV2::DryRun
    } else {
        request.migration_policy
    };
    match mode {
        InstallModeV2::Managed => {
            let nodes = console.nodes()?;
            let target_node_id = optional_text(&request.target_node_id);
            let legacy_host_ip = optional_text(&request.host_ip);
            let matches = if let Some(node_id) = target_node_id {
                nodes
                    .iter()
                    .filter(|node| node.node_id == node_id)
                    .collect::<Vec<_>>()
            } else if let Some(host_ip) = legacy_host_ip {
                warnings.push(
                    "host_ip is deprecated for Store installs; use target_node_id".to_string(),
                );
                nodes
                    .iter()
                    .filter(|node| node.host_ip == host_ip)
                    .collect::<Vec<_>>()
            } else {
                return Err(StoreRequestError::new(
                    400,
                    "target_node_id is required for a Managed install",
                )
                .into());
            };
            let node = match matches.as_slice() {
                [] => {
                    return Err(StoreRequestError::new(
                        404,
                        target_node_id.map_or_else(
                            || format!("no Node is registered for host_ip {}", request.host_ip),
                            |node_id| format!("target Node {node_id} was not found"),
                        ),
                    )
                    .into());
                }
                [node] => *node,
                _ => {
                    return Err(StoreRequestError::new(
                        409,
                        "the Managed target does not resolve to a unique Node",
                    )
                    .into());
                }
            };
            if nodes
                .iter()
                .filter(|candidate| candidate.host_ip == node.host_ip)
                .count()
                != 1
            {
                return Err(StoreRequestError::new(
                    409,
                    format!("Node host {} is not unique", node.host_ip),
                )
                .into());
            }
            if let Some(host_ip) = legacy_host_ip
                && host_ip != node.host_ip
            {
                return Err(StoreRequestError::new(
                    400,
                    format!(
                        "host_ip {host_ip} conflicts with target Node {} host {}",
                        node.node_id, node.host_ip
                    ),
                )
                .into());
            }
            if target_node_id.is_some() && legacy_host_ip.is_some() {
                warnings.push(
                    "host_ip is deprecated and was only checked against target_node_id".to_string(),
                );
            }
            let endpoint = optional_text(&request.endpoint).unwrap_or_default();
            if !endpoint.is_empty() {
                let identity =
                    orchestrator_legacy::parse_endpoint_id(endpoint).map_err(|error| {
                        StoreRequestError::new(400, format!("invalid endpoint: {error}"))
                    })?;
                if identity.host != node.host_ip {
                    return Err(StoreRequestError::new(
                        400,
                        format!(
                            "endpoint host {} conflicts with target Node {} host {}",
                            identity.host, node.node_id, node.host_ip
                        ),
                    )
                    .into());
                }
            }
            Ok(NormalizedStoreInstallRequest {
                mode,
                target_node_id: node.node_id.clone(),
                host_ip: node.host_ip.clone(),
                endpoint: endpoint.to_string(),
                start: request.start,
                migration_policy,
                execute_service_driver: true,
                external_service_running: false,
                warnings,
            })
        }
        InstallModeV2::External => {
            if request.execute_service_driver {
                return Err(StoreRequestError::new(
                    400,
                    "mode=External conflicts with execute_service_driver=true",
                )
                .into());
            }
            if optional_text(&request.target_node_id).is_some() {
                return Err(StoreRequestError::new(
                    400,
                    "mode=External must not set target_node_id",
                )
                .into());
            }
            if !request.start {
                return Err(StoreRequestError::new(
                    400,
                    "mode=External requires start=true because it registers an already-running endpoint",
                )
                .into());
            }
            let endpoint = required_text(&request.endpoint, "endpoint")?;
            let identity = orchestrator_legacy::parse_endpoint_id(endpoint).map_err(|error| {
                StoreRequestError::new(400, format!("invalid external endpoint: {error}"))
            })?;
            if let Some(host_ip) = optional_text(&request.host_ip) {
                if host_ip != identity.host {
                    return Err(StoreRequestError::new(
                        400,
                        format!(
                            "deprecated host_ip {host_ip} conflicts with endpoint host {}",
                            identity.host
                        ),
                    )
                    .into());
                }
                warnings.push(
                    "host_ip is deprecated for External installs; endpoint determines the host"
                        .to_string(),
                );
            }
            Ok(NormalizedStoreInstallRequest {
                mode,
                target_node_id: String::new(),
                host_ip: identity.host.to_string(),
                endpoint: endpoint.to_string(),
                start: true,
                migration_policy,
                execute_service_driver: false,
                external_service_running: true,
                warnings,
            })
        }
    }
}

fn release_install_action(
    service_id: &str,
    version: Option<&str>,
    gateway_node_id: &str,
    normalized: &NormalizedStoreInstallRequest,
) -> Result<ActionRequest> {
    if !normalized.endpoint.is_empty() {
        orchestrator_legacy::validate_endpoint_service_name(&normalized.endpoint, service_id)
            .map_err(|error| {
                StoreRequestError::new(400, format!("endpoint does not match service: {error}"))
            })?;
    }
    let mut action = ActionRequest::new("", "release.install", BTreeMap::new());
    action
        .fields
        .insert("service_id".to_string(), service_id.to_string());
    action
        .fields
        .insert("confirm".to_string(), "true".to_string());
    action.fields.insert(
        "target_node_id".to_string(),
        normalized.target_node_id.clone(),
    );
    action
        .fields
        .insert("host_ip".to_string(), normalized.host_ip.clone());
    action.fields.insert(
        "mode".to_string(),
        match normalized.mode {
            InstallModeV2::Managed => "Managed",
            InstallModeV2::External => "External",
        }
        .to_string(),
    );
    action
        .fields
        .insert("start".to_string(), normalized.start.to_string());
    action.fields.insert(
        "migration_policy".to_string(),
        match normalized.migration_policy {
            MigrationPolicyV2::Apply => "Apply",
            MigrationPolicyV2::DryRun => "DryRun",
            MigrationPolicyV2::Skip => "Skip",
        }
        .to_string(),
    );
    action.fields.insert(
        "execute_service_driver".to_string(),
        normalized.execute_service_driver.to_string(),
    );
    action.fields.insert(
        "external_service_running".to_string(),
        normalized.external_service_running.to_string(),
    );
    action.fields.insert(
        "migration_dry_run".to_string(),
        (normalized.migration_policy == MigrationPolicyV2::DryRun).to_string(),
    );
    if let Some(version) = version {
        action
            .fields
            .insert("version".to_string(), version.to_string());
    }
    insert_optional(&mut action, "endpoint", &normalized.endpoint);
    insert_optional(&mut action, "gateway_node_id", gateway_node_id);
    Ok(action)
}

fn release_stop_action(
    service_id: &str,
    version: Option<&str>,
    normalized: &NormalizedStoreInstallRequest,
) -> ActionRequest {
    let mut action = ActionRequest::new("", "service.stop", BTreeMap::new());
    action
        .fields
        .insert("service_id".to_string(), service_id.to_string());
    action
        .fields
        .insert("confirm".to_string(), "true".to_string());
    action
        .fields
        .insert("host_ip".to_string(), normalized.host_ip.clone());
    action
        .fields
        .insert("execute_service_driver".to_string(), "true".to_string());
    if let Some(version) = version {
        action
            .fields
            .insert("version".to_string(), version.to_string());
    }
    insert_optional(&mut action, "endpoint", &normalized.endpoint);
    action
}

fn import_release_metadata(
    console: &mut OrchestratorActionConsole,
    repo_root: &Path,
    source_url: &str,
    checksum: Option<&str>,
) -> Result<ExternalReleaseImport> {
    Ok(console.import_external_release(repo_root, source_url, checksum)?)
}

fn action_succeeded(result: &ActionDispatchResult) -> bool {
    result.status.eq_ignore_ascii_case("SUCCEEDED") && result.error.trim().is_empty()
}

fn observed_install_lifecycle(
    operation_status: &str,
    deployment_status: Option<&str>,
) -> ReleaseStateV2 {
    if !operation_status.eq_ignore_ascii_case("SUCCEEDED") {
        return if matches!(
            operation_status.trim().to_ascii_uppercase().as_str(),
            "PLANNED" | "AWAITING_CONFIRMATION" | "RUNNING" | "QUEUED" | "LEASED"
        ) {
            ReleaseStateV2::Deploying
        } else {
            ReleaseStateV2::Failed
        };
    }
    match projected_runtime_state(deployment_status) {
        ReleaseStateV2::Imported => ReleaseStateV2::Failed,
        state => state,
    }
}

fn projected_runtime_state(deployment_status: Option<&str>) -> ReleaseStateV2 {
    match deployment_status
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "running" => ReleaseStateV2::Running,
        "stopped" => ReleaseStateV2::Stopped,
        "failed" => ReleaseStateV2::Failed,
        "installing" | "dispatching" | "starting" | "planned" | "deferred" => {
            ReleaseStateV2::Deploying
        }
        _ => ReleaseStateV2::Imported,
    }
}

fn extend_unique(target: &mut Vec<String>, additions: &[String]) {
    for warning in additions {
        if !target.contains(warning) {
            target.push(warning.clone());
        }
    }
}

pub fn installed_services(
    console: &OrchestratorActionConsole,
) -> Result<BTreeMap<String, InstalledServiceView>> {
    installed_services_from_deployments(console.view()?.deployments)
}

pub fn installed_services_from_deployments(
    deployments: Vec<DeploymentViewRow>,
) -> Result<BTreeMap<String, InstalledServiceView>> {
    let mut grouped = BTreeMap::<String, Vec<DeploymentViewRow>>::new();
    for deployment in deployments {
        grouped
            .entry(deployment.service_id.clone())
            .or_default()
            .push(deployment);
    }
    let mut installed = BTreeMap::new();
    for (service_id, mut deployments) in grouped {
        deployments.sort_by(|left, right| {
            left.host_ip
                .cmp(&right.host_ip)
                .then_with(|| left.version.cmp(&right.version))
        });
        let versions = deployments
            .iter()
            .map(|deployment| deployment.version.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let kind = deployments
            .first()
            .map(|deployment| deployment.kind.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let deployment_rows = deployments
            .into_iter()
            .map(|deployment| StoreDeploymentView {
                version: deployment.version,
                host_ip: deployment.host_ip,
                status: deployment.status,
            })
            .collect();
        installed.insert(
            service_id,
            InstalledServiceView {
                version: versions.join(" / "),
                versions,
                kind,
                deployments: deployment_rows,
            },
        );
    }
    Ok(installed)
}

pub fn modules_from_index(index: &Value) -> Result<Vec<StoreModuleView>> {
    let modules = index
        .get("modules")
        .cloned()
        .ok_or_else(|| StoreRequestError::new(400, "store index is missing modules"))?;
    serde_json::from_value(modules)
        .map_err(|err| StoreRequestError::new(400, format!("invalid store modules: {err}")).into())
}

pub fn configured_index_url() -> String {
    std::env::var("OJOS_STORE_INDEX_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_STORE_INDEX_PATH.to_string())
}

fn github_releases(repo: &str, per_page: u8) -> Result<GithubReleaseListView> {
    let repo = repo.trim();
    if !valid_repo_slug(repo) {
        return Err(StoreRequestError::new(400, "repo must look like owner/name").into());
    }
    let per_page = per_page.clamp(1, 30);
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page={per_page}");
    let document = http_get_json(&url, true)?;
    let releases = document
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|release| GithubReleaseView {
            tag_name: json_string(&release, "tag_name"),
            name: json_string(&release, "name"),
            prerelease: release["prerelease"].as_bool().unwrap_or(false),
            published_at: json_string(&release, "published_at"),
            html_url: json_string(&release, "html_url"),
            assets: release["assets"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|asset| GithubAssetView {
                    name: json_string(&asset, "name"),
                    size: asset["size"].as_u64().unwrap_or(0),
                    browser_download_url: json_string(&asset, "browser_download_url"),
                    content_type: json_string(&asset, "content_type"),
                })
                .collect(),
        })
        .collect();
    Ok(GithubReleaseListView {
        repo: repo.to_string(),
        releases,
    })
}

fn json_string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

fn load_index_document(repo_root: &Path, index_url: &str) -> Result<Value> {
    if index_url.starts_with("http://") || index_url.starts_with("https://") {
        return http_get_json(index_url, false);
    }
    let relative = index_url
        .strip_prefix("file://")
        .unwrap_or(index_url)
        .trim_start_matches('/');
    let path = safe_repo_child(repo_root, relative)?;
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read store index {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse store index {}", path.display()))
}

fn safe_repo_child(repo_root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(StoreRequestError::new(
            400,
            "store index path must stay inside the repository",
        )
        .into());
    }
    Ok(repo_root.join(relative))
}

fn http_get_json(url: &str, github: bool) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let mut current = url.trim().to_string();
    let mut hops = 0_u32;
    let (status, body) = loop {
        orchestrator_legacy::validate_outbound_url(&current)
            .map_err(|err| anyhow!("fetch {url} blocked: {err}"))?;
        let mut request = agent.get(&current).header("User-Agent", USER_AGENT);
        if github
            && hops == 0
            && let Some(token) = github_token()
        {
            request = request.header("Authorization", format!("Bearer {token}").as_str());
            request = request.header("Accept", "application/vnd.github+json");
        }
        let response = request
            .call()
            .map_err(|err| anyhow!("fetch {url} failed: {err}"))?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if hops >= MAX_REDIRECTS {
                return Err(anyhow!(
                    "fetch {url} failed: more than {MAX_REDIRECTS} redirects"
                ));
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| anyhow!("fetch {url} failed: redirect has no location"))?;
            current = orchestrator_legacy::resolve_outbound_redirect(&current, location)
                .map_err(|err| anyhow!("fetch {url} failed: {err}"))?;
            hops += 1;
            continue;
        }
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_FETCH_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .with_context(|| format!("read {url}"))?;
        break (status, body);
    };
    if body.len() > MAX_FETCH_BYTES {
        return Err(anyhow!(
            "response from {url} exceeds {MAX_FETCH_BYTES} bytes"
        ));
    }
    if !(200..=299).contains(&status) {
        return Err(anyhow!("fetch {url} failed: http {status}"));
    }
    serde_json::from_slice(&body).with_context(|| format!("parse JSON from {url}"))
}

fn valid_repo_slug(repo: &str) -> bool {
    let parts = repo.split('/').collect::<Vec<_>>();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 100
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
        })
}

fn validate_required_action_fields(
    console: &OrchestratorActionConsole,
    request: &ActionRequest,
) -> Result<()> {
    let form = console
        .action_form(&request.action)
        .ok_or_else(|| StoreRequestError::new(400, format!("unknown action {}", request.action)))?;
    let missing = form
        .fields
        .iter()
        .filter(|field| field.required && request.field(&field.name).is_none())
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(StoreRequestError::new(
            400,
            format!(
                "{} requires form field{} {}",
                request.action,
                if missing.len() == 1 { "" } else { "s" },
                missing.join(", ")
            ),
        )
        .into());
    }
    Ok(())
}

fn insert_optional(request: &mut ActionRequest, key: &str, value: &str) {
    if let Some(value) = optional_text(value) {
        request.fields.insert(key.to_string(), value.to_string());
    }
}

fn optional_text(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn required_text<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    optional_text(value)
        .ok_or_else(|| StoreRequestError::new(400, format!("{field} is required")).into())
}

fn ensure_package_loading() -> Result<()> {
    if package_load_enabled() {
        Ok(())
    } else {
        Err(StoreRequestError::new(
            403,
            "external package load is disabled; set ORCHESTRATOR_RELEASE_PACKAGE_LOAD=1 (dev/staging only) to allow store import and install",
        )
        .into())
    }
}

fn ensure_checksum(checksum: Option<&str>) -> Result<()> {
    if require_release_checksum() && checksum.is_none() {
        Err(StoreRequestError::new(
            400,
            "checksum is required while ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM is enabled; provide sha256:<hex>",
        )
        .into())
    } else {
        Ok(())
    }
}

fn package_load_enabled() -> bool {
    env_flag("ORCHESTRATOR_RELEASE_PACKAGE_LOAD")
}

fn require_release_checksum() -> bool {
    env_flag("ORCHESTRATOR_REQUIRE_RELEASE_CHECKSUM")
}

fn allow_private_release_source() -> bool {
    env_flag("ORCHESTRATOR_ALLOW_PRIVATE_RELEASE_SOURCE")
}

fn github_token() -> Option<String> {
    ["OJOS_GITHUB_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::NodeRecord;
    use serde_json::json;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }

    fn console_with_node(node_id: &str, host_ip: &str) -> OrchestratorActionConsole {
        let mut console =
            OrchestratorActionConsole::load_with_database_url(repo_root(), None).unwrap();
        console
            .upsert_node(NodeRecord {
                node_id: node_id.to_string(),
                host_ip: host_ip.to_string(),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({}),
                status: "ready".to_string(),
                created_at: String::new(),
                updated_at: String::new(),
            })
            .unwrap();
        console
    }

    #[test]
    fn repository_manifests_are_not_reported_as_installed() {
        let console = OrchestratorActionConsole::load_with_database_url(repo_root(), None).unwrap();
        assert!(!console.services().unwrap().is_empty());
        assert!(installed_services(&console).unwrap().is_empty());
    }

    #[test]
    fn importing_release_metadata_never_creates_a_deployment() {
        let mut console =
            OrchestratorActionConsole::load_with_database_url(repo_root(), None).unwrap();
        assert!(console.view().unwrap().deployments.is_empty());
        let imported =
            import_release_metadata(&mut console, &repo_root(), "services/gateway", None).unwrap();
        assert_eq!(imported.service.id, "gateway");
        assert!(console.view().unwrap().deployments.is_empty());
        assert!(installed_services(&console).unwrap().is_empty());
    }

    #[test]
    fn v2_defaults_resolve_node_and_request_real_driver_start() {
        let console = console_with_node("node-1", "10.0.0.8");
        let decoded: StoreInstallRequest =
            serde_json::from_str(r#"{"service_id":"gateway","target_node_id":"node-1"}"#).unwrap();
        assert_eq!(decoded.mode, InstallModeV2::Managed);
        assert!(decoded.start);
        assert_eq!(decoded.migration_policy, MigrationPolicyV2::Apply);
        assert!(!decoded.execute_service_driver, "legacy field stays inert");

        let normalized = normalize_store_install_request(&console, &decoded).unwrap();
        assert_eq!(normalized.target_node_id, "node-1");
        assert_eq!(normalized.host_ip, "10.0.0.8");
        assert!(normalized.execute_service_driver);
        assert!(!normalized.external_service_running);
        let action = release_install_action("gateway", Some("1.2.3"), "", &normalized)
            .expect("default install action");
        assert_eq!(action.field("target_node_id"), Some("node-1"));
        assert_eq!(action.field("host_ip"), Some("10.0.0.8"));
        assert_eq!(action.field("start"), Some("true"));
        assert_eq!(action.field("execute_service_driver"), Some("true"));
        assert_eq!(action.field("external_service_running"), Some("false"));
        assert_eq!(action.field("migration_dry_run"), Some("false"));
    }

    #[test]
    fn legacy_host_and_flags_map_to_v2_without_allowing_planned_installs() {
        let console = console_with_node("node-legacy", "10.0.0.9");
        let managed = StoreInstallRequest {
            service_id: "gateway".to_string(),
            host_ip: "10.0.0.9".to_string(),
            execute_service_driver: false,
            migration_dry_run: true,
            ..StoreInstallRequest::default()
        };
        let normalized = normalize_store_install_request(&console, &managed).unwrap();
        assert_eq!(normalized.target_node_id, "node-legacy");
        assert!(normalized.execute_service_driver);
        assert_eq!(normalized.migration_policy, MigrationPolicyV2::DryRun);
        assert!(
            normalized
                .warnings
                .iter()
                .any(|warning| warning.contains("host_ip"))
        );
        assert!(
            normalized
                .warnings
                .iter()
                .any(|warning| warning.contains("migration_dry_run"))
        );

        let external = StoreInstallRequest {
            service_id: "gateway".to_string(),
            endpoint: "10.0.0.20:8080:gateway".to_string(),
            external_service_running: true,
            ..StoreInstallRequest::default()
        };
        let normalized = normalize_store_install_request(&console, &external).unwrap();
        assert_eq!(normalized.mode, InstallModeV2::External);
        assert_eq!(normalized.host_ip, "10.0.0.20");
        assert!(!normalized.execute_service_driver);
        assert!(normalized.external_service_running);
    }

    #[test]
    fn new_and_legacy_install_controls_reject_ambiguous_combinations() {
        let console = console_with_node("node-1", "10.0.0.8");
        let both_legacy_modes = StoreInstallRequest {
            service_id: "gateway".to_string(),
            endpoint: "10.0.0.20:8080:gateway".to_string(),
            execute_service_driver: true,
            external_service_running: true,
            ..StoreInstallRequest::default()
        };
        let error = normalize_store_install_request(&console, &both_legacy_modes).unwrap_err();
        assert_eq!(
            error.downcast_ref::<StoreRequestError>().unwrap().status(),
            400
        );

        let external_with_node = StoreInstallRequest {
            service_id: "gateway".to_string(),
            mode: InstallModeV2::External,
            target_node_id: "node-1".to_string(),
            endpoint: "10.0.0.8:8080:gateway".to_string(),
            ..StoreInstallRequest::default()
        };
        let error = normalize_store_install_request(&console, &external_with_node).unwrap_err();
        assert_eq!(
            error.downcast_ref::<StoreRequestError>().unwrap().status(),
            400
        );

        let missing_node = StoreInstallRequest {
            service_id: "gateway".to_string(),
            target_node_id: "missing".to_string(),
            ..StoreInstallRequest::default()
        };
        let error = normalize_store_install_request(&console, &missing_node).unwrap_err();
        assert_eq!(
            error.downcast_ref::<StoreRequestError>().unwrap().status(),
            404
        );
    }

    #[test]
    fn start_false_is_a_real_install_followed_by_a_driver_authorized_stop() {
        let console = console_with_node("node-1", "10.0.0.8");
        let request = StoreInstallRequest {
            service_id: "gateway".to_string(),
            target_node_id: "node-1".to_string(),
            start: false,
            ..StoreInstallRequest::default()
        };
        let normalized = normalize_store_install_request(&console, &request).unwrap();
        assert!(normalized.execute_service_driver);
        let install = release_install_action("gateway", None, "", &normalized).unwrap();
        assert_eq!(install.field("execute_service_driver"), Some("true"));
        let stop = release_stop_action("gateway", None, &normalized);
        assert_eq!(stop.action, "service.stop");
        assert_eq!(stop.field("confirm"), Some("true"));
        assert_eq!(stop.field("execute_service_driver"), Some("true"));
        assert_eq!(stop.field("host_ip"), Some("10.0.0.8"));
    }

    #[test]
    fn planned_and_failed_operations_are_never_terminally_installed() {
        for (operation, deployment, expected) in [
            ("PLANNED", Some("planned"), ReleaseStateV2::Deploying),
            ("RUNNING", Some("starting"), ReleaseStateV2::Deploying),
            ("FAILED", Some("running"), ReleaseStateV2::Failed),
            ("SUCCEEDED", None, ReleaseStateV2::Failed),
            ("SUCCEEDED", Some("deferred"), ReleaseStateV2::Deploying),
            ("SUCCEEDED", Some("running"), ReleaseStateV2::Running),
            ("SUCCEEDED", Some("stopped"), ReleaseStateV2::Stopped),
        ] {
            let lifecycle = observed_install_lifecycle(operation, deployment);
            assert_eq!(lifecycle, expected);
            let installed = matches!(lifecycle, ReleaseStateV2::Running | ReleaseStateV2::Stopped);
            assert_eq!(
                installed,
                matches!(expected, ReleaseStateV2::Running | ReleaseStateV2::Stopped)
            );
        }
    }

    #[test]
    fn local_index_is_typed_and_cacheable() {
        let console = OrchestratorActionConsole::load_with_database_url(repo_root(), None).unwrap();
        let catalog = StoreCatalog::new();
        let first = catalog.index(&console, &repo_root(), false).unwrap();
        let second = catalog.index(&console, &repo_root(), false).unwrap();
        assert!(!first.cached);
        assert!(second.cached);
        let modules =
            serde_json::from_value::<Vec<StoreModuleView>>(first.index["modules"].clone()).unwrap();
        assert!(modules.iter().any(|module| module.id == "gateway"));
    }

    #[test]
    fn github_repo_slug_is_strict() {
        assert!(valid_repo_slug("owner/repo"));
        assert!(!valid_repo_slug("owner"));
        assert!(!valid_repo_slug("owner/repo/extra"));
        assert!(!valid_repo_slug("owner/re po"));
    }
}
