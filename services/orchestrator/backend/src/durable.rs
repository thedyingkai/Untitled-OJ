//! Backend-facing adapter over the two production persistence dialects.
//!
//! HTTP handlers use this type instead of branching on SQLite.  Keeping the
//! branch here makes it impossible for the PostgreSQL daemon to advertise a
//! capability whose mutation is rejected only because a route was wired to a
//! concrete SQLite type.

use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, DurableOperation, HeartbeatRequest, Job, JobError, JobEvent,
    JobStore, NewJob, OperationRepository, OperationStoreError, ResolveExpiredSuccessRequest,
};
use orchestrator_legacy::{
    ApiBindingState, NodeRecord, OrchestratorStore, ServiceReleaseContract, TopologyEndpointSpec,
    TopologyRevision, TopologySpec, TopologyStatus, api_version_matches,
    release_supports_link_probe_v1, validate_service_release,
};
use orchestrator_storage::{
    ApiBinding, AuditRecord, CertificateActivation, CertificateRotation,
    ControlPlaneAnomalyCounters, EnrollmentLookup, EnrollmentRedemption, HistoryRetentionReport,
    IdempotencyBegin, JobMetricsSnapshot, NewAuditRecord, NewNodeCertificate,
    NodeCertificateRecord, NodeEnrollmentCode, PostgresError, PostgresJobStore,
    PostgresOperationStore, PostgresOrchestratorStore, SqliteJobStore, SqliteOperationStore,
    SqliteOrchestratorStore, StorageError, StoredIdempotentResponse, StoredNodeRuntimeFacts,
    StoredRuntimeInstance, TopologyApplyOutcome, TopologyHeads,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub(crate) const MANAGED_RUNTIME_REPORT_STALE_MS: i64 = 60_000;
pub(crate) const EXTERNAL_RUNTIME_PROBE_STALE_MS: i64 = 60_000;

#[derive(Debug, Clone)]
pub(crate) enum DurableStore {
    Sqlite(SqliteOrchestratorStore),
    Postgres(PostgresOrchestratorStore),
}

impl DurableStore {
    pub(crate) fn service_release_contract(
        &self,
        service_id: &str,
        version: &str,
    ) -> Result<Option<ServiceReleaseContract>, DurableError> {
        let releases = match self {
            Self::Sqlite(store) => store.list_service_releases(),
            Self::Postgres(store) => store.list_service_releases(),
        }
        .map_err(|error| DurableError::Storage(error.to_string()))?;
        releases
            .into_iter()
            .find(|release| release.service_name == service_id && release.version == version)
            .map(|release| {
                ServiceReleaseContract::from_json_value(release.manifest)
                    .map_err(|error| DurableError::Invariant(error.to_string()))
            })
            .transpose()
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "sqlite",
            Self::Postgres(_) => "postgres",
        }
    }

    pub(crate) fn readiness(&self) -> Result<Value, DurableError> {
        match self {
            Self::Sqlite(store) => {
                let report = store.readiness().map_err(DurableError::from)?;
                let legacy_import = store.legacy_import_report().map_err(DurableError::from)?;
                Ok(json!({
                    "store": "sqlite",
                    "schema_version": report.schema_version,
                    "expected_schema_version": report.expected_schema_version,
                    "journal_mode": report.journal_mode,
                    "foreign_keys": report.foreign_keys,
                    "legacy_import": legacy_import,
                }))
            }
            Self::Postgres(store) => {
                let report = store.readiness().map_err(DurableError::from)?;
                let legacy_import = store.legacy_import_report().map_err(DurableError::from)?;
                Ok(json!({
                    "store": "postgres",
                    "schema_version": report.schema_version,
                    "expected_schema_version": report.expected_schema_version,
                    "tls_enabled": report.tls_enabled,
                    "in_recovery": report.in_recovery,
                    "pool_connections": report.pool_connections,
                    "pool_idle_connections": report.pool_idle_connections,
                    "legacy_import": legacy_import,
                }))
            }
        }
    }

    pub(crate) fn as_sqlite(&self) -> Option<&SqliteOrchestratorStore> {
        match self {
            Self::Sqlite(store) => Some(store),
            Self::Postgres(_) => None,
        }
    }

    pub(crate) fn job_store(&self) -> DurableJobStore {
        match self {
            Self::Sqlite(store) => DurableJobStore::Sqlite(SqliteJobStore::new(store.clone())),
            Self::Postgres(store) => {
                DurableJobStore::Postgres(PostgresJobStore::new(store.clone()))
            }
        }
    }

    pub(crate) fn operation_store(&self) -> DurableOperationStore {
        match self {
            Self::Sqlite(store) => {
                DurableOperationStore::Sqlite(SqliteOperationStore::new(store.clone()))
            }
            Self::Postgres(store) => {
                DurableOperationStore::Postgres(PostgresOperationStore::new(store.clone()))
            }
        }
    }

    pub(crate) fn begin_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
        now_ms: i64,
    ) -> Result<IdempotencyBegin, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .begin_idempotent_request(scope, key, request_sha256, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .begin_idempotent_request(scope, key, request_sha256, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn complete_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
        response: &StoredIdempotentResponse,
        now_ms: i64,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .complete_idempotent_request(scope, key, request_sha256, response, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .complete_idempotent_request(scope, key, request_sha256, response, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn abort_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .abort_idempotent_request(scope, key, request_sha256)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .abort_idempotent_request(scope, key, request_sha256)
                .map_err(Into::into),
        }
    }

    pub(crate) fn append_audit_record(
        &self,
        record: NewAuditRecord,
    ) -> Result<AuditRecord, DurableError> {
        match self {
            Self::Sqlite(store) => store.append_audit_record(record).map_err(Into::into),
            Self::Postgres(store) => store.append_audit_record(record).map_err(Into::into),
        }
    }

    #[cfg(test)]
    pub(crate) fn audit_records(
        &self,
        request_id: Option<&str>,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<AuditRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .audit_records(request_id, after_sequence, limit)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .audit_records(request_id, after_sequence, limit)
                .map_err(Into::into),
        }
    }

    pub(crate) fn registered_service_ids(&self) -> Result<BTreeSet<String>, DurableError> {
        let services = match self {
            Self::Sqlite(store) => store.list_services(),
            Self::Postgres(store) => store.list_services(),
        }
        .map_err(|error| DurableError::Storage(error.to_string()))?;
        Ok(services.into_iter().map(|service| service.id).collect())
    }

    pub(crate) fn link_probe_source_endpoints(
        &self,
        spec: &TopologySpec,
    ) -> Result<BTreeSet<String>, LinkProbeBindingError> {
        let runtime_instances = self
            .runtime_instances(None)
            .map_err(|error| LinkProbeBindingError::Storage(error.to_string()))?;
        // One runtime scan and one release query per validation/reconcile,
        // independent of Link cardinality. The two indexes below make the
        // 2,000 Endpoint / 8,000 Link production shape O(E + R + L), while a
        // per-release cache parses each signed manifest at most once.
        let mut runtime_by_endpoint_service =
            BTreeMap::<(&str, &str), Vec<&StoredRuntimeInstance>>::new();
        for stored in &runtime_instances {
            runtime_by_endpoint_service
                .entry((
                    stored.endpoint.as_str(),
                    stored.instance.service_id.as_str(),
                ))
                .or_default()
                .push(stored);
        }
        let releases = match self {
            Self::Sqlite(store) => store.list_service_releases(),
            Self::Postgres(store) => store.list_service_releases(),
        }
        .map_err(|error| LinkProbeBindingError::Storage(error.to_string()))?;
        let releases_by_identity = releases
            .iter()
            .map(|release| {
                (
                    (release.service_name.as_str(), release.version.as_str()),
                    release,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let endpoint_services = spec
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.endpoint.as_str(), endpoint.service_id.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut capable_endpoints = BTreeSet::new();
        let mut release_capability_cache = BTreeMap::<(&str, &str), Result<bool, String>>::new();
        // API-bound Links are enforced through the consumer/provider binding
        // state machine and Gateway health. Requiring the legacy inbound
        // link-probe endpoint as well would make non-listening consumers (for
        // example judge-worker) impossible to connect.
        for link in spec
            .links
            .iter()
            .filter(|link| link.enabled && link.api_bindings.is_empty())
        {
            let service_id = endpoint_services
                .get(link.source_endpoint.as_str())
                .ok_or_else(|| {
                    LinkProbeBindingError::Binding(format!(
                        "link source endpoint {} does not exist",
                        link.source_endpoint
                    ))
                })?;
            let matching = runtime_by_endpoint_service
                .get(&(link.source_endpoint.as_str(), *service_id))
                .map(Vec::as_slice)
                .unwrap_or_default();
            if matching.len() != 1 {
                return Err(LinkProbeBindingError::Binding(format!(
                    "link {} -> {} requires exactly one runtime projection bound to source endpoint {} and service {}; found {}",
                    link.source_endpoint,
                    link.target_endpoint,
                    link.source_endpoint,
                    service_id,
                    matching.len()
                )));
            }
            let release_version = matching[0].instance.release_version.trim();
            if release_version.is_empty() {
                return Err(LinkProbeBindingError::Binding(format!(
                    "link source endpoint {} has no deterministic release version binding; controlled reprovision is required",
                    link.source_endpoint
                )));
            }
            let capability = release_capability_cache
                .entry((*service_id, release_version))
                .or_insert_with(|| {
                    let release = releases_by_identity
                        .get(&(*service_id, release_version))
                        .ok_or_else(|| "the exact release record is missing".to_string())?;
                    let manifest =
                        ServiceReleaseContract::from_json_value(release.manifest.clone())
                            .map_err(|error| format!("release manifest is invalid: {error}"))?
                            .release;
                    validate_service_release(&manifest)
                        .map_err(|error| format!("release manifest is invalid: {error}"))?;
                    if manifest.service_name.as_str() != *service_id
                        || manifest.version != release_version
                    {
                        return Err(
                            "release record identity does not match its manifest".to_string()
                        );
                    }
                    Ok(release_supports_link_probe_v1(&manifest))
                })
                .clone()
                .map_err(|detail| {
                    LinkProbeBindingError::Binding(format!(
                        "link source endpoint {} is bound to invalid release {}@{}: {detail}",
                        link.source_endpoint, service_id, release_version
                    ))
                })?;
            if !capability {
                return Err(LinkProbeBindingError::Capability(format!(
                    "link {} -> {} source release {}@{} does not declare orchestrator.link-probe.v1",
                    link.source_endpoint, link.target_endpoint, service_id, release_version
                )));
            }
            capable_endpoints.insert(link.source_endpoint.clone());
        }
        Ok(capable_endpoints)
    }

    pub(crate) fn validate_topology_api_bindings(
        &self,
        spec: &TopologySpec,
    ) -> Result<(), TopologyApiBindingError> {
        if spec.links.iter().all(|link| link.api_bindings.is_empty()) {
            return Ok(());
        }
        let runtime_instances = self
            .runtime_instances(None)
            .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?;
        let releases = match self {
            Self::Sqlite(store) => store.list_service_releases(),
            Self::Postgres(store) => store.list_service_releases(),
        }
        .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?;
        let mut contracts = BTreeMap::new();
        for release in releases {
            let contract = ServiceReleaseContract::from_json_value(release.manifest.clone())
                .map_err(|error| {
                    TopologyApiBindingError::Contract(format!(
                        "release {}@{} has invalid Service Contract: {error}",
                        release.service_name, release.version
                    ))
                })?;
            contracts.insert((release.service_name, release.version), contract);
        }
        let endpoint_specs = spec
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
            .collect::<BTreeMap<_, _>>();
        for link in spec
            .links
            .iter()
            .filter(|link| link.enabled && !link.api_bindings.is_empty())
        {
            let source = exact_runtime_for_topology_endpoint(
                &runtime_instances,
                endpoint_specs[link.source_endpoint.as_str()],
            )?;
            let target = exact_runtime_for_topology_endpoint(
                &runtime_instances,
                endpoint_specs[link.target_endpoint.as_str()],
            )?;
            ensure_running_healthy_runtime(self, source, "consumer")?;
            ensure_running_healthy_runtime(self, target, "provider")?;
            let source_contract = contracts
                .get(&(
                    source.instance.service_id.clone(),
                    source.instance.release_version.clone(),
                ))
                .ok_or_else(|| {
                    TopologyApiBindingError::Contract(format!(
                        "consumer deployment {} has no exact registered release {}@{}",
                        source.instance.deployment_id,
                        source.instance.service_id,
                        source.instance.release_version
                    ))
                })?;
            let target_contract = contracts
                .get(&(
                    target.instance.service_id.clone(),
                    target.instance.release_version.clone(),
                ))
                .ok_or_else(|| {
                    TopologyApiBindingError::Contract(format!(
                        "provider deployment {} has no exact registered release {}@{}",
                        target.instance.deployment_id,
                        target.instance.service_id,
                        target.instance.release_version
                    ))
                })?;

            for binding in &link.api_bindings {
                let requirement = source_contract
                    .requirements()
                    .iter()
                    .find(|requirement| requirement.binding_name() == binding.requirement_name)
                    .ok_or_else(|| {
                        TopologyApiBindingError::Binding(format!(
                            "consumer release {}@{} does not require binding {}",
                            source.instance.service_id,
                            source.instance.release_version,
                            binding.requirement_name
                        ))
                    })?;
                if requirement.api_id() != binding.api_id {
                    return Err(TopologyApiBindingError::Binding(format!(
                        "binding {} API {} does not match consumer requirement {}",
                        binding.requirement_name,
                        binding.api_id,
                        requirement.api_id()
                    )));
                }
                if !binding.provider_deployment_id.is_empty()
                    && binding.provider_deployment_id != target.instance.deployment_id
                {
                    return Err(TopologyApiBindingError::Binding(format!(
                        "binding {} selects provider {}, but Link targets deployment {}",
                        binding.requirement_name,
                        binding.provider_deployment_id,
                        target.instance.deployment_id
                    )));
                }
                let version_requirement = if binding.version.trim().is_empty() {
                    requirement.version_requirement()
                } else {
                    binding.version.as_str()
                };
                let provided = target_contract.release.apis.iter().find(|api| {
                    api.api_id == binding.api_id
                        && api_version_matches(version_requirement, &api.version)
                        && api.protocol == link.protocol
                });
                let Some(provided) = provided else {
                    return Err(TopologyApiBindingError::Binding(format!(
                        "provider release {}@{} does not provide {} matching version {} and protocol {}",
                        target.instance.service_id,
                        target.instance.release_version,
                        binding.api_id,
                        version_requirement,
                        link.protocol
                    )));
                };
                if !matches!(provided.auth_mode.as_str(), "workload" | "public") {
                    return Err(TopologyApiBindingError::Binding(format!(
                        "provider {} API {} uses unsupported {} auth; workload consumers cannot delegate end-user identity",
                        target.instance.deployment_id, binding.api_id, provided.auth_mode
                    )));
                }
            }
        }
        Ok(())
    }

    /// Resolves one immutable TopologySpec to the exact durable binding rows
    /// that will be staged by the apply saga. Provider selection is entirely
    /// explicit in the Link: this function never searches for or silently
    /// chooses a different deployment.
    pub(crate) fn resolve_topology_api_bindings(
        &self,
        spec: &TopologySpec,
        revision_id: &str,
        operation_id: &str,
    ) -> Result<Vec<ApiBinding>, TopologyApiBindingError> {
        self.validate_topology_api_bindings(spec)?;
        let runtime_instances = self
            .runtime_instances(None)
            .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?;
        let releases = match self {
            Self::Sqlite(store) => store.list_service_releases(),
            Self::Postgres(store) => store.list_service_releases(),
        }
        .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?;
        let mut contracts = BTreeMap::new();
        for release in releases {
            let contract = ServiceReleaseContract::from_json_value(release.manifest.clone())
                .map_err(|error| TopologyApiBindingError::Contract(error.to_string()))?;
            contracts.insert((release.service_name, release.version), contract);
        }
        let endpoint_specs = spec
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
            .collect::<BTreeMap<_, _>>();
        let now = durable_now_marker();
        let mut desired = Vec::new();
        for link in spec
            .links
            .iter()
            .filter(|link| link.enabled && !link.api_bindings.is_empty())
        {
            let source = exact_runtime_for_topology_endpoint(
                &runtime_instances,
                endpoint_specs[link.source_endpoint.as_str()],
            )?;
            let target = exact_runtime_for_topology_endpoint(
                &runtime_instances,
                endpoint_specs[link.target_endpoint.as_str()],
            )?;
            ensure_running_healthy_runtime(self, source, "consumer")?;
            ensure_running_healthy_runtime(self, target, "provider")?;
            let source_contract = contracts
                .get(&(
                    source.instance.service_id.clone(),
                    source.instance.release_version.clone(),
                ))
                .ok_or_else(|| {
                    TopologyApiBindingError::Contract(format!(
                        "consumer deployment {} has no exact release contract",
                        source.instance.deployment_id
                    ))
                })?;
            let target_contract = contracts
                .get(&(
                    target.instance.service_id.clone(),
                    target.instance.release_version.clone(),
                ))
                .ok_or_else(|| {
                    TopologyApiBindingError::Contract(format!(
                        "provider deployment {} has no exact release contract",
                        target.instance.deployment_id
                    ))
                })?;
            for selection in &link.api_bindings {
                let requirement = source_contract
                    .requirements()
                    .iter()
                    .find(|requirement| requirement.binding_name() == selection.requirement_name)
                    .ok_or_else(|| {
                        TopologyApiBindingError::Binding(format!(
                            "consumer {} does not declare requirement {}",
                            source.instance.deployment_id, selection.requirement_name
                        ))
                    })?;
                let version_requirement = if selection.version.trim().is_empty() {
                    requirement.version_requirement()
                } else {
                    selection.version.as_str()
                };
                let provider_api = target_contract
                    .release
                    .apis
                    .iter()
                    .find(|api| {
                        api.api_id == selection.api_id
                            && api.protocol == link.protocol
                            && api_version_matches(version_requirement, &api.version)
                    })
                    .ok_or_else(|| {
                        TopologyApiBindingError::Binding(format!(
                            "provider {} does not expose {} matching {}",
                            target.instance.deployment_id, selection.api_id, version_requirement
                        ))
                    })?;
                if !matches!(provider_api.auth_mode.as_str(), "workload" | "public") {
                    return Err(TopologyApiBindingError::Binding(format!(
                        "provider {} API {} uses unsupported {} auth; workload consumers cannot delegate end-user identity",
                        target.instance.deployment_id, selection.api_id, provider_api.auth_mode
                    )));
                }
                desired.push(ApiBinding {
                    binding_id: topology_binding_id(
                        &source.instance.deployment_id,
                        &selection.requirement_name,
                    ),
                    requirement_name: selection.requirement_name.clone(),
                    api_id: selection.api_id.clone(),
                    api_version: provider_api.version.clone(),
                    consumer_deployment_id: source.instance.deployment_id.clone(),
                    consumer_service_id: source.instance.service_id.clone(),
                    consumer_node_id: source.node_id.clone(),
                    consumer_endpoint: source.endpoint.clone(),
                    provider_deployment_id: target.instance.deployment_id.clone(),
                    provider_service_id: target.instance.service_id.clone(),
                    provider_node_id: target.node_id.clone(),
                    provider_endpoint: target.endpoint.clone(),
                    provider_path: provider_api.path_prefix.clone(),
                    virtual_endpoint: format!("/internal/apis/{}", selection.api_id),
                    protocol: provider_api.protocol.clone(),
                    methods: provider_api.methods.clone(),
                    auth_mode: "workload".to_string(),
                    provider_auth_mode: provider_api.auth_mode.clone(),
                    permission: provider_api.permission.clone(),
                    timeout_ms: requirement.timeout_ms().or(Some(30_000)),
                    topology_id: spec.topology_id.clone(),
                    topology_revision_id: revision_id.to_string(),
                    link_source_endpoint: link.source_endpoint.clone(),
                    link_target_endpoint: link.target_endpoint.clone(),
                    credential_ref: String::new(),
                    credential_generation: 1,
                    context_generation: 1,
                    desired_state: "ACTIVE".to_string(),
                    observed_state: "PENDING".to_string(),
                    health: "UNKNOWN".to_string(),
                    drift: Vec::new(),
                    last_operation_id: operation_id.to_string(),
                    state: ApiBindingState::Pending,
                    optional: requirement.optional(),
                    reason: String::new(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                });
            }
        }
        desired.sort_by(|left, right| {
            (&left.consumer_deployment_id, &left.requirement_name)
                .cmp(&(&right.consumer_deployment_id, &right.requirement_name))
        });
        let current = self
            .api_bindings_for_topology(&spec.topology_id)
            .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?;
        Ok(stage_binding_generations(
            desired,
            current,
            revision_id,
            operation_id,
            &now,
        ))
    }

    pub(crate) fn stage_precomputed_topology_api_bindings(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        mut bindings: Vec<ApiBinding>,
    ) -> Result<Vec<ApiBinding>, TopologyApiBindingError> {
        let now = durable_now_marker();
        for binding in &mut bindings {
            if binding.state == ApiBindingState::Unbound && binding.optional {
                continue;
            }
            if !matches!(
                binding.state,
                ApiBindingState::Resolved | ApiBindingState::Active
            ) {
                return Err(TopologyApiBindingError::Binding(format!(
                    "precomputed binding {} is not resolved",
                    binding.binding_id
                )));
            }
            binding.topology_id = topology_id.to_string();
            binding.topology_revision_id = revision_id.to_string();
            binding.last_operation_id = operation_id.to_string();
            binding.desired_state = "ACTIVE".to_string();
            binding.observed_state = "PENDING".to_string();
            binding.health = "UNKNOWN".to_string();
            binding.state = ApiBindingState::Pending;
            binding.updated_at = now.clone();
        }
        bindings.retain(|binding| binding.state != ApiBindingState::Unbound);
        let current = self
            .api_bindings_for_topology(topology_id)
            .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?;
        Ok(stage_binding_generations(
            bindings,
            current,
            revision_id,
            operation_id,
            &now,
        ))
    }

    pub(crate) fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store.get_node(node_id),
            Self::Postgres(store) => store.get_node(node_id),
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn list_nodes(&self) -> Result<Vec<NodeRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store.list_nodes(),
            Self::Postgres(store) => store.list_nodes(),
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn upsert_node(&self, node: NodeRecord) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => {
                let mut store = store.clone();
                store.upsert_node(node)
            }
            Self::Postgres(store) => {
                let mut store = store.clone();
                store.upsert_node(node)
            }
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn delete_node(&self, node_id: &str) -> Result<(), DurableError> {
        // Removal is also an identity boundary. Revocation is committed before
        // the node record disappears so an already-open certificate cannot be
        // used to claim work after administrative removal.
        self.revoke_node_certificates(node_id, current_time_ms(), "node removed")?;
        match self {
            Self::Sqlite(store) => {
                let mut store = store.clone();
                store.delete_node(node_id)
            }
            Self::Postgres(store) => {
                let mut store = store.clone();
                store.delete_node(node_id)
            }
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn register_node_enrollment(
        &self,
        node: &NodeRecord,
        code: &NodeEnrollmentCode,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .register_node_enrollment(node, code)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .register_node_enrollment(node, code)
                .map_err(Into::into),
        }
    }

    #[cfg(test)]
    pub(crate) fn node_enrollment_code_by_digest(
        &self,
        digest: &str,
    ) -> Result<Option<NodeEnrollmentCode>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .node_enrollment_code_by_digest(digest)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .node_enrollment_code_by_digest(digest)
                .map_err(Into::into),
        }
    }

    pub(crate) fn lookup_node_enrollment(
        &self,
        digest: &str,
        csr_sha256: &str,
    ) -> Result<EnrollmentLookup, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .lookup_node_enrollment(digest, csr_sha256)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .lookup_node_enrollment(digest, csr_sha256)
                .map_err(Into::into),
        }
    }

    pub(crate) fn redeem_node_enrollment_code(
        &self,
        digest: &str,
        csr_sha256: &str,
        now_ms: i64,
        certificate: NewNodeCertificate,
    ) -> Result<EnrollmentRedemption, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .redeem_node_enrollment_code(digest, csr_sha256, now_ms, certificate)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .redeem_node_enrollment_code(digest, csr_sha256, now_ms, certificate)
                .map_err(Into::into),
        }
    }

    pub(crate) fn node_certificate(
        &self,
        serial_hex: &str,
    ) -> Result<Option<NodeCertificateRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store.node_certificate(serial_hex).map_err(Into::into),
            Self::Postgres(store) => store.node_certificate(serial_hex).map_err(Into::into),
        }
    }

    pub(crate) fn rotate_node_certificate(
        &self,
        current_serial: &str,
        node_id: &str,
        now_ms: i64,
        replacement: NewNodeCertificate,
    ) -> Result<CertificateRotation, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .rotate_node_certificate(current_serial, node_id, now_ms, replacement)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .rotate_node_certificate(current_serial, node_id, now_ms, replacement)
                .map_err(Into::into),
        }
    }

    pub(crate) fn revoke_node_certificates(
        &self,
        node_id: &str,
        now_ms: i64,
        reason: &str,
    ) -> Result<u64, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .revoke_node_certificates(node_id, now_ms, reason)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .revoke_node_certificates(node_id, now_ms, reason)
                .map_err(Into::into),
        }
    }

    pub(crate) fn activate_node_certificate(
        &self,
        node_id: &str,
        current_serial: &str,
        now_ms: i64,
    ) -> Result<CertificateActivation, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .activate_node_certificate(node_id, current_serial, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .activate_node_certificate(node_id, current_serial, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn create_initial_topology_revision(
        &self,
        spec: TopologySpec,
        created_at: String,
        created_by: String,
        message: String,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .create_initial_topology_revision(spec, created_at, created_by, message)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .create_initial_topology_revision(spec, created_at, created_by, message)
                .map_err(Into::into),
        }
    }

    pub(crate) fn create_next_topology_revision(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        spec: TopologySpec,
        created_at: String,
        created_by: String,
        message: String,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .create_next_topology_revision(
                    topology_id,
                    expected_draft_revision_id,
                    spec,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .create_next_topology_revision(
                    topology_id,
                    expected_draft_revision_id,
                    spec,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn create_topology_rollback_revision(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        rollback_to_revision_id: &str,
        created_at: String,
        created_by: String,
        message: String,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .create_topology_rollback_revision(
                    topology_id,
                    expected_draft_revision_id,
                    rollback_to_revision_id,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .create_topology_rollback_revision(
                    topology_id,
                    expected_draft_revision_id,
                    rollback_to_revision_id,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn begin_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .begin_topology_apply(topology_id, revision_id, operation_id, updated_at)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .begin_topology_apply(topology_id, revision_id, operation_id, updated_at)
                .map_err(Into::into),
        }
    }

    pub(crate) fn finish_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        outcome: TopologyApplyOutcome,
        updated_at: &str,
    ) -> Result<TopologyHeads, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .finish_topology_apply(topology_id, revision_id, operation_id, outcome, updated_at)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .finish_topology_apply(topology_id, revision_id, operation_id, outcome, updated_at)
                .map_err(Into::into),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_topology_apply_fenced(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        outcome: TopologyApplyOutcome,
        updated_at: &str,
        job_id: &str,
        lease_token: &str,
        now_ms: i64,
    ) -> Result<TopologyHeads, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .finish_topology_apply_fenced(
                    topology_id,
                    revision_id,
                    operation_id,
                    outcome,
                    updated_at,
                    job_id,
                    lease_token,
                    now_ms,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .finish_topology_apply_fenced(
                    topology_id,
                    revision_id,
                    operation_id,
                    outcome,
                    updated_at,
                    job_id,
                    lease_token,
                    now_ms,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn finish_topology_apply_group_fenced(
        &self,
        members: &[orchestrator_storage::TopologyApplyGroupMember],
        operation_id: &str,
        updated_at: &str,
        job_id: &str,
        lease_token: &str,
        now_ms: i64,
    ) -> Result<Vec<TopologyHeads>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .finish_topology_apply_group_fenced(
                    members,
                    operation_id,
                    updated_at,
                    job_id,
                    lease_token,
                    now_ms,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .finish_topology_apply_group_fenced(
                    members,
                    operation_id,
                    updated_at,
                    job_id,
                    lease_token,
                    now_ms,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn resolve_expired_topology_apply_group_success(
        &self,
        members: &[orchestrator_storage::TopologyApplyGroupMember],
        operation_id: &str,
        job_id: &str,
        now_ms: i64,
        result: Value,
    ) -> Result<Option<Job>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .resolve_expired_topology_apply_group_success(
                    members,
                    operation_id,
                    job_id,
                    now_ms,
                    result,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .resolve_expired_topology_apply_group_success(
                    members,
                    operation_id,
                    job_id,
                    now_ms,
                    result,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn compensate_completed_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        previous_revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> Result<TopologyHeads, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .compensate_completed_topology_apply(
                    topology_id,
                    revision_id,
                    previous_revision_id,
                    operation_id,
                    updated_at,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .compensate_completed_topology_apply(
                    topology_id,
                    revision_id,
                    previous_revision_id,
                    operation_id,
                    updated_at,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn topology_heads(
        &self,
        topology_id: &str,
    ) -> Result<Option<TopologyHeads>, DurableError> {
        match self {
            Self::Sqlite(store) => store.topology_heads(topology_id).map_err(Into::into),
            Self::Postgres(store) => store.topology_heads(topology_id).map_err(Into::into),
        }
    }

    pub(crate) fn list_topology_heads(&self) -> Result<Vec<TopologyHeads>, DurableError> {
        match self {
            Self::Sqlite(store) => store.list_topology_heads().map_err(Into::into),
            Self::Postgres(store) => store.list_topology_heads().map_err(Into::into),
        }
    }

    pub(crate) fn topology_revision(
        &self,
        topology_id: &str,
        revision_id: &str,
    ) -> Result<Option<TopologyRevision>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .topology_revision(topology_id, revision_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .topology_revision(topology_id, revision_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn topology_revisions(
        &self,
        topology_id: &str,
    ) -> Result<Vec<TopologyRevision>, DurableError> {
        match self {
            Self::Sqlite(store) => store.topology_revisions(topology_id).map_err(Into::into),
            Self::Postgres(store) => store.topology_revisions(topology_id).map_err(Into::into),
        }
    }

    pub(crate) fn topology_status(
        &self,
        topology_id: &str,
    ) -> Result<Option<TopologyStatus>, DurableError> {
        match self {
            Self::Sqlite(store) => store.topology_status(topology_id).map_err(Into::into),
            Self::Postgres(store) => store.topology_status(topology_id).map_err(Into::into),
        }
    }

    pub(crate) fn put_topology_status(&self, status: &TopologyStatus) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store.put_topology_status(status).map_err(Into::into),
            Self::Postgres(store) => store.put_topology_status(status).map_err(Into::into),
        }
    }

    pub(crate) fn put_reconciled_topology_status(
        &self,
        status: &TopologyStatus,
        expected_applied_revision_id: &str,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .put_reconciled_topology_status(status, expected_applied_revision_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .put_reconciled_topology_status(status, expected_applied_revision_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn runtime_instance(
        &self,
        deployment_id: &str,
    ) -> Result<Option<StoredRuntimeInstance>, DurableError> {
        match self {
            Self::Sqlite(store) => store.runtime_instance(deployment_id).map_err(Into::into),
            Self::Postgres(store) => store.runtime_instance(deployment_id).map_err(Into::into),
        }
    }

    pub(crate) fn put_runtime_instance(
        &self,
        value: &StoredRuntimeInstance,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store.put_runtime_instance(value).map_err(Into::into),
            Self::Postgres(store) => store.put_runtime_instance(value).map_err(Into::into),
        }
    }

    pub(crate) fn delete_runtime_instance(
        &self,
        deployment_id: &str,
    ) -> Result<bool, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .delete_runtime_instance(deployment_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .delete_runtime_instance(deployment_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn replace_runtime_instance(
        &self,
        replaced_deployment_id: &str,
        value: &StoredRuntimeInstance,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .replace_runtime_instance(replaced_deployment_id, value)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .replace_runtime_instance(replaced_deployment_id, value)
                .map_err(Into::into),
        }
    }

    pub(crate) fn runtime_instances(
        &self,
        node_id: Option<&str>,
    ) -> Result<Vec<StoredRuntimeInstance>, DurableError> {
        match self {
            Self::Sqlite(store) => store.runtime_instances(node_id).map_err(Into::into),
            Self::Postgres(store) => store.runtime_instances(node_id).map_err(Into::into),
        }
    }

    #[cfg(test)]
    pub(crate) fn put_node_runtime_facts(
        &self,
        value: &StoredNodeRuntimeFacts,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store.put_node_runtime_facts(value).map_err(Into::into),
            Self::Postgres(store) => store.put_node_runtime_facts(value).map_err(Into::into),
        }
    }

    pub(crate) fn apply_node_runtime_report(
        &self,
        value: &StoredNodeRuntimeFacts,
        expected_managed_deployment_ids: Option<&[String]>,
        runtime_instances: &[(StoredRuntimeInstance, StoredRuntimeInstance)],
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .apply_node_runtime_report(
                    value,
                    expected_managed_deployment_ids,
                    runtime_instances,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .apply_node_runtime_report(
                    value,
                    expected_managed_deployment_ids,
                    runtime_instances,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn node_runtime_facts(
        &self,
        node_id: &str,
    ) -> Result<Option<StoredNodeRuntimeFacts>, DurableError> {
        match self {
            Self::Sqlite(store) => store.node_runtime_facts(node_id).map_err(Into::into),
            Self::Postgres(store) => store.node_runtime_facts(node_id).map_err(Into::into),
        }
    }

    pub(crate) fn managed_runtime_report_unavailable_reason(
        &self,
        runtime: &StoredRuntimeInstance,
        at_ms: i64,
    ) -> Result<Option<String>, DurableError> {
        if runtime.management_mode != orchestrator_storage::RuntimeManagementMode::Managed {
            return Ok(None);
        }
        let Some(facts) = self.node_runtime_facts(&runtime.node_id)? else {
            return Ok(Some(
                "managed deployment has no authenticated Node runtime report".to_string(),
            ));
        };
        if facts.is_stale_at(at_ms, MANAGED_RUNTIME_REPORT_STALE_MS) {
            return Ok(Some(format!(
                "authenticated Node runtime report is older than {} seconds",
                MANAGED_RUNTIME_REPORT_STALE_MS / 1_000
            )));
        }
        if facts
            .facts
            .get("inventory_complete")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Ok(Some(
                "latest authenticated Node runtime report has an incomplete Docker inventory"
                    .to_string(),
            ));
        }
        if runtime.last_observed_at_ms <= 0 || runtime.last_observed_at_ms < facts.observed_at_ms {
            return Ok(Some(
                "deployment has no observation from the latest complete Node runtime report"
                    .to_string(),
            ));
        }
        Ok(None)
    }

    pub(crate) fn runtime_with_current_evidence(
        &self,
        mut runtime: StoredRuntimeInstance,
        at_ms: i64,
    ) -> Result<StoredRuntimeInstance, DurableError> {
        if let Some(reason) = self.managed_runtime_report_unavailable_reason(&runtime, at_ms)? {
            runtime.instance.observed_state = orchestrator_runtime::RuntimeObservedState::Unknown;
            runtime.instance.health = "UNKNOWN".to_string();
            runtime.instance.runtime_attested = false;
            runtime.drift_reason = merge_runtime_evidence(&runtime.drift_reason, &reason);
        }
        if runtime.management_mode == orchestrator_storage::RuntimeManagementMode::External {
            let unavailable = if runtime.external_probe_protocol.trim().is_empty() {
                Some("external deployment has no persisted health probe contract".to_string())
            } else if runtime.last_observed_at_ms <= 0
                || at_ms.saturating_sub(runtime.last_observed_at_ms)
                    > EXTERNAL_RUNTIME_PROBE_STALE_MS
            {
                Some(format!(
                    "external health probe evidence is older than {} seconds",
                    EXTERNAL_RUNTIME_PROBE_STALE_MS / 1_000
                ))
            } else {
                None
            };
            if let Some(reason) = unavailable {
                runtime.instance.observed_state =
                    orchestrator_runtime::RuntimeObservedState::Unknown;
                runtime.instance.health = "UNKNOWN".to_string();
                runtime.drift_reason = merge_runtime_evidence(&runtime.drift_reason, &reason);
            }
        }
        Ok(runtime)
    }

    pub(crate) fn binding_with_current_runtime_evidence(
        &self,
        mut binding: ApiBinding,
        at_ms: i64,
    ) -> Result<ApiBinding, DurableError> {
        if binding.desired_state != "ACTIVE"
            || !matches!(
                binding.state,
                ApiBindingState::Resolved | ApiBindingState::Active
            )
        {
            return Ok(binding);
        }
        let mut unavailable = Vec::new();
        for (role, deployment_id) in [
            ("consumer", binding.consumer_deployment_id.as_str()),
            ("provider", binding.provider_deployment_id.as_str()),
        ] {
            let Some(runtime) = self.runtime_instance(deployment_id)? else {
                unavailable.push(format!("{role} deployment {deployment_id} is missing"));
                continue;
            };
            let runtime = self.runtime_with_current_evidence(runtime, at_ms)?;
            let (expected_service_id, expected_node_id) = if role == "consumer" {
                (
                    binding.consumer_service_id.as_str(),
                    binding.consumer_node_id.as_str(),
                )
            } else {
                (
                    binding.provider_service_id.as_str(),
                    binding.provider_node_id.as_str(),
                )
            };
            if runtime.instance.service_id != expected_service_id
                || runtime.node_id != expected_node_id
            {
                unavailable.push(format!(
                    "{role} deployment {deployment_id} assignment changed from service {expected_service_id} on Node {expected_node_id} to service {} on Node {}",
                    runtime.instance.service_id, runtime.node_id
                ));
                continue;
            }
            if runtime.instance.desired_state != orchestrator_runtime::RuntimeDesiredState::Running
                || runtime.instance.observed_state
                    != orchestrator_runtime::RuntimeObservedState::Running
                || !runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
                || !runtime.drift_reason.trim().is_empty()
                || (runtime.management_mode == orchestrator_storage::RuntimeManagementMode::Managed
                    && !runtime.instance.runtime_attested)
            {
                let detail = if runtime.drift_reason.trim().is_empty() {
                    "runtime is not desired Running, observed Running/Healthy".to_string()
                } else {
                    runtime.drift_reason
                };
                unavailable.push(format!(
                    "{role} deployment {deployment_id} is unavailable: {detail}"
                ));
            }
        }
        if !unavailable.is_empty() {
            binding.observed_state = "ERROR".to_string();
            binding.health = "DEGRADED".to_string();
            for reason in unavailable {
                let reason = bounded_runtime_evidence(&reason);
                if !binding.drift.contains(&reason) {
                    binding.drift.push(reason);
                }
            }
            binding.reason = bounded_runtime_evidence(&binding.drift.join("; "));
        }
        Ok(binding)
    }

    pub(crate) fn api_bindings_for_deployment(
        &self,
        deployment_id: &str,
    ) -> Result<Vec<ApiBinding>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .api_bindings_for_deployment(deployment_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .api_bindings_for_deployment(deployment_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn api_bindings_for_topology(
        &self,
        topology_id: &str,
    ) -> Result<Vec<ApiBinding>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .api_bindings_for_topology(topology_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .api_bindings_for_topology(topology_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn replace_deployment_api_bindings(
        &self,
        deployment_id: &str,
        bindings: &[ApiBinding],
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .replace_deployment_api_bindings(deployment_id, bindings)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .replace_deployment_api_bindings(deployment_id, bindings)
                .map_err(Into::into),
        }
    }

    pub(crate) fn replace_topology_api_bindings(
        &self,
        topology_id: &str,
        bindings: &[ApiBinding],
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .replace_topology_api_bindings(topology_id, bindings)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .replace_topology_api_bindings(topology_id, bindings)
                .map_err(Into::into),
        }
    }

    pub(crate) fn purge_terminal_history(
        &self,
        completed_before_ms: i64,
        now_ms: i64,
    ) -> Result<HistoryRetentionReport, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .purge_terminal_history(completed_before_ms, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .purge_terminal_history(completed_before_ms, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn put_state<T: Serialize>(
        &self,
        namespace: &str,
        key: &str,
        value: &T,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store.put_state(namespace, key, value).map_err(Into::into),
            Self::Postgres(store) => store.put_state(namespace, key, value).map_err(Into::into),
        }
    }

    pub(crate) fn get_state<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<T>, DurableError> {
        match self {
            Self::Sqlite(store) => store.get_state(namespace, key).map_err(Into::into),
            Self::Postgres(store) => store.get_state(namespace, key).map_err(Into::into),
        }
    }

    pub(crate) fn delete_state(&self, namespace: &str, key: &str) -> Result<bool, DurableError> {
        match self {
            Self::Sqlite(store) => store.delete_state(namespace, key).map_err(Into::into),
            Self::Postgres(store) => store.delete_state(namespace, key).map_err(Into::into),
        }
    }
}

fn merge_runtime_evidence(existing: &str, reason: &str) -> String {
    if existing.trim().is_empty() {
        bounded_runtime_evidence(reason)
    } else if existing.contains(reason) {
        bounded_runtime_evidence(existing)
    } else {
        bounded_runtime_evidence(&format!("{existing}; {reason}"))
    }
}

fn bounded_runtime_evidence(value: &str) -> String {
    const MAX_BYTES: usize = 512;
    let mut bounded = String::new();
    for character in value.chars() {
        let printable = if character.is_control() {
            ' '
        } else {
            character
        };
        if bounded.len() + printable.len_utf8() > MAX_BYTES {
            break;
        }
        bounded.push(printable);
    }
    bounded.trim().to_string()
}

fn current_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[derive(Debug)]
pub(crate) enum DurableJobStore {
    Sqlite(SqliteJobStore),
    Postgres(PostgresJobStore),
}

impl DurableJobStore {
    pub(crate) fn active_job_count(&self, node_id: &str) -> Result<u64, JobError> {
        match self {
            Self::Sqlite(store) => store.active_job_count(node_id),
            Self::Postgres(store) => store.active_job_count(node_id),
        }
    }

    pub(crate) fn metrics_snapshot(&self, now_ms: i64) -> Result<JobMetricsSnapshot, JobError> {
        match self {
            Self::Sqlite(store) => store.metrics_snapshot(now_ms),
            Self::Postgres(store) => store.metrics_snapshot(now_ms),
        }
    }
}

impl JobStore for DurableJobStore {
    fn enqueue(&mut self, job: NewJob, now_ms: i64) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.enqueue(job, now_ms),
            Self::Postgres(store) => store.enqueue(job, now_ms),
        }
    }

    fn claim(&mut self, request: ClaimRequest) -> Result<Option<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.claim(request),
            Self::Postgres(store) => store.claim(request),
        }
    }

    fn heartbeat(&mut self, request: HeartbeatRequest) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.heartbeat(request),
            Self::Postgres(store) => store.heartbeat(request),
        }
    }

    fn complete(&mut self, request: CompleteRequest) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.complete(request),
            Self::Postgres(store) => store.complete(request),
        }
    }

    fn resolve_expired_success(
        &mut self,
        request: ResolveExpiredSuccessRequest,
    ) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.resolve_expired_success(request),
            Self::Postgres(store) => store.resolve_expired_success(request),
        }
    }

    fn request_cancel(&mut self, job_id: &str, now_ms: i64) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.request_cancel(job_id, now_ms),
            Self::Postgres(store) => store.request_cancel(job_id, now_ms),
        }
    }

    fn expired_leases(&self, now_ms: i64) -> Result<Vec<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.expired_leases(now_ms),
            Self::Postgres(store) => store.expired_leases(now_ms),
        }
    }

    fn recover_expired(&mut self, now_ms: i64) -> Result<Vec<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.recover_expired(now_ms),
            Self::Postgres(store) => store.recover_expired(now_ms),
        }
    }

    fn get(&self, job_id: &str) -> Result<Option<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.get(job_id),
            Self::Postgres(store) => store.get(job_id),
        }
    }

    fn list(&self) -> Result<Vec<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.list(),
            Self::Postgres(store) => store.list(),
        }
    }

    fn events(&self, job_id: &str, after_sequence: u64) -> Result<Vec<JobEvent>, JobError> {
        match self {
            Self::Sqlite(store) => store.events(job_id, after_sequence),
            Self::Postgres(store) => store.events(job_id, after_sequence),
        }
    }
}

#[derive(Debug)]
pub(crate) enum DurableOperationStore {
    Sqlite(SqliteOperationStore),
    Postgres(PostgresOperationStore),
}

impl DurableOperationStore {
    pub(crate) fn anomaly_candidates(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.anomaly_candidates(),
            Self::Postgres(store) => store.anomaly_candidates(),
        }
    }

    pub(crate) fn observe_active_operation_anomalies(
        &self,
        candidates: &[DurableOperation],
        now_ms: i64,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.observe_active_operation_anomalies(candidates, now_ms),
            Self::Postgres(store) => store.observe_active_operation_anomalies(candidates, now_ms),
        }
    }

    pub(crate) fn migrate_legacy_anomaly_state(
        &self,
        expired_leases: u64,
        long_operations: u64,
        expired_lease_episodes: &std::collections::BTreeMap<String, String>,
        long_operation_episodes: &std::collections::BTreeSet<String>,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.migrate_legacy_anomaly_state(
                expired_leases,
                long_operations,
                expired_lease_episodes,
                long_operation_episodes,
            ),
            Self::Postgres(store) => store.migrate_legacy_anomaly_state(
                expired_leases,
                long_operations,
                expired_lease_episodes,
                long_operation_episodes,
            ),
        }
    }
}

impl OperationRepository for DurableOperationStore {
    fn create(
        &mut self,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.create(operation),
            Self::Postgres(store) => store.create(operation),
        }
    }

    fn get(&self, operation_id: &str) -> Result<Option<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.get(operation_id),
            Self::Postgres(store) => store.get(operation_id),
        }
    }

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.compare_and_swap(expected_revision, operation),
            Self::Postgres(store) => store.compare_and_swap(expected_revision, operation),
        }
    }

    fn recoverable(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.recoverable(),
            Self::Postgres(store) => store.recoverable(),
        }
    }

    fn list(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.list(),
            Self::Postgres(store) => store.list(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum DurableError {
    #[error("optimistic concurrency conflict: {0}")]
    Conflict(String),
    #[error("storage invariant failed: {0}")]
    Invariant(String),
    #[error("domain validation failed: {0}")]
    Domain(String),
    #[error("durable storage error: {0}")]
    Storage(String),
}

#[derive(Debug, Error)]
pub(crate) enum LinkProbeBindingError {
    #[error("{0}")]
    Binding(String),
    #[error("{0}")]
    Capability(String),
    #[error("durable storage error: {0}")]
    Storage(String),
}

#[derive(Debug, Error)]
pub(crate) enum TopologyApiBindingError {
    #[error("{0}")]
    Binding(String),
    #[error("{0}")]
    Contract(String),
    #[error("durable storage error: {0}")]
    Storage(String),
}

fn exact_runtime_for_endpoint<'a>(
    instances: &'a [StoredRuntimeInstance],
    endpoint: &str,
    service_id: &str,
) -> Result<&'a StoredRuntimeInstance, TopologyApiBindingError> {
    let matching = instances
        .iter()
        .filter(|instance| {
            instance.endpoint == endpoint && instance.instance.service_id == service_id
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [instance] => Ok(*instance),
        _ => Err(TopologyApiBindingError::Binding(format!(
            "endpoint {endpoint} for service {service_id} requires exactly one runtime projection; found {}",
            matching.len()
        ))),
    }
}

fn exact_runtime_for_topology_endpoint<'a>(
    instances: &'a [StoredRuntimeInstance],
    endpoint: &TopologyEndpointSpec,
) -> Result<&'a StoredRuntimeInstance, TopologyApiBindingError> {
    let configured_deployment = endpoint
        .config
        .as_object()
        .and_then(|config| config.get("deployment_id"))
        .and_then(Value::as_str)
        .filter(|deployment_id| !deployment_id.trim().is_empty());
    if let Some(deployment_id) = configured_deployment {
        let matching = instances
            .iter()
            .filter(|instance| {
                instance.instance.deployment_id == deployment_id
                    && instance.instance.service_id == endpoint.service_id
            })
            .collect::<Vec<_>>();
        return match matching.as_slice() {
            [instance] => Ok(*instance),
            _ => Err(TopologyApiBindingError::Binding(format!(
                "endpoint {} selects deployment {deployment_id}, but found {} exact runtime projections",
                endpoint.endpoint,
                matching.len()
            ))),
        };
    }
    exact_runtime_for_endpoint(instances, &endpoint.endpoint, &endpoint.service_id)
}

fn ensure_running_healthy_runtime(
    storage: &DurableStore,
    runtime: &StoredRuntimeInstance,
    role: &str,
) -> Result<(), TopologyApiBindingError> {
    let runtime = storage
        .runtime_with_current_evidence(runtime.clone(), current_time_ms())
        .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?;
    if runtime.instance.desired_state != orchestrator_runtime::RuntimeDesiredState::Running
        || runtime.instance.observed_state != orchestrator_runtime::RuntimeObservedState::Running
        || !runtime.instance.health.eq_ignore_ascii_case("HEALTHY")
        || !runtime.drift_reason.is_empty()
        || (runtime.management_mode == orchestrator_storage::RuntimeManagementMode::Managed
            && !runtime.instance.runtime_attested)
    {
        let evidence_detail = if runtime.drift_reason.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", runtime.drift_reason)
        };
        return Err(TopologyApiBindingError::Binding(format!(
            "{role} deployment {} is not desired Running, observed Running/Healthy, and runtime-attested without drift{evidence_detail}",
            runtime.instance.deployment_id,
        )));
    }
    if runtime.management_mode == orchestrator_storage::RuntimeManagementMode::Managed
        && let Some(reason) = storage
            .managed_runtime_report_unavailable_reason(&runtime, current_time_ms())
            .map_err(|error| TopologyApiBindingError::Storage(error.to_string()))?
    {
        return Err(TopologyApiBindingError::Binding(format!(
            "{role} deployment {} has unavailable runtime evidence: {reason}",
            runtime.instance.deployment_id,
        )));
    }
    Ok(())
}

fn topology_binding_id(deployment_id: &str, requirement_name: &str) -> String {
    let digest = Sha256::digest(format!("{deployment_id}\0{requirement_name}").as_bytes());
    format!("binding-{digest:x}")
}

fn durable_now_marker() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("unix-ms:{millis}")
}

/// A workload JWT carries one deployment-wide credential generation. If any
/// route for a consumer changes, every still-active sibling is therefore
/// staged with the same next generation. This makes old tokens fail every API
/// immediately after the Gateway atomically switches the route table.
fn stage_binding_generations(
    mut desired: Vec<ApiBinding>,
    current: Vec<ApiBinding>,
    revision_id: &str,
    operation_id: &str,
    now: &str,
) -> Vec<ApiBinding> {
    let mut consumers = current
        .iter()
        .map(|binding| binding.consumer_deployment_id.clone())
        .chain(
            desired
                .iter()
                .map(|binding| binding.consumer_deployment_id.clone()),
        )
        .collect::<BTreeSet<_>>();
    let desired_keys = desired
        .iter()
        .map(|binding| {
            (
                binding.consumer_deployment_id.clone(),
                binding.requirement_name.clone(),
            )
        })
        .collect::<BTreeSet<_>>();

    for consumer in std::mem::take(&mut consumers) {
        let mut wanted = desired
            .iter()
            .filter(|binding| binding.consumer_deployment_id == consumer)
            .collect::<Vec<_>>();
        let mut active = current
            .iter()
            .filter(|binding| {
                binding.consumer_deployment_id == consumer
                    && binding.desired_state == "ACTIVE"
                    && binding.state == ApiBindingState::Active
            })
            .collect::<Vec<_>>();
        wanted.sort_by_key(|binding| binding.requirement_name.as_str());
        active.sort_by_key(|binding| binding.requirement_name.as_str());
        let changed = wanted.len() != active.len()
            || wanted
                .iter()
                .zip(active.iter())
                .any(|(wanted, active)| !same_binding_route(wanted, active));
        let previous_generation = current
            .iter()
            .filter(|binding| binding.consumer_deployment_id == consumer)
            .map(|binding| {
                binding
                    .credential_generation
                    .max(binding.context_generation)
            })
            .max()
            .unwrap_or(0);
        let generation = if changed {
            previous_generation.saturating_add(1).max(1)
        } else {
            previous_generation.max(1)
        };
        for binding in desired
            .iter_mut()
            .filter(|binding| binding.consumer_deployment_id == consumer)
        {
            binding.credential_generation = generation;
            binding.context_generation = generation;
            if let Some(existing) = current
                .iter()
                .find(|existing| existing.binding_id == binding.binding_id)
            {
                binding.created_at = existing.created_at.clone();
            }
        }
        for existing in current.iter().filter(|binding| {
            binding.consumer_deployment_id == consumer
                && binding.desired_state == "ACTIVE"
                && !desired_keys.contains(&(
                    binding.consumer_deployment_id.clone(),
                    binding.requirement_name.clone(),
                ))
        }) {
            let mut revoked = existing.clone();
            revoked.topology_revision_id = revision_id.to_string();
            revoked.credential_generation = generation;
            revoked.context_generation = generation;
            revoked.desired_state = "REVOKED".to_string();
            revoked.observed_state = "PENDING".to_string();
            revoked.health = "UNKNOWN".to_string();
            revoked.drift.clear();
            revoked.last_operation_id = operation_id.to_string();
            revoked.state = ApiBindingState::Pending;
            revoked.updated_at = now.to_string();
            desired.push(revoked);
        }
    }
    desired.sort_by(|left, right| {
        (&left.consumer_deployment_id, &left.requirement_name)
            .cmp(&(&right.consumer_deployment_id, &right.requirement_name))
    });
    desired
}

fn same_binding_route(left: &ApiBinding, right: &ApiBinding) -> bool {
    left.requirement_name == right.requirement_name
        && left.api_id == right.api_id
        && left.api_version == right.api_version
        && left.consumer_deployment_id == right.consumer_deployment_id
        && left.consumer_service_id == right.consumer_service_id
        && left.consumer_node_id == right.consumer_node_id
        && left.consumer_endpoint == right.consumer_endpoint
        && left.provider_deployment_id == right.provider_deployment_id
        && left.provider_service_id == right.provider_service_id
        && left.provider_node_id == right.provider_node_id
        && left.provider_endpoint == right.provider_endpoint
        && left.provider_path == right.provider_path
        && left.virtual_endpoint == right.virtual_endpoint
        && left.protocol == right.protocol
        && left.methods == right.methods
        && left.auth_mode == right.auth_mode
        && left.provider_auth_mode == right.provider_auth_mode
        && left.permission == right.permission
        && left.timeout_ms == right.timeout_ms
        && left.link_source_endpoint == right.link_source_endpoint
        && left.link_target_endpoint == right.link_target_endpoint
        && left.optional == right.optional
}

impl From<StorageError> for DurableError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Conflict(detail) => Self::Conflict(detail),
            StorageError::Invariant(detail) => Self::Invariant(detail),
            StorageError::Domain(detail) => Self::Domain(detail),
            other => Self::Storage(other.to_string()),
        }
    }
}

impl From<PostgresError> for DurableError {
    fn from(error: PostgresError) -> Self {
        match error {
            PostgresError::Conflict(detail) => Self::Conflict(detail),
            PostgresError::Invariant(detail) => Self::Invariant(detail),
            PostgresError::Domain(detail) => Self::Domain(detail),
            other => Self::Storage(other.to_string()),
        }
    }
}

#[cfg(test)]
mod binding_generation_tests {
    use super::*;

    fn binding(requirement: &str, provider: &str, generation: u64) -> ApiBinding {
        ApiBinding {
            binding_id: topology_binding_id("consumer-1", requirement),
            requirement_name: requirement.to_string(),
            api_id: format!("api.{requirement}"),
            api_version: "1.0.0".to_string(),
            consumer_deployment_id: "consumer-1".to_string(),
            consumer_service_id: "consumer".to_string(),
            consumer_node_id: "node-b".to_string(),
            consumer_endpoint: "10.0.0.2:9000:consumer".to_string(),
            provider_deployment_id: provider.to_string(),
            provider_service_id: "provider".to_string(),
            provider_node_id: "node-a".to_string(),
            provider_endpoint: "10.0.0.1:8080:provider".to_string(),
            provider_path: format!("/{requirement}"),
            virtual_endpoint: format!("/internal/apis/api.{requirement}"),
            protocol: "http".to_string(),
            methods: vec!["GET".to_string()],
            auth_mode: "workload".to_string(),
            provider_auth_mode: "workload".to_string(),
            permission: format!("permission.{requirement}"),
            timeout_ms: Some(5_000),
            topology_id: "primary".to_string(),
            topology_revision_id: "revision-1".to_string(),
            link_source_endpoint: "10.0.0.2:9000:consumer".to_string(),
            link_target_endpoint: "10.0.0.1:8080:provider".to_string(),
            credential_ref: String::new(),
            credential_generation: generation,
            context_generation: generation,
            desired_state: "ACTIVE".to_string(),
            observed_state: "ACTIVE".to_string(),
            health: "HEALTHY".to_string(),
            drift: Vec::new(),
            last_operation_id: "operation-1".to_string(),
            state: ApiBindingState::Active,
            optional: false,
            reason: String::new(),
            created_at: "unix-ms:1".to_string(),
            updated_at: "unix-ms:1".to_string(),
        }
    }

    #[test]
    fn one_route_change_bumps_every_sibling_generation() {
        let current = vec![
            binding("alpha", "provider-a", 7),
            binding("beta", "provider-a", 7),
        ];
        let mut desired = current.clone();
        desired[0].provider_deployment_id = "provider-b".to_string();
        desired[0].provider_endpoint = "10.0.0.3:8080:provider".to_string();
        desired[0].link_target_endpoint = desired[0].provider_endpoint.clone();
        for binding in &mut desired {
            binding.state = ApiBindingState::Resolved;
            binding.observed_state = "RESOLVED".to_string();
        }
        let staged =
            stage_binding_generations(desired, current, "revision-2", "operation-2", "unix-ms:2");
        assert_eq!(staged.len(), 2);
        assert!(staged.iter().all(|binding| {
            binding.credential_generation == 8 && binding.context_generation == 8
        }));
    }

    #[test]
    fn removed_requirement_is_staged_revoked_with_new_generation() {
        let current = vec![
            binding("alpha", "provider-a", 3),
            binding("beta", "provider-a", 3),
        ];
        let staged = stage_binding_generations(
            vec![current[0].clone()],
            current,
            "revision-2",
            "operation-2",
            "unix-ms:2",
        );
        let removed = staged
            .iter()
            .find(|binding| binding.requirement_name == "beta")
            .expect("removed requirement remains as a revocation record");
        assert_eq!(removed.desired_state, "REVOKED");
        assert_eq!(removed.state, ApiBindingState::Pending);
        assert_eq!(removed.credential_generation, 4);
        assert!(staged.iter().all(|binding| binding.context_generation == 4));
    }

    #[test]
    fn stale_external_probe_degrades_binding_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let store = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("external-binding.db")).unwrap(),
        );
        let evidence_at_ms = 120_000_i64;
        for (deployment_id, service_id, observed_at_ms) in [
            ("consumer-1", "consumer", evidence_at_ms),
            (
                "provider-a",
                "provider",
                evidence_at_ms - EXTERNAL_RUNTIME_PROBE_STALE_MS - 1,
            ),
        ] {
            let node_id = if deployment_id == "consumer-1" {
                "node-b"
            } else {
                "node-a"
            };
            let runtime: StoredRuntimeInstance = serde_json::from_value(json!({
                "node_id": node_id,
                "instance": {
                    "deployment_id": deployment_id,
                    "service_id": service_id,
                    "release_version": "1.0.0",
                    "container_id": "",
                    "artifact_digest": format!("sha256:{}", "a".repeat(64)),
                    "desired_state": "RUNNING",
                    "observed_state": "RUNNING",
                    "health": "HEALTHY"
                },
                "management_mode": "EXTERNAL",
                "endpoint": format!("https://{service_id}.example"),
                "external_probe_protocol": "https",
                "external_probe_health_path": "/healthz/ready",
                "last_observed_at_ms": observed_at_ms,
                "updated_at": format!("unix-ms:{observed_at_ms}")
            }))
            .unwrap();
            store.put_runtime_instance(&runtime).unwrap();
        }
        let projected = store
            .binding_with_current_runtime_evidence(binding("read", "provider-a", 1), evidence_at_ms)
            .unwrap();
        assert_eq!(projected.observed_state, "ERROR");
        assert_eq!(projected.health, "DEGRADED");
        assert!(projected.reason.contains("older than 60 seconds"));
    }

    #[test]
    fn changed_runtime_assignment_degrades_the_exact_binding() {
        let directory = tempfile::tempdir().unwrap();
        let store = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("assignment-binding.db")).unwrap(),
        );
        let evidence_at_ms = 120_000_i64;
        for (deployment_id, service_id, node_id) in [
            ("consumer-1", "consumer", "node-b"),
            ("provider-a", "provider", "node-reassigned"),
        ] {
            let runtime: StoredRuntimeInstance = serde_json::from_value(json!({
                "node_id": node_id,
                "instance": {
                    "deployment_id": deployment_id,
                    "service_id": service_id,
                    "release_version": "1.0.0",
                    "container_id": "",
                    "artifact_digest": format!("sha256:{}", "a".repeat(64)),
                    "desired_state": "RUNNING",
                    "observed_state": "RUNNING",
                    "health": "HEALTHY"
                },
                "management_mode": "EXTERNAL",
                "endpoint": format!("https://{service_id}.example"),
                "external_probe_protocol": "https",
                "external_probe_health_path": "/healthz/ready",
                "last_observed_at_ms": evidence_at_ms,
                "updated_at": format!("unix-ms:{evidence_at_ms}")
            }))
            .unwrap();
            store.put_runtime_instance(&runtime).unwrap();
        }
        let projected = store
            .binding_with_current_runtime_evidence(binding("read", "provider-a", 1), evidence_at_ms)
            .unwrap();
        assert_eq!(projected.observed_state, "ERROR");
        assert_eq!(projected.health, "DEGRADED");
        assert!(projected.reason.contains("assignment changed"));
        assert!(projected.reason.contains("node-reassigned"));
    }

    #[test]
    fn topology_binding_resolver_rejects_unattested_or_drifted_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let store = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("binding-runtime.db")).unwrap(),
        );
        let facts_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        for node_id in ["node-consumer", "node-provider"] {
            store
                .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                    node_id: node_id.to_string(),
                    observed_at_ms: facts_now,
                    received_at_ms: facts_now,
                    facts: json!({
                        "schema_version": 1,
                        "report_id": format!("report-{node_id}"),
                        "inventory_complete": true
                    }),
                })
                .unwrap();
        }
        let runtime = |deployment: &str, service: &str, attested: bool, drift: &str| {
            serde_json::from_value::<StoredRuntimeInstance>(json!({
                "node_id": format!("node-{service}"),
                "instance": {
                    "deployment_id": deployment,
                    "service_id": service,
                    "release_version": "1.0.0",
                    "container_id": format!("container-{service}"),
                    "artifact_digest": format!("registry.example/{service}@sha256:{}", "a".repeat(64)),
                    "runtime_attested": attested,
                    "desired_state": "RUNNING",
                    "observed_state": "RUNNING",
                    "health": "HEALTHY"
                },
                "management_mode": "MANAGED",
                "endpoint": "",
                "last_observed_at_ms": facts_now,
                "drift_reason": drift,
                "updated_at": "unix-ms:1"
            }))
            .unwrap()
        };
        store
            .put_runtime_instance(&runtime("deployment-consumer", "consumer", true, ""))
            .unwrap();
        store
            .put_runtime_instance(&runtime("deployment-provider", "provider", false, ""))
            .unwrap();
        let spec: TopologySpec = serde_json::from_value(json!({
            "api_version": "v1",
            "topology_id": "runtime-attestation",
            "root_endpoint": "10.0.0.1:9000:consumer",
            "authority": {
                "root_endpoint": "10.0.0.1:9000:consumer",
                "exposure_policy": "private"
            },
            "endpoints": [
                {
                    "endpoint": "10.0.0.1:9000:consumer",
                    "service_id": "consumer",
                    "protocol": "http",
                    "config": {"deployment_id": "deployment-consumer"}
                },
                {
                    "endpoint": "10.0.0.2:9000:provider",
                    "service_id": "provider",
                    "protocol": "http",
                    "config": {"deployment_id": "deployment-provider"}
                }
            ],
            "links": [{
                "source_endpoint": "10.0.0.1:9000:consumer",
                "target_endpoint": "10.0.0.2:9000:provider",
                "protocol": "http",
                "auth_mode": "workload",
                "scope": "api",
                "enabled": true,
                "policy": {},
                "api_bindings": [{
                    "requirement": "provider_api",
                    "api_id": "provider.api",
                    "provider_deployment_id": "deployment-provider",
                    "selection": "explicit"
                }]
            }]
        }))
        .unwrap();
        let unattested = store
            .resolve_topology_api_bindings(&spec, "revision-1", "operation-1")
            .unwrap_err()
            .to_string();
        assert!(unattested.contains("runtime-attested"), "{unattested}");

        store
            .put_runtime_instance(&runtime(
                "deployment-provider",
                "provider",
                true,
                "HostConfig drift",
            ))
            .unwrap();
        let drifted = store
            .resolve_topology_api_bindings(&spec, "revision-1", "operation-1")
            .unwrap_err()
            .to_string();
        assert!(drifted.contains("without drift"), "{drifted}");

        store
            .put_runtime_instance(&runtime("deployment-provider", "provider", true, ""))
            .unwrap();
        store
            .put_node_runtime_facts(&StoredNodeRuntimeFacts {
                node_id: "node-provider".to_string(),
                observed_at_ms: facts_now - 60_001,
                received_at_ms: facts_now - 60_001,
                facts: json!({
                    "schema_version": 1,
                    "report_id": "stale-provider",
                    "inventory_complete": true
                }),
            })
            .unwrap();
        let stale = store
            .resolve_topology_api_bindings(&spec, "revision-1", "operation-1")
            .unwrap_err()
            .to_string();
        assert!(stale.contains("older than 60 seconds"), "{stale}");
    }
}
